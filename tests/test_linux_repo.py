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

sys.path.insert(0, str(Path(__file__).absolute().parents[1] / "scripts"))
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


CONTROL = "Package: recog\nVersion: {}\nArchitecture: amd64\nDescription: test\n test package\n"


def deb(version, extra=(), control=CONTROL):
    members = [
        ("debian-binary", b"2.0\n"),
        *extra,
        ("control.tar.gz", tar_gz({"./control": control.format(version).encode()})),
        ("data.tar.gz", tar_gz({"./usr/bin/recog": b"x"})),
    ]
    return ar(members)


def ar(members):
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


def sign_into(home, dist, out):
    """A new key signs dist's indexes into out; returns its public key."""
    if not (shutil.which("gpg") and shutil.which("gpgv")):
        pytest.skip("GnuPG is not installed")
    no_passphrase = ("--pinentry-mode", "loopback", "--passphrase", "")
    repo.gpg(home, *no_passphrase, "--quick-gen-key", "recog test", "ed25519", "sign")
    repo.gpg(home, "--clearsign", "--output", out / "InRelease", dist / "Release")
    repo.gpg(home, "--detach-sign", "--output", out / "recog.db.sig", dist / "recog.db")
    return repo.gpg(home, "--armor", "--export")


def test_signatures_when_made_with_another_key_then_they_do_not_verify(dist, gnupg_home):
    keys = {}
    for name in ("ours", "other"):
        out = dist / name
        out.mkdir()
        keys[name] = (sign_into(gnupg_home(), dist, out), out)
    key, ours = keys["ours"]
    signed = [(ours / "InRelease").read_bytes(), (ours / "recog.db.sig").read_bytes()]
    repo.check_signatures(indexes(dist), *signed, key)
    other = keys["other"][1]
    with pytest.raises(ValueError, match="does not verify"):
        repo.check_signatures(indexes(dist), (other / "InRelease").read_bytes(), signed[1], key)
    with pytest.raises(ValueError, match="does not verify"):
        repo.check_signatures(indexes(dist), signed[0], (other / "recog.db.sig").read_bytes(), key)


@pytest.fixture
def published_repository(dist, gnupg_home, monkeypatch, tmp_path_factory):
    key = tmp_path_factory.mktemp("key") / "recog.asc"
    key.write_bytes(sign_into(gnupg_home(), dist, dist))
    monkeypatch.setattr(repo, "PUBLIC_KEY", key)
    return dist


def run_verify(monkeypatch, dist):
    monkeypatch.setattr(
        sys, "argv", ["linux_repo.py", "verify", "--base", dist.as_uri(), "--tag", TAG]
    )
    repo.main()


def test_published_repository_when_verified_then_apt_and_pacman_get_what_was_signed(
    published_repository, monkeypatch, capsys
):
    run_verify(monkeypatch, published_repository)
    assert "signed indexes of recog 1.2.3-1 match their packages" in capsys.readouterr().out


def test_package_when_replaced_after_signing_then_the_repository_fails_verification(
    published_repository, monkeypatch
):
    package = published_repository / "recog-1.2.3-1-x86_64.pkg.tar.zst"
    package.write_bytes(package.read_bytes() + b"tampered")
    with pytest.raises(ValueError, match="does not match the published"):
        run_verify(monkeypatch, published_repository)


def test_release_when_changed_after_signing_then_the_repository_fails_verification(
    published_repository, monkeypatch
):
    release = published_repository / "Release"
    release.write_bytes(release.read_bytes().replace(b"Label: Recog", b"Label: Other"))
    with pytest.raises(ValueError, match="InRelease signs a different Release"):
        run_verify(monkeypatch, published_repository)


def test_signed_deb_when_an_odd_sized_member_precedes_control_then_its_fields_are_read():
    # dpkg-sig puts _gpgbuilder between debian-binary and the control archive.
    control = repo.deb_control(deb("1.2.3-1", extra=[("_gpgbuilder", b"odd")]))
    assert repo.stanza(control)["Version"] == "1.2.3-1"


