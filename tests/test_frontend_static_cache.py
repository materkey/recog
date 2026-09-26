"""Static web UI files are revalidated so an update never mixes old and new code."""

import pytest
from starlette.testclient import TestClient

from recog.web import server


@pytest.mark.parametrize("path", ["/", "/static/app.js", "/static/style.css"])
def test_web_ui_file_when_served_then_browser_must_revalidate(path):
    client = TestClient(server.app)  # no lifespan: static routes need no services
    response = client.get(path)
    assert response.status_code == 200
    assert response.headers["cache-control"] == "no-cache"


def test_unchanged_script_when_revalidated_then_not_modified():
    client = TestClient(server.app)
    etag = client.get("/static/app.js").headers["etag"]
    response = client.get("/static/app.js", headers={"If-None-Match": etag})
    assert response.status_code == 304
    assert response.headers["cache-control"] == "no-cache"
