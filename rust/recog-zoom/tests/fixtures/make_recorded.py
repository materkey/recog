"""Write recorded.json: helper snapshots rebuilt from recorded Accessibility dumps.

The dumps (.local/zoom-discovery, not in git) were taken from Zoom Workplace 7.1.9 in
test meetings with scripted participants: full dumps of the tree (`ax-*.txt`, one
`path :: attr=value | ...` line per element), a log of the tree's changes while two
participants took turns speaking (`ax-watch-fake-speakers.txt`) and two lines of an
early helper (`probe-out.jsonl`). This script reads the tab groups and the account
button out of them the way recog/native/macos/zoom_ax_probe.swift does and writes the
snapshot sequences the differential test feeds to both implementations.

Names are anonymized: only the test rig's participants (Web Tester, Speaker A/B,
Observer C) are kept; every other display name becomes a made-up one.

    python rust/recog-zoom/tests/fixtures/make_recorded.py .local/zoom-discovery
"""

import json
import re
import sys
from pathlib import Path

MEETING_WINDOW = "zm.meeting.window.main"
MAX_DEPTH = 6
RIG = re.compile(r"(Web Tester|Speaker [A-Z]|Observer [A-Z])")
STAND_INS = ["Иван Петров", "Анна Смирнова", "Olga Ivanova"]
LINE = re.compile(
    r"^(?:(?P<time>\d\d:\d\d:\d\d\.\d+) (?P<op>[~+-]) )?(?P<path>window\[\d+\][^ ]*)(?: :: (?P<attrs>.*))?$"
)


def parse_attrs(text):
    attrs = {}
    for part in (text or "").split(" | "):
        key, _, value = part.partition("=")
        attrs[key.strip()] = value.strip()
    return attrs


def parent(path):
    return path.rsplit("/", 1)[0] if "/" in path else None


def role(element):
    return element.get("role", "")


class Tree:
    """Elements by path, in the order the dump listed them."""

    def __init__(self):
        self.elements = {}

    def set(self, path, attrs):
        self.elements[path] = attrs

    def remove(self, path):
        for known in [p for p in self.elements if p == path or p.startswith(path + "/")]:
            del self.elements[known]

    def children(self, path):
        return [p for p in self.elements if parent(p) == path]

    def tile(self, path):
        element = self.elements[path]
        if role(element) != "AXTabGroup" or not element.get("AXDescription"):
            return None
        parts = []
        for child in self.children(path):
            attrs = self.elements[child]
            part = {"role": role(attrs)}
            if attrs.get("AXDescription"):
                part["description"] = attrs["AXDescription"]
            if attrs.get("AXValue"):
                part["value"] = attrs["AXValue"]
            parts.append(part)
        return {"description": element["AXDescription"], "children": parts}

    def scan(self, path, depth, tiles):
        found = self.tile(path)
        if found:
            tiles.append(found)
        if depth <= 0 or role(self.elements[path]) == "AXScrollArea":
            return
        for child in self.children(path):
            self.scan(child, depth - 1, tiles)

    def snapshot(self):
        windows = [p for p in self.elements if "/" not in p]
        tiles = []
        view = None
        meeting = [w for w in windows if self.elements[w].get("AXIdentifier") == MEETING_WINDOW]
        if meeting:
            view = "window"
            self.scan(meeting[0], MAX_DEPTH, tiles)
        else:
            for window in windows:
                if self.elements[window].get("subrole") == "AXSystemDialog":
                    self.scan(window, MAX_DEPTH, tiles)
            if tiles:
                view = "minimal"
        out = {"state": "idle"}
        if view:
            out = {"state": "meeting", "view": view, "tiles": tiles}
        for window in windows:
            if window in meeting:
                continue
            for child in self.children(window):
                description = self.elements[child].get("AXDescription", "")
                is_button = role(self.elements[child]) == "AXButton"
                if is_button and description.startswith("Zoom, ") and description.count(",") >= 2:
                    out["account"] = description
                    return out
        return out


def read_dump(path):
    tree = Tree()
    for line in path.read_text(encoding="utf-8").splitlines():
        match = LINE.match(line)
        if match:
            tree.set(match["path"], parse_attrs(match["attrs"]))
    return tree


