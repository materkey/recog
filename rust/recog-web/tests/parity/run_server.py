# The Python side of rust/recog-web/tests/differential.rs: recog/web/server.py under
# uvicorn on the given port, with the bridge's debug file moved into the test's
# directory. Everything else comes from the environment the test sets (a temporary
# data directory, HOME, configuration file and stub CLIs on PATH).
#
#     python -I run_server.py <repo> <port> <debug file>
import pathlib
import sys

repo, port, debug_file = sys.argv[1], int(sys.argv[2]), sys.argv[3]
sys.path.insert(0, repo)

from recog.web import server  # noqa: E402

_real_path = pathlib.Path


def _path(*args):
    if args and args[0] == "/tmp/ktalk_debug.json":
        return _real_path(debug_file)
    return _real_path(*args)


server.Path = _path

import uvicorn  # noqa: E402

uvicorn.run(server.app, host="127.0.0.1", port=port, log_level="warning")
