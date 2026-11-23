"""The Linux repository indexes bind to the exact packages of their release."""

import gzip
import hashlib
import importlib
import io
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
repo = importlib.import_module("linux_repo")
TAG = "v1.2.3"


def tar_gz(files):
    out = io.BytesIO()
    with (
        gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as packed,
        tarfile.open(fileobj=packed, mode="w") as tar,
    ):
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()


def deb(version):
    control = (
        f"Package: recog\nVersion: {version}\nArchitecture: amd64\nDescription: test\n".encode()
    )
    members = [
        ("debian-binary", b"2.0\n"),
        ("control.tar.gz", tar_gz({"./control": control})),
        ("data.tar.gz", tar_gz({"./usr/bin/recog": b"x"})),
    ]
    out = b"!<arch>\n"
    for name, data in members:
        out += f"{name:<16}{0:<12}{0:<6}{0:<6}{644:<8}{len(data):<10}`\n".encode() + data
        out += b"\n" if len(data) % 2 else b""
    return out


def arch_package(directory, version):
    pkginfo = (
        f"# test\npkgname = recog\npkgbase = recog\npkgver = {version}\npkgdesc = test\n"
        "builddate = 1700000000\nsize = 10\narch = x86_64\nlicense = MIT\n"
        "depend = glibc>=2.39\ndepend = ffmpeg\n"
    ).encode()
    plain = directory / "pkg.tar"
    with tarfile.open(plain, "w") as tar:
        info = tarfile.TarInfo(".PKGINFO")
        info.size = len(pkginfo)
        tar.addfile(info, io.BytesIO(pkginfo))
    path = directory / f"recog-{version}-x86_64.pkg.tar.zst"
    subprocess.run(["zstd", "-q", "--rm", plain, "-o", path], check=True)
    return path


@pytest.fixture
def dist(tmp_path):
    if not shutil.which("zstd"):
        pytest.skip("zstd is not installed")
    (tmp_path / "recog_1.2.3-1_amd64.deb").write_bytes(deb("1.2.3-1"))
    arch_package(tmp_path, "1.2.3-1")
    (tmp_path / "recog-v1.2.3-linux-x86_64.tar.gz").write_bytes(b"archive")
    repo.index(tmp_path)
    return tmp_path


@pytest.fixture
def gnupg_home():
    # The agent's socket path must fit sun_path, too short for pytest's directories on macOS.
    homes = []

    def make():
        homes.append(tempfile.mkdtemp(prefix="gpg", dir="/tmp"))
        return homes[-1]

    yield make
    for home in homes:
        subprocess.run(["gpgconf", "--homedir", home, "--kill", "gpg-agent"], check=False)
        shutil.rmtree(home, ignore_errors=True)


def published(dist):
    return {
        p.name: (p.stat().st_size, hashlib.sha256(p.read_bytes()).hexdigest())
        for p in dist.iterdir()
    }


def indexes(dist):
    return {name: (dist / name).read_bytes() for name in repo.INDEXES}


def test_release_packages_when_indexed_then_both_indexes_name_their_exact_files(dist):
    files = published(dist)
    repo.check(indexes(dist), files, TAG)
    packages = (dist / "Packages").read_text()
    assert "Filename: ./recog_1.2.3-1_amd64.deb\n" in packages
    assert f"SHA256: {files['recog_1.2.3-1_amd64.deb'][1]}\n" in packages
    entry = repo.db_entry((dist / "recog.db").read_bytes())
    assert entry["FILENAME"] == "recog-1.2.3-1-x86_64.pkg.tar.zst"
    assert entry["DEPENDS"] == "glibc>=2.39\nffmpeg"
    assert entry["SHA256SUM"] == files["recog-1.2.3-1-x86_64.pkg.tar.zst"][1]


@pytest.mark.parametrize("changed", ["recog_1.2.3-1_amd64.deb", "recog-1.2.3-1-x86_64.pkg.tar.zst"])
def test_published_package_when_it_differs_from_the_index_then_signing_is_refused(dist, changed):
    files = published(dist)
    size, _ = files[changed]
    files[changed] = (size, "0" * 64)
    with pytest.raises(ValueError, match="does not match the published"):
        repo.check(indexes(dist), files, TAG)


def test_indexes_when_checked_for_another_tag_then_signing_is_refused(dist):
    with pytest.raises(ValueError, match="another version"):
        repo.check(indexes(dist), published(dist), "v1.2.4")


def test_packages_when_changed_after_release_was_written_then_the_chain_is_broken(dist):
    changed = dict(indexes(dist), Packages=(dist / "Packages").read_bytes() + b"Extra: field\n")
    with pytest.raises(ValueError, match="Release does not match Packages"):
        repo.check(changed, published(dist), TAG)


def test_signatures_when_made_with_another_key_then_they_do_not_verify(dist, gnupg_home):
    if not (shutil.which("gpg") and shutil.which("gpgv")):
        pytest.skip("GnuPG is not installed")
    keys = {}
    for name in ("ours", "other"):
        home = gnupg_home()
        repo.gpg(
            home,
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            name,
            "ed25519",
            "sign",
        )
        out = dist / name
        out.mkdir()
        repo.gpg(home, "--clearsign", "--output", out / "InRelease", dist / "Release")
        repo.gpg(home, "--detach-sign", "--output", out / "recog.db.sig", dist / "recog.db")
        keys[name] = (repo.gpg(home, "--armor", "--export"), out)
    key, ours = keys["ours"]
    signed = [(ours / "InRelease").read_bytes(), (ours / "recog.db.sig").read_bytes()]
    repo.check_signatures(indexes(dist), *signed, key)
    other = keys["other"][1]
    with pytest.raises(ValueError, match="does not verify"):
        repo.check_signatures(indexes(dist), (other / "InRelease").read_bytes(), signed[1], key)
    with pytest.raises(ValueError, match="does not verify"):
        repo.check_signatures(indexes(dist), signed[0], (other / "recog.db.sig").read_bytes(), key)