def read_watch(path, base):
    """Snapshots after each burst of changes, with the seconds since the first."""
    tree = base
    snapshots = []
    start = last = None
    for line in path.read_text(encoding="utf-8").splitlines():
        match = LINE.match(line)
        if not match or not match["time"]:
            continue
        hours, minutes, seconds = match["time"].split(":")
        at = int(hours) * 3600 + int(minutes) * 60 + float(seconds)
        start = at if start is None else start
        if last is not None and at - last > 0.05:
            snapshots.append((round(last - start, 3), tree.snapshot()))
        last = at
        if match["op"] == "-":
            tree.remove(match["path"])
        else:
            tree.set(match["path"], parse_attrs(match["attrs"]))
    if last is not None:
        snapshots.append((round(last - start, 3), tree.snapshot()))
    return snapshots


class Anonymizer:
    def __init__(self):
        self.names = {}

    def name(self, name):
        if RIG.fullmatch(name):
            return name
        return self.names.setdefault(name, STAND_INS[len(self.names) % len(STAND_INS)])

    def learn(self, snapshot):
        for tile in snapshot.get("tiles", []):
            for child in tile.get("children", []):
                if child.get("role") == "AXButton" and child.get("description"):
                    self.name(child["description"])
            if tile.get("name"):
                self.name(tile["name"])
        account = snapshot.get("account", "")
        if account.count(",") >= 2:
            self.name(account.split(",")[1].strip())

    def apply(self, value):
        if isinstance(value, dict):
            return {key: self.apply(item) for key, item in value.items()}
        if isinstance(value, list):
            return [self.apply(item) for item in value]
        if isinstance(value, str):
            for real, stand_in in self.names.items():
                value = value.replace(real, stand_in)
        return value


def scenario(name, timed, start=1_000.0):
    return {
        "name": name,
        "events": [{"line": json.dumps(s, ensure_ascii=False), "at": start + t} for t, s in timed],
    }


def main(source):
    source = Path(source)
    dumps = {
        name: read_dump(source / f"ax-{name}.txt")
        for name in ("waitingroom", "meeting-1p", "meeting-2p", "meeting-5p", "minimal")
    }
    growth = [
        (0.0, dumps["waitingroom"].snapshot()),
        (2.0, dumps["meeting-1p"].snapshot()),
        (4.0, dumps["meeting-1p"].snapshot()),
        (6.0, dumps["meeting-2p"].snapshot()),
        (8.0, dumps["meeting-5p"].snapshot()),
        (10.0, dumps["meeting-5p"].snapshot()),
        (12.0, dumps["minimal"].snapshot()),
        (14.0, dumps["minimal"].snapshot()),
        (16.0, {"state": "idle"}),
        (22.1, {"state": "idle"}),
    ]
    # The change log starts from a tree it does not list: only the meeting window is
    # taken from a full dump, the tiles come from the changes.
    window = Tree()
    window.set("window[0]", dumps["meeting-5p"].elements["window[0]"])
    speakers = read_watch(source / "ax-watch-fake-speakers.txt", window)
    probe = [
        (2.0 * i, json.loads(line))
        for i, line in enumerate((source / "probe-out.jsonl").read_text().splitlines())
    ]
    anonymizer = Anonymizer()
    for _, snapshot in growth + speakers + probe:
        anonymizer.learn(snapshot)
    scenarios = [
        scenario("recorded meeting from the waiting room to minimal view and out", growth),
        scenario("recorded speakers taking turns, from the change log of a meeting", speakers),
        scenario(
            "recorded lines of an early helper that named the tile itself",
            [(-2.0, {"state": "idle"}), *probe],
        ),
    ]
    out = Path(__file__).with_name("recorded.json")
    out.write_text(
        json.dumps(anonymizer.apply(scenarios), ensure_ascii=False, indent=1) + "\n",
        encoding="utf-8",
    )
    print(f"wrote {out}: {sum(len(s['events']) for s in scenarios)} snapshots", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else ".local/zoom-discovery")
