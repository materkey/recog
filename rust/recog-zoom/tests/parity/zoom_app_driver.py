"""The Python side of the differential test of rust/recog-zoom (tests/python_parity.rs).

Reads one JSON request from stdin and prints one JSON answer: what recog/zoom_app.py
does with the same input the Rust test gives the crate.

- "sessions": event lists run through ZoomAppSession and _StatusLog as _pump runs
  them (a helper line, a bridge message, a dropped link), with what each event sent,
  logged and left in the session;
- "vocabulary": load_vocabulary() of the given Zoom bundles, or of the installed one;
- "calls": the module's pure functions on given arguments;
- "plist": the bytes plistlib writes for the helper's Info.plist.
"""

import base64
import contextlib
import io
import json
import plistlib
import sys
from pathlib import Path


def _vocabulary(zoom_app, spec):
    if spec is None:
        return zoom_app.ENGLISH
    return zoom_app.Vocabulary(**{key: frozenset(words) for key, words in spec.items()})


def _state(session):
    return {
        "state": session.state,
        "unread": session.unread,
        "own_mic_unknown": session.own_mic_unknown,
        "joined": session.joined,
    }


def _session(zoom_app, scenario):
    session = zoom_app.ZoomAppSession(_vocabulary(zoom_app, scenario.get("vocabulary")))
    log = zoom_app._StatusLog()
    results = []
    for event in scenario["events"]:
        stderr = io.StringIO()
        sent = []
        error = False
        with contextlib.redirect_stderr(stderr):
            try:
                if "line" in event:
                    # The line branch of _pump.
                    frames = session.frames(json.loads(event["line"]), event["at"])
                    log.note(session.state)
                    log.note_unread(session.unread)
                    log.note_own_mic_unknown(session.own_mic_unknown)
                    sent = [
                        json.dumps(f, ensure_ascii=False, separators=(",", ":")) for f in frames
                    ]
                elif "message" in event:
                    # The message branch of _pump.
                    message = json.loads(event["message"])
                    if message.get("type") == "meeting_join_rejected":
                        print(f"[zoom-app] join rejected: {message.get('reason')}", file=sys.stderr)
                        session.on_rejected(event["at"])
                else:
                    session.on_disconnect()
            except Exception:  # noqa: BLE001 - any exception ends the run in _pump too
                error = True
        results.append(
            {"sent": sent, "log": stderr.getvalue().splitlines(), "error": error, **_state(session)}
        )
        if error:
            break  # the run that read it is over
    return results


def _sorted(vocabulary):
    return {
        "muted": sorted(vocabulary.muted),
        "unmuted": sorted(vocabulary.unmuted),
        "mic_open_item": sorted(vocabulary.mic_open_item),
        "mic_closed_item": sorted(vocabulary.mic_closed_item),
    }


def _call(zoom_app, call):
    function, args = call["fn"], call["args"]
    try:
        if function == "tile_name":
            return {"ok": zoom_app.tile_name(*args)}
        if function == "parse_tile":
            tile = zoom_app.parse_tile(args[0], args[1], _vocabulary(zoom_app, args[2]))
            return {"ok": [tile.name, tile.muted, tile.active]}
        if function == "account_name":
            return {"ok": zoom_app.account_name(*args)}
        if function == "menu_account_name":
            return {"ok": zoom_app.menu_account_name(*args)}
        if function == "own_mic_muted":
            return {"ok": zoom_app.own_mic_muted(args[0], _vocabulary(zoom_app, args[1]))}
    except Exception as error:  # noqa: BLE001 - the answer is which one was raised
        return {"error": type(error).__name__}
    raise ValueError(f"unknown function {function}")


def main():
    request = json.load(sys.stdin)
    sys.path.insert(0, request["repo_root"])
    from recog import zoom_app

    kind = request["kind"]
    if kind == "sessions":
        answer = [_session(zoom_app, scenario) for scenario in request["scenarios"]]
    elif kind == "vocabulary":
        apps = request.get("apps")
        loaded = (
            zoom_app.load_vocabulary()
            if apps is None
            else zoom_app.load_vocabulary(tuple(map(Path, apps)))
        )
        answer = _sorted(loaded)
    elif kind == "calls":
        answer = [_call(zoom_app, call) for call in request["calls"]]
    elif kind == "plist":
        answer = base64.b64encode(plistlib.dumps(zoom_app._helper_info())).decode()
    else:
        raise ValueError(f"unknown request {kind}")
    json.dump(answer, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
