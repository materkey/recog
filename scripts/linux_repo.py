"""Index a release's Linux packages as apt and pacman repositories, sign the
indexes on the trusted Mac, and verify what users download.

The repositories are the release assets themselves: apt and pacman read
releases/latest/download/, so every release carries indexes of its own
packages, and the latest release is the repository (docs/linux-packages.md).
"""

import argparse
import base64
import gzip
import hashlib
import io
import os
import re
import subprocess
import tarfile
import tempfile
import time
import urllib.request
from pathlib import Path

PROJECT = "materkey/recog"
LATEST = f"https://github.com/{PROJECT}/releases/latest/download/"
PACKAGES, RELEASE, IN_RELEASE = "Packages", "Release", "InRelease"
DB, DB_SIG = "recog.db", "recog.db.sig"
INDEXES = (PACKAGES, RELEASE, DB)
# The draft's assets as GitHub reports them (name, size, SHA-256), handed with
# the indexes from the release workstation to the signing Mac.
ASSETS = "assets.tsv"
ACCOUNT = "com.recog.linux-packages"
SIGNER = "Recog packages <materkey@users.noreply.github.com>"
PUBLIC_KEY = Path(__file__).resolve().parents[1] / "deploy/landing/recog.asc"
DEB = re.compile(r"recog_[^/_]+_amd64\.deb")
PKG = re.compile(r"recog-[^/]+-x86_64\.pkg\.tar\.zst")
# Field order of repo-add's desc entries, with their .PKGINFO keys.
DESC = (
    ("FILENAME", None),
    ("NAME", "pkgname"),
    ("BASE", "pkgbase"),
    ("VERSION", "pkgver"),
    ("DESC", "pkgdesc"),
    ("GROUPS", "group"),
    ("CSIZE", None),
    ("ISIZE", "size"),
    ("SHA256SUM", None),
    ("URL", "url"),
    ("LICENSE", "license"),
    ("ARCH", "arch"),
    ("BUILDDATE", "builddate"),
    ("PACKAGER", "packager"),
    ("REPLACES", "replaces"),
    ("CONFLICTS", "conflict"),
    ("PROVIDES", "provides"),
    ("DEPENDS", "depend"),
    ("OPTDEPENDS", "optdepend"),
    ("MAKEDEPENDS", "makedepend"),
    ("CHECKDEPENDS", "checkdepend"),
)


def stable_tag(text):
    if not re.fullmatch(r"v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", text):
        raise argparse.ArgumentTypeError("expected a stable release tag such as v0.3.0")
    return text


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def deb_control(data):
    """The control stanza of a .deb, an ar archive holding control.tar.*."""
    if not data.startswith(b"!<arch>\n"):
        raise ValueError("Not a Debian package")
    position = 8
    while position + 60 <= len(data):
        header = data[position : position + 60]
        name = header[:16].decode().strip().rstrip("/")
        size = int(header[48:58])
        start = position + 60
        if name.startswith("control.tar"):
            with tarfile.open(fileobj=io.BytesIO(data[start : start + size])) as tar:
                for member in tar.getmembers():
                    if member.name.removeprefix("./") == "control":
                        return tar.extractfile(member).read().decode()
        position = start + size + size % 2
    raise ValueError("The Debian package has no control file")


def stanza(text):
    """The fields of a control stanza by name. A continuation line of the
    Description keeps its leading space in the name, so it matches no field."""
    fields = {}
    for line in text.splitlines():
        key, separator, value = line.partition(":")
        if separator:
            fields[key] = value.strip()
    return fields


def pkginfo(path):
    # tar recognises the zstd compression by itself (bsdtar and GNU tar).
    text = subprocess.run(["tar", "-xOf", path, ".PKGINFO"], check=True, capture_output=True).stdout
    info = {}
    for line in text.decode().splitlines():
        key, separator, value = line.partition(" = ")
        if separator:
            info.setdefault(key, []).append(value)
    return info


