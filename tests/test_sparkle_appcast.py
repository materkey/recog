"""Release checks that bind a signed update to its exact public archive."""

import base64
import importlib
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
appcast = importlib.import_module("sparkle_appcast")
SIGNATURE = base64.b64encode(bytes(64)).decode()
TAG = "v1.2.3"
URL = f"{appcast.PROJECT}/releases/download/{TAG}/{appcast.IMAGE}"


def feed():
    return f'''<rss xmlns:sparkle="{appcast.NS[1:-1]}"><channel><item>
      <sparkle:version>1.2.3</sparkle:version><sparkle:shortVersionString>1.2.3</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
      <enclosure url="{URL}" length="123" sparkle:edSignature="{SIGNATURE}"/>
      </item></channel></rss>'''.encode()


def test_stable_release_when_verified_then_enclosure_names_its_exact_version():
    assert appcast.image_signature(feed(), TAG, 123) == SIGNATURE


@pytest.mark.parametrize(
    ("original", "replacement"),
    [
        (URL, URL.replace("download/v1.2.3", "latest/download")),
        ("1.2.3</", "1.2.4</"),
        ('length="123"', 'length="124"'),
        (SIGNATURE, "AA=="),
    ],
)
def test_inconsistent_enclosure_when_checked_then_publication_is_refused(original, replacement):
    with pytest.raises(ValueError):
        appcast.image_signature(feed().replace(original.encode(), replacement.encode()), TAG, 123)


def test_signed_feed_when_split_then_only_the_exact_signed_bytes_are_returned():
    body = feed() + b"\n"
    footer = (
        f"<!-- sparkle-signatures:\nedSignature: {SIGNATURE}\nlength: {len(body)}\n-->\n".encode()
    )
    assert appcast.signed_feed_data(body + footer) == (body, SIGNATURE)
    for broken in (body, body[1:] + footer, body + footer + b"<item/>"):
        with pytest.raises(ValueError):
            appcast.signed_feed_data(broken)
