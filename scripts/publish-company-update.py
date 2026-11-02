#!/usr/bin/env python3
"""Promote an existing signed company release; leave all other service settings intact."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPOSITORY = "qinghua362330/cc-switch-company"
SERVICE = "codex-provision.service"
KEYS = ("CC_SWITCH_STABLE_VERSION", "CC_SWITCH_STABLE_TAG",
        "CC_SWITCH_CANARY_VERSION", "CC_SWITCH_CANARY_TAG")


def release_version(tag):
    match = re.fullmatch(r"v(\d+\.\d+\.\d+)-company\.([1-9]\d*)", tag)
    if not match or int(match[2]) > 65535:
        raise ValueError("invalid company release tag")
    return match[1]


def validate_manifest(data, tag, commit):
    release_version(tag)
    if data.get("version") != tag[1:] or data.get("build_sha") != commit:
        raise ValueError("release identity does not match the approved build")
    prefix = f"https://github.com/{REPOSITORY}/releases/download/{tag}/"
    for platform in ("windows-x86_64", "darwin-aarch64", "darwin-x86_64"):
        entry = data.get("platforms", {}).get(platform, {})
        installer = data.get("installers", {}).get(platform, {})
        if not entry.get("signature") or not str(entry.get("url", "")).startswith(prefix):
            raise ValueError("missing signed platform artifact")
        if installer.get("url") != entry["url"] or not re.fullmatch(r"[0-9a-f]{64}", installer.get("sha256", "")):
            raise ValueError("missing platform artifact checksum")


def updated_environment(original, tag):
    version = release_version(tag)
    values = {KEYS[0]: version, KEYS[1]: tag, KEYS[2]: version, KEYS[3]: tag}
    seen = set()
    output = []
    for line in original.splitlines(keepends=True):
        key = line.split("=", 1)[0].strip()
        if key in values:
            if key in seen:
                raise ValueError("duplicate release setting")
            seen.add(key)
            output.append(f"{key}={values[key]}\n")
        else:
            output.append(line)
    if output and not output[-1].endswith("\n"):
        output[-1] += "\n"
    output.extend(f"{key}={values[key]}\n" for key in KEYS if key not in seen)
    return "".join(output)


def atomic_replace(path, data, mode):
    fd, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            os.fchmod(handle.fileno(), mode)
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def service_healthy(url):
    try:
        urllib.request.urlopen(url, timeout=3).close()
    except urllib.error.HTTPError as error:
        return error.code == 401
    except (OSError, urllib.error.URLError):
        pass
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--env-file", type=Path, default=Path("/etc/codex-provision.env"))
    parser.add_argument("--health-url", default="http://127.0.0.1:18181/api/client/update")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
        raise ValueError("invalid build commit")
    validate_manifest(json.loads(args.manifest.read_text()), args.tag, args.commit)
    path = args.env_file
    if path.is_symlink() or not path.is_file():
        raise ValueError("environment must be a regular non-symlink file")
    old = path.read_bytes()
    new = updated_environment(old.decode("utf-8"), args.tag).encode("utf-8")
    if not args.apply:
        print(json.dumps({"mode": "dry-run", "version": args.tag[1:], "keys": KEYS}))
        return
    if os.geteuid() != 0:
        raise ValueError("production promotion requires root")
    subprocess.run(["systemctl", "is-active", "--quiet", SERVICE], check=True, timeout=10)
    backup = Path(tempfile.mkdtemp(prefix="cc-switch-update-", dir="/opt/backups"))
    os.chmod(backup, 0o700)
    atomic_replace(backup / path.name, old, 0o600)
    mode = path.stat().st_mode & 0o777
    if path.read_bytes() != old:
        raise ValueError("environment changed during preflight; no changes applied")
    atomic_replace(path, new, mode)
    try:
        subprocess.run(["systemctl", "restart", SERVICE], check=True, timeout=30)
        for _ in range(10):
            if service_healthy(args.health_url):
                break
            time.sleep(1)
        else:
            raise RuntimeError("provision service health check failed")
    except Exception:
        if path.read_bytes() != new:
            raise RuntimeError("environment changed after promotion; manual recovery required") from None
        atomic_replace(path, old, mode)
        subprocess.run(["systemctl", "restart", SERVICE], check=True, timeout=30)
        raise RuntimeError("promotion failed; original environment restored") from None
    print(json.dumps({"mode": "promoted", "version": args.tag[1:], "backup": str(backup),
                      "env_sha256": hashlib.sha256(new).hexdigest()}))


if __name__ == "__main__":
    main()