def pacman_db(path):
    """A sync database with one package, as repo-add writes it."""
    data = Path(path).read_bytes()
    info = pkginfo(path)
    known = {"FILENAME": [Path(path).name], "CSIZE": [str(len(data))], "SHA256SUM": [sha256(data)]}
    desc = ""
    for name, key in DESC:
        values = known.get(name) or info.get(key)
        if values:
            desc += f"%{name}%\n" + "\n".join(values) + "\n\n"
    entry = f"{info['pkgname'][0]}-{info['pkgver'][0]}"
    when = int(info["builddate"][0])
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=when) as packed:
        with tarfile.open(fileobj=packed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            folder = tarfile.TarInfo(entry)
            folder.type, folder.mode, folder.mtime = tarfile.DIRTYPE, 0o755, when
            tar.addfile(folder)
            content = desc.encode()
            member = tarfile.TarInfo(f"{entry}/desc")
            member.size, member.mode, member.mtime = len(content), 0o644, when
            tar.addfile(member, io.BytesIO(content))
    return out.getvalue()


def db_entry(data):
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        files = [m for m in tar.getmembers() if m.isfile()]
        if len(files) != 1 or not files[0].name.endswith("/desc"):
            raise ValueError("Expected one package in the pacman database")
        text = tar.extractfile(files[0]).read().decode()
    fields = {}
    for block in text.strip().split("\n\n"):
        name, _, values = block.partition("\n")
        fields[name.strip("%")] = values
    return fields


def apt_release(packages, when):
    date = time.strftime("%a, %d %b %Y %H:%M:%S UTC", time.gmtime(when))
    return (
        "Origin: Recog\nLabel: Recog\nArchitectures: amd64\n"
        f"Date: {date}\nSHA256:\n {sha256(packages)} {len(packages)} {PACKAGES}\n"
    ).encode()


def index(dist):
    """Packages, Release and recog.db for the one .deb and .pkg.tar.zst in dist."""
    debs = [p for p in dist.iterdir() if DEB.fullmatch(p.name)]
    pkgs = [p for p in dist.iterdir() if PKG.fullmatch(p.name)]
    if len(debs) != 1 or len(pkgs) != 1:
        raise ValueError("Expected one Debian and one Arch package")
    deb = debs[0].read_bytes()
    control = deb_control(deb).rstrip("\n")
    packages = (
        f"{control}\nFilename: ./{debs[0].name}\nSize: {len(deb)}\nSHA256: {sha256(deb)}\n".encode()
    )
    when = int(os.environ.get("SOURCE_DATE_EPOCH") or time.time())
    (dist / PACKAGES).write_bytes(packages)
    (dist / RELEASE).write_bytes(apt_release(packages, when))
    (dist / DB).write_bytes(pacman_db(pkgs[0]))


def referenced(indexes):
    """The package files the indexes point to: the .deb, then the Arch package."""
    deb = stanza(indexes[PACKAGES].decode()).get("Filename", "").removeprefix("./")
    pkg = db_entry(indexes[DB]).get("FILENAME", "")
    if not DEB.fullmatch(deb) or not PKG.fullmatch(pkg):
        raise ValueError("The indexes name unexpected package files")
    return deb, pkg


def published_as(files, name, size, digest):
    """Whether the release has name with this size and SHA-256, given as index text."""
    return tuple(map(str, files.get(name, ()))) == (size, digest)


def check(indexes, files, tag):
    """Bind the indexes to the packages: files maps a name to (size, sha256)."""
    deb, pkg = referenced(indexes)
    packages = indexes[PACKAGES]
    listed = re.findall(r"^ ([0-9a-f]{64}) +(\d+) +(\S+)$", indexes[RELEASE].decode(), re.M)
    if listed != [(sha256(packages), str(len(packages)), PACKAGES)]:
        raise ValueError("Release does not match Packages")
    fields = stanza(packages.decode())
    if packages.decode().strip().count("\n\n") or fields.get("Package") != "recog":
        raise ValueError("Expected one recog package in Packages")
    if not published_as(files, deb, fields.get("Size"), fields.get("SHA256")):
        raise ValueError(f"Packages does not match the published {deb}")
    entry = db_entry(indexes[DB])
    if entry.get("NAME") != "recog" or not published_as(
        files, pkg, entry.get("CSIZE"), entry.get("SHA256SUM")
    ):
        raise ValueError(f"{DB} does not match the published {pkg}")
    version = f"{tag[1:]}-1"
    if fields.get("Version") != version or entry.get("VERSION") != version:
        raise ValueError(f"The indexes describe another version than {tag}")


def gpg(home, *args, data=None):
    return subprocess.run(
        ["gpg", "--homedir", home, "--batch", "--yes", *args],
        input=data,
        check=True,
        capture_output=True,
    ).stdout


def verified(signed, key, detached=None):
    """The data a signature covers, once it verifies against the published key."""
    with tempfile.TemporaryDirectory() as home:
        keyring = Path(home) / "recog.gpg"
        keyring.write_bytes(gpg(home, "--dearmor", data=key))
        with tempfile.TemporaryDirectory() as work:
            sig = Path(work) / "sig"
            sig.write_bytes(signed)
            args = ["gpgv", "--homedir", home, "--keyring", keyring]
            if detached is None:
                args += ["--output", "-", sig]
            else:
                data = Path(work) / "data"
                data.write_bytes(detached)
                args += [sig, data]
            run = subprocess.run(args, capture_output=True)
            if run.returncode:
                raise ValueError("Signature does not verify against the published key")
            return run.stdout if detached is None else detached


def check_signatures(indexes, in_release, db_sig, key):
    if verified(in_release, key).rstrip(b"\n") != indexes[RELEASE].rstrip(b"\n"):
        raise ValueError("InRelease signs a different Release")
    verified(db_sig, key, indexes[DB])


def keychain_key():
    found = subprocess.run(
        ["security", "find-generic-password", "-s", ACCOUNT, "-a", "signing", "-w"],
        capture_output=True,
        text=True,
    )
    if found.returncode:
        raise SystemExit(f"No {ACCOUNT} item in the login Keychain (docs/linux-packages.md)")
    return base64.b64decode(found.stdout.strip())


def new_key(_args):
    if (
        subprocess.run(
            ["security", "find-generic-password", "-s", ACCOUNT], capture_output=True
        ).returncode
        == 0
    ):
        raise SystemExit(f"The {ACCOUNT} Keychain item already exists; keep using that key")
    with tempfile.TemporaryDirectory() as home:
        gpg(
            home,
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            SIGNER,
            "ed25519",
            "sign",
            "never",
        )
        listing = gpg(home, "--with-colons", "--list-secret-keys").decode()
        fingerprint = re.search(r"^fpr:+([0-9A-F]{40}):", listing, re.M)[1]
        secret = base64.b64encode(
            gpg(home, "--armor", "--export-secret-keys", fingerprint)
        ).decode()
        public = gpg(home, "--armor", "--export", fingerprint)
        subprocess.run(["gpgconf", "--homedir", home, "--kill", "gpg-agent"], check=False)
    # The command goes through stdin so the key never appears in a process list.
    command = f'add-generic-password -s {ACCOUNT} -a signing -l "Recog Linux package signing key" -w {secret}\n'
    subprocess.run(["security", "-i"], input=command, text=True, check=True, capture_output=True)
    PUBLIC_KEY.write_bytes(public)
    print(f"Signing key {fingerprint} is in the login Keychain; its public key is {PUBLIC_KEY}")


def release_files(tag):
    shown = subprocess.run(
        [
            "gh",
            "release",
            "view",
            tag,
            "--repo",
            PROJECT,
            "--json",
            "assets",
            "--jq",
            ".assets[]|[.name,.size,.digest]|@tsv",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    files = {}
    for line in shown.splitlines():
        name, size, digest = line.split("\t")
        files[name] = (int(size), digest.removeprefix("sha256:"))
    return files


def unchanged(files, data):
    """Each file is byte for byte the release asset of its name."""
    for name, content in data.items():
        if files.get(name) != (len(content), sha256(content)):
            raise SystemExit(f"{name} differs from the release asset")


def download(tag, names, out):
    """Release assets, each checked against the digest GitHub reports for it."""
    files = release_files(tag)
    patterns = [arg for name in names for arg in ("--pattern", name)]
    subprocess.run(
        ["gh", "release", "download", tag, "--repo", PROJECT, "--dir", out, "--clobber", *patterns],
        check=True,
    )
    data = {name: (Path(out) / name).read_bytes() for name in names}
    unchanged(files, data)
    return files, data


def write_assets(directory, files):
    lines = (f"{name}\t{size}\t{digest}\n" for name, (size, digest) in sorted(files.items()))
    (Path(directory) / ASSETS).write_text("".join(lines))


def read_assets(directory):
    files = {}
    for line in (Path(directory) / ASSETS).read_text().splitlines():
        name, size, digest = line.split("\t")
        files[name] = (int(size), digest)
    return files


def signing_input(tag, out):
    """On the release workstation: the draft's indexes and asset digests, for the signing Mac."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    files, indexes = download(tag, INDEXES, out)
    check(indexes, files, tag)
    write_assets(out, files)


def prepare(args):
    signing_input(args.tag, args.dir)
    print(
        f"Copy {args.dir} to the signing Mac and run there: python3 scripts/linux_repo.py sign {args.tag} DIR"
    )


def sign(args):
    """On the signing Mac, offline: check the prepared indexes again, then sign them."""
    out = Path(args.dir)
    files = read_assets(out)
    indexes = {name: (out / name).read_bytes() for name in INDEXES}
    unchanged(files, indexes)
    check(indexes, files, args.tag)
    sign_indexes(out, indexes)
    print(f"Signed: upload {out / IN_RELEASE} and {out / DB_SIG} to {args.tag}")


def ready(args):
    """The draft carries signed indexes of its own packages. Publishing it
    without them stops pacman -Syu on every Arch system using the repository."""
    with tempfile.TemporaryDirectory() as out:
        files, data = download(args.tag, (*INDEXES, IN_RELEASE, DB_SIG), out)
    indexes = {name: data[name] for name in INDEXES}
    check(indexes, files, args.tag)
    check_signatures(indexes, data[IN_RELEASE], data[DB_SIG], PUBLIC_KEY.read_bytes())
    print(f"{args.tag}: the Linux repository is signed and matches its packages")


def sign_indexes(out, indexes):
    """InRelease and recog.db.sig next to the checked indexes in out."""
    with tempfile.TemporaryDirectory() as home:
        gpg(home, "--import", data=keychain_key())
        gpg(
            home,
            "--clearsign",
            "--digest-algo",
            "SHA512",
            "--output",
            out / IN_RELEASE,
            out / RELEASE,
        )
        gpg(home, "--detach-sign", "--output", out / DB_SIG, out / DB)
        subprocess.run(["gpgconf", "--homedir", home, "--kill", "gpg-agent"], check=False)
    check_signatures(
        indexes,
        (out / IN_RELEASE).read_bytes(),
        (out / DB_SIG).read_bytes(),
        PUBLIC_KEY.read_bytes(),
    )


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def verify(args):
    """Read the repository as apt and pacman do and check every byte they install."""
    base = args.base.rstrip("/") + "/"
    indexes = {name: fetch(base + name) for name in INDEXES}
    check_signatures(
        indexes, fetch(base + IN_RELEASE), fetch(base + DB_SIG), PUBLIC_KEY.read_bytes()
    )
    files = {}
    for name in referenced(indexes):
        data = fetch(base + name)
        files[name] = (len(data), sha256(data))
    version = stanza(indexes[PACKAGES].decode()).get("Version", "")
    check(indexes, files, args.tag or "v" + version.removesuffix("-1"))
    print(f"{base}: signed indexes of recog {version} match their packages")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="action", required=True)
    sub = subs.add_parser("index", help="write the indexes next to the packages (release workflow)")
    sub.add_argument("dist", type=Path)
    sub.set_defaults(run=lambda a: index(a.dist))
    sub = subs.add_parser("new-key", help="create the signing key once, in the login Keychain")
    sub.set_defaults(run=new_key)
    sub = subs.add_parser(
        "prepare", help="download and check a draft's indexes for the signing Mac"
    )
    sub.add_argument("tag", type=stable_tag)
    sub.add_argument("dir")
    sub.set_defaults(run=prepare)
    sub = subs.add_parser("sign", help="sign prepared indexes on the signing Mac, offline")
    sub.add_argument("tag", type=stable_tag)
    sub.add_argument("dir")
    sub.set_defaults(run=sign)
    sub = subs.add_parser("ready", help="check a draft's signed repository before publishing it")
    sub.add_argument("tag", type=stable_tag)
    sub.set_defaults(run=ready)
    sub = subs.add_parser("verify", help="check a published repository against the public key")
    sub.add_argument("--base", default=LATEST)
    sub.add_argument("--tag", type=stable_tag)
    sub.set_defaults(run=verify)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
