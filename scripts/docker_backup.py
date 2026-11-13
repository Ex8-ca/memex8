#!/usr/bin/env python3
"""Cold Docker Compose backup, isolated recovery, and backup-gated app upgrade."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import uuid


def run(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def capture(args):
    return run(args, stdout=subprocess.PIPE, text=True).stdout


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def verify(directory):
    directory = Path(directory)
    checks = json.loads((directory / "checksums.json").read_text())
    if "manifest.json" not in checks or "images.tar" not in checks:
        raise ValueError("Incomplete backup checksum inventory")
    for name, expected in checks.items():
        if Path(name).name != name or (directory / name).is_symlink():
            raise ValueError("Invalid backup filename")
        if digest(directory / name) != expected:
            raise ValueError("Checksum mismatch: " + name)
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest.get("format") != 1 or set(manifest["services"]) != {"memex8", "qdrant"}:
        raise ValueError("Unsupported backup manifest")
    for service in manifest["services"].values():
        for mount in service["mounts"]:
            if mount["archive"] not in checks:
                raise ValueError("Unverified volume archive")
            with tarfile.open(directory / mount["archive"]) as archive:
                # Read every regular file to detect truncated archives as well.
                for member in archive:
                    if member.isfile():
                        stream = archive.extractfile(member)
                        while stream.read(1024 * 1024):
                            pass
    return manifest


def backup(compose, output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory = output / (datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8])
    directory.mkdir(mode=0o700)
    manifest = {"format": 1, "services": {}, "excluded": ["/watch (source documents)"]}
    containers = {}
    for service in ("memex8", "qdrant"):
        ids = capture(compose + ["ps", "-a", "-q", service]).split()
        if len(ids) != 1:
            raise ValueError("Expected one existing container for " + service)
        info = json.loads(capture(["docker", "inspect", ids[0]]))[0]
        containers[service] = info
        required = "/qdrant/storage" if service == "qdrant" else "/var/lib/memex8"
        mounts = [m for m in info["Mounts"] if m.get("RW") and m["Destination"] != "/watch"]
        if required not in {m["Destination"] for m in mounts}:
            raise ValueError("Missing persistent data mount: " + required)
        manifest["services"][service] = {
            "image": info["Image"], "config": info["Config"],
            "mounts": [{"target": m["Destination"], "archive": f"{service}-{i}.tar"} for i, m in enumerate(mounts)]}
    # Both files can contain credentials; the directory is deliberately private.
    (directory / "compose.resolved.json").write_text(capture(compose + ["config", "--format", "json"]))
    with (directory / "images.tar").open("wb") as stream:
        run(["docker", "image", "save", *sorted({c["Image"] for c in containers.values()})], stdout=stream)
    running = [s for s in ("memex8", "qdrant") if containers[s]["State"]["Running"]]
    try:
        # Stop writers first, then database; copy only after both have stopped.
        for service in running:
            run(compose + ["stop", "--timeout", "60", service])
        for service, info in containers.items():
            current = json.loads(capture(["docker", "inspect", info["Id"]]))[0]
            if current["State"]["Running"]:
                raise ValueError("Container is still running: " + service)
            for mount in manifest["services"][service]["mounts"]:
                with (directory / mount["archive"]).open("wb") as stream:
                    run(["docker", "cp", info["Id"] + ":" + mount["target"].rstrip("/") + "/.", "-"], stdout=stream)
        (directory / "manifest.json").write_text(json.dumps(manifest, indent=2))
        checks = {p.name: digest(p) for p in directory.iterdir() if p.is_file()}
        (directory / "checksums.json").write_text(json.dumps(checks, indent=2))
        verify(directory)
        (directory / "COMPLETE").write_text("Backup checksums and data archives verified. Restore drill still required.\n")
    finally:
        # Restart original containers, without recreating or pulling anything.
        for service in reversed(running):
            run(compose + ["start", service])
    return directory


def restore(directory, output):
    directory, output = Path(directory).resolve(), Path(output).resolve()
    manifest = verify(directory)  # Validate before loading images/creating volumes.
    output.mkdir(parents=True, mode=0o700)  # Refuse an existing recovery directory.
    run(["docker", "image", "load", "--input", str(directory / "images.tar")])
    prefix = "memex8-recovery-" + uuid.uuid4().hex[:12]
    config = {"name": prefix, "services": {}, "volumes": {}}
    for service, saved in manifest["services"].items():
        original = saved["config"]
        item = {"image": saved["image"], "environment": original.get("Env", []),
                "entrypoint": original.get("Entrypoint"), "command": original.get("Cmd"),
                "user": original.get("User", ""), "volumes": [], "restart": "no"}
        if original.get("WorkingDir"):
            item["working_dir"] = original["WorkingDir"]
        for i, mount in enumerate(saved["mounts"]):
            volume = f"{prefix}-{service}-{i}"
            existing = capture(["docker", "volume", "ls", "--format", "{{.Name}} "]).split()
            if volume in existing:
                raise ValueError("Refusing to overwrite existing volume " + volume)
            run(["docker", "volume", "create", volume], stdout=subprocess.DEVNULL)
            helper = capture(["docker", "create", "--network", "none", "--mount", f"type=volume,source={volume},target=/restore", "--entrypoint", "/bin/sh", manifest["services"]["memex8"]["image"]]).strip()
            try:
                with (directory / mount["archive"]).open("rb") as stream:
                    run(["docker", "cp", "-a", "-", helper + ":/restore"], stdin=stream)
            finally:
                run(["docker", "rm", helper], stdout=subprocess.DEVNULL)
            config["volumes"][volume] = {"external": True, "name": volume}
            item["volumes"].append({"type": "volume", "source": volume, "target": mount["target"]})
        if service == "memex8":
            item["environment"] = [e for e in item["environment"] if not e.startswith("QDRANT_URL=")] + ["QDRANT_URL=http://qdrant:6334"]
            item["ports"] = ["127.0.0.1:18080:8080"]
            item["depends_on"] = ["qdrant"]
        config["services"][service] = item
    path = output / "compose.recovery.json"
    path.write_text(json.dumps(config, indent=2))
    return path


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("backup", "verify", "restore", "upgrade"))
    parser.add_argument("--output", help="Backup parent directory, or new recovery directory")
    parser.add_argument("--backup", help="Existing backup directory for verify/restore")
    parser.add_argument("-f", "--compose-file", action="append", default=[])
    parser.add_argument("--image", help="Exact new memex8 image tag/digest for upgrade (required)")
    args = parser.parse_args()
    compose = ["docker", "compose"]
    for file in args.compose_file:
        compose += ["-f", file]
    try:
        if args.action in ("verify", "restore"):
            if not args.backup:
                parser.error("--backup is required")
            if args.action == "verify":
                verify(args.backup)
                print("Checksums and data archives verified; no services changed.")
            else:
                if not args.output:
                    parser.error("--output must name a new recovery directory")
                print("Recovery prepared (not started):", restore(args.backup, args.output))
        else:
            if not args.output:
                parser.error("--output is required")
            if args.action == "upgrade":
                if not args.image or args.image.startswith("-"):
                    parser.error("--image must name the already-built new image")
                run(["docker", "image", "inspect", args.image], stdout=subprocess.DEVNULL)
                if not args.compose_file:
                    parser.error("upgrade requires -f pointing to your existing compose file(s)")
            directory = backup(compose, args.output)
            print("Verified backup:", directory, flush=True)
            if args.action == "upgrade":
                saved = verify(directory)
                override = directory / "upgrade.override.json"
                override.write_text(json.dumps({"services": {"memex8": {
                    "image": args.image,
                    # Preserve the actual container settings, even if Compose
                    # files have since drifted or disagree with each other.
                    # Escape "$" so Compose does not interpolate values such as an
                    # argon2 hash ("$argon2id$v=19$...") when it reads this file.
                    "environment": [e.replace("$", "$$") for e in saved["services"]["memex8"]["config"].get("Env", [])],
                    "healthcheck": {"test": ["CMD", "curl", "-fsS", "http://localhost:8080/health"], "interval": "5s", "timeout": "3s", "retries": 12}
                }}}))
                # Only replace the app. Never upgrade Qdrant or remove volumes.
                run(compose + ["-f", str(override), "up", "-d", "--no-deps", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "120", "memex8"])
                print("App upgrade completed. Keep the override for subsequent Compose operations:", override)
    except (ValueError, OSError, subprocess.CalledProcessError, tarfile.TarError) as exc:
        # Docker commands may already have logged diagnostics. Do not dump manifests.
        print("Operation failed; no successful upgrade is claimed:", str(exc), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
