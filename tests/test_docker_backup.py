import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("backup", Path(__file__).parents[1] / "scripts/docker_backup.py")
backup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup)


def tar_bytes():
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w") as archive:
        info = tarfile.TarInfo("memory.data")
        content = b"memory payload with original metadata"
        info.size = len(content)
        archive.addfile(info, io.BytesIO(content))
    return data.getvalue()


class DockerBackupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.calls = []
        self.running = {"memex8": True, "qdrant": True}
        self.compose = ["docker", "compose", "-f", "compose.yml"]

    def info(self, service):
        return {"Id": service, "Image": "sha256:" + service,
                "State": {"Running": self.running[service]},
                "Mounts": [{"RW": True, "Destination": "/qdrant/storage" if service == "qdrant" else "/var/lib/memex8"}],
                "Config": {"Env": ["MEMEX8_API_KEY=example", "QDRANT_URL=http://old"], "Entrypoint": [service], "Cmd": [], "User": "1000"}}

    def capture(self, args):
        self.calls.append(args)
        if "inspect" in args:
            return json.dumps([self.info(args[-1])])
        if "ps" in args:
            return args[-1] + "\n"
        if "config" in args:
            return '{"services":{}}'
        if "create" in args:
            return "helper\n"
        return ""

    def fake_run(self, args, **kwargs):
        self.calls.append(args)
        if "stop" in args:
            self.running[args[-1]] = False
        if "start" in args:
            self.running[args[-1]] = True
        if "save" in args or ("cp" in args and args[-1] == "-"):
            kwargs["stdout"].write(tar_bytes())
        return subprocess.CompletedProcess(args, 0)

    def make_backup(self):
        with patch.object(backup, "capture", side_effect=self.capture), patch.object(backup, "run", side_effect=self.fake_run):
            return backup.backup(self.compose, self.root / "backups")

    def test_backup_stops_writers_and_database_before_copy_and_restarts(self):
        directory = self.make_backup()
        self.assertTrue((directory / "COMPLETE").is_file())
        self.assertEqual(set(backup.verify(directory)["services"]), {"memex8", "qdrant"})
        stops = [i for i, c in enumerate(self.calls) if "stop" in c]
        copies = [i for i, c in enumerate(self.calls) if "cp" in c]
        self.assertLess(max(stops), min(copies))
        self.assertEqual([c[-1] for c in self.calls if "stop" in c], ["memex8", "qdrant"])
        self.assertEqual([c[-1] for c in self.calls if "start" in c], ["qdrant", "memex8"])
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)

    def test_failed_copy_restarts_original_services_without_complete_marker(self):
        def fail_copy(args, **kwargs):
            if "cp" in args:
                raise subprocess.CalledProcessError(1, args)
            return self.fake_run(args, **kwargs)
        with patch.object(backup, "capture", side_effect=self.capture), patch.object(backup, "run", side_effect=fail_copy):
            with self.assertRaises(subprocess.CalledProcessError):
                backup.backup(self.compose, self.root / "backups")
        self.assertTrue(all(self.running.values()))
        self.assertEqual(list(self.root.rglob("COMPLETE")), [])

    def test_previously_stopped_services_remain_stopped(self):
        self.running = {"memex8": False, "qdrant": False}
        self.make_backup()
        self.assertFalse(any("start" in c for c in self.calls))

    def test_corruption_blocks_restore_before_any_docker_command(self):
        directory = self.make_backup()
        (directory / "memex8-0.tar").write_bytes(b"corrupt")
        with patch.object(backup, "run") as run:
            with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                backup.restore(directory, self.root / "recovery")
            run.assert_not_called()

    def test_restore_creates_isolated_volumes_and_does_not_start_service(self):
        directory = self.make_backup()
        self.calls.clear()
        with patch.object(backup, "capture", side_effect=self.capture), patch.object(backup, "run", side_effect=self.fake_run):
            path = backup.restore(directory, self.root / "recovery")
        config = json.loads(path.read_text())
        self.assertEqual(config["services"]["memex8"]["ports"], ["127.0.0.1:18080:8080"])
        self.assertIn("QDRANT_URL=http://qdrant:6334", config["services"]["memex8"]["environment"])
        self.assertTrue(all(v.startswith("memex8-recovery-") for v in config["volumes"]))
        self.assertFalse(any("up" in c or "start" in c for c in self.calls))

    def test_failed_backup_blocks_upgrade(self):
        with patch.object(backup, "backup", side_effect=ValueError("failed")), patch.object(backup, "run") as run, patch("sys.argv", ["backup", "upgrade", "-f", "compose.yml", "--output", str(self.root), "--image", "memex8:new"]):
            self.assertEqual(backup.main(), 1)
        self.assertFalse(any("up" in call.args[0] for call in run.call_args_list))

    def test_upgrade_preserves_running_embedding_configuration(self):
        directory = self.make_backup()
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        original = ["EMBEDDING_PROVIDER=openai", "EMBEDDING_MODEL=text-embedding-3-small",
                    "EMBEDDING_DIMENSIONS=1536", "OPENAI_API_KEY=test-only"]
        manifest["services"]["memex8"]["config"]["Env"] = original
        manifest_path.write_text(json.dumps(manifest))
        checks_path = directory / "checksums.json"
        checks = json.loads(checks_path.read_text())
        checks["manifest.json"] = backup.digest(manifest_path)
        checks_path.write_text(json.dumps(checks))
        with patch.object(backup, "backup", return_value=directory), patch.object(backup, "run") as run, patch("sys.argv", ["backup", "upgrade", "-f", "compose.yml", "--output", str(self.root), "--image", "memex8:new"]):
            self.assertEqual(backup.main(), 0)
        override = json.loads((directory / "upgrade.override.json").read_text())
        self.assertEqual(override["services"]["memex8"]["environment"], original)
        self.assertTrue(any("up" in call.args[0] for call in run.call_args_list))


if __name__ == "__main__":
    unittest.main()