@pytest.mark.parametrize(
    ("package", "message"),
    [
        (b"not an archive", "Not a Debian package"),
        (ar([("debian-binary", b"2.0\n")]), "no control file"),
    ],
)
def test_file_when_it_is_not_a_complete_deb_then_indexing_is_refused(package, message):
    with pytest.raises(ValueError, match=message):
        repo.deb_control(package)


def test_description_when_a_continuation_line_looks_like_a_field_then_it_changes_nothing():
    control = CONTROL.format("1.2.3-1") + " Size: 1\n SHA256: forged\n"
    fields = repo.stanza(control)
    assert "Size" not in fields
    assert "SHA256" not in fields
    assert fields["Version"] == "1.2.3-1"


def test_release_directory_when_it_holds_two_debian_packages_then_indexing_is_refused(dist):
    (dist / "recog_1.2.4-1_amd64.deb").write_bytes(deb("1.2.4-1"))
    with pytest.raises(ValueError, match="one Debian and one Arch package"):
        repo.index(dist)


def with_packages(dist, text):
    """The indexes with another Packages and the Release that lists it."""
    return dict(indexes(dist), Packages=text.encode(), Release=repo.apt_release(text.encode(), 0))


def test_packages_when_it_names_a_file_outside_the_release_then_signing_is_refused(dist):
    text = (dist / "Packages").read_text().replace("Filename: ./", "Filename: ../")
    with pytest.raises(ValueError, match="unexpected package files"):
        repo.check(with_packages(dist, text), published(dist), TAG)


def test_packages_when_it_lists_two_packages_then_signing_is_refused(dist):
    text = (dist / "Packages").read_text()
    with pytest.raises(ValueError, match="one recog package"):
        repo.check(with_packages(dist, text + "\n" + text), published(dist), TAG)


def merged(*databases):
    out = io.BytesIO()
    with (
        gzip.GzipFile(fileobj=out, mode="wb") as packed,
        tarfile.open(fileobj=packed, mode="w") as tar,
    ):
        for data in databases:
            with tarfile.open(fileobj=io.BytesIO(data)) as source:
                for member in source.getmembers():
                    tar.addfile(member, source.extractfile(member) if member.isfile() else None)
    return out.getvalue()


@pytest.fixture
def other_package(tmp_path_factory):
    return arch_package(tmp_path_factory.mktemp("other"), "1.2.4-1")


def test_pacman_database_when_it_lists_two_packages_then_signing_is_refused(dist, other_package):
    two = merged((dist / "recog.db").read_bytes(), repo.pacman_db(other_package))
    with pytest.raises(ValueError, match="one package"):
        repo.db_entry(two)


def test_indexes_when_apt_and_pacman_disagree_on_the_version_then_signing_is_refused(
    dist, other_package
):
    digest = hashlib.sha256(other_package.read_bytes()).hexdigest()
    files = dict(published(dist), **{other_package.name: (other_package.stat().st_size, digest)})
    with pytest.raises(ValueError, match="another version"):
        repo.check(dict(indexes(dist), **{"recog.db": repo.pacman_db(other_package)}), files, TAG)


def test_published_repository_when_verified_for_another_release_then_it_fails(
    published_repository, monkeypatch
):
    monkeypatch.setattr(
        sys,
        "argv",
        ["linux_repo.py", "verify", "--base", published_repository.as_uri(), "--tag", "v1.2.4"],
    )
    with pytest.raises(ValueError, match="another version"):
        repo.main()


def test_published_repository_when_verified_without_a_tag_then_its_own_version_is_checked(
    published_repository, monkeypatch, capsys
):
    monkeypatch.setattr(
        sys, "argv", ["linux_repo.py", "verify", "--base", published_repository.as_uri()]
    )
    repo.main()
    assert "recog 1.2.3-1" in capsys.readouterr().out


def test_command_line_when_no_action_is_given_then_usage_is_printed(monkeypatch):
    monkeypatch.setattr(sys, "argv", ["linux_repo.py"])
    with pytest.raises(SystemExit) as stopped:
        repo.main()
    assert stopped.value.code == 2
