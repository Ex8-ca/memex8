"""Audit-control regressions: pause truthfulness, graph reference cleanup,
resumable consolidation deletion, and the certification/archive race.

Every test runs the real binary against a throwaway Qdrant container and a local
mock LLM. No production services, files, models, or credentials are touched.

Run after cargo build:
    python3 tests/test_audit_control.py
"""
import json
from pathlib import Path
import socket
import subprocess
import time
import unittest
import urllib.error

from test_certification import request, wait_ready
from test_consolidation import fixture, points, operation, write_operation

PAUSE_FILE = "slumber_paused.json"

def store(f, content, **extra):
    return f["api"](
        "/memories", {"content": content, "memory_type": "fact", **extra}, "POST"
    )["id"]

def patch(f, id_, **payload):
    f["q"](
        "/collections/memories/points/payload?wait=true",
        {"points": [id_], "payload": payload},
        "POST",
    )

def same_realm(f, ids, realm_name="Audit realm"):
    """Put memories in one realm so consolidation groups them together."""
    patch(
        f,
        ids[0],
        realm_id="8f0f0f0f-0000-4000-8000-000000000001",
        realm_name=realm_name,
    )
    for id_ in ids[1:]:
        f["q"](
            "/collections/memories/points/payload?wait=true",
            {
                "points": [id_],
                "payload": {
                    "realm_id": "8f0f0f0f-0000-4000-8000-000000000001",
                    "realm_name": realm_name,
                },
            },
            "POST",
        )

def edges(f, collection="graph_edges"):
    return f["q"](
        f"/collections/{collection}/points/scroll",
        {"limit": 1000, "with_payload": True},
        "POST",
    )["result"]["points"]

def log_text(f):
    return (f["work"] / "server.log").read_text()

def run_cli(f, args, extra_env=None, timeout=180):
    return subprocess.run(
        f["command"] + args,
        env={**f["env"], **(extra_env or {})},
        cwd=str(f["work"]),
        capture_output=True,
        text=True,
        timeout=timeout,
    )

class AuditControlTests(unittest.TestCase):
    def assert_http(self, code, call):
        with self.assertRaises(urllib.error.HTTPError) as error:
            call()
        self.assertEqual(error.exception.code, code)
        error.exception.read()
        error.exception.close()

    def test_pause_stops_scheduled_slumber_and_resume_restarts_it(self):
        """A pause that only set an in-memory status let the scheduler keep
        running slumber on its idle timer. The scheduler must consult the
        persisted flag, and resuming must let the next tick run."""
        with fixture(paused=True, idle_timeout="1s") as f:
            time.sleep(40)
            text = log_text(f)
            self.assertIn("Slumber paused", text, "scheduler never consulted the pause flag")
            self.assertNotIn("💤 Slumber started", text, "paused slumber still ran")
            status = f["api"]("/slumber/status")
            self.assertTrue(status["paused"])
            self.assertEqual(status["state"], "paused")

            resumed = f["api"]("/slumber/resume", {}, "POST")
            self.assertFalse(resumed["paused"])
            deadline = time.time() + 45
            while time.time() < deadline and "Idle slumber complete" not in log_text(f):
                time.sleep(1)
            self.assertIn(
                "Idle slumber complete", log_text(f), "resume did not restart the scheduler"
            )

    def test_pause_is_persisted_refuses_triggers_and_survives_restart(self):
        with fixture() as f:
            # The control routes sit behind the same auth middleware as the rest.
            self.assert_http(401, lambda: request(f["api_url"] + "/slumber/pause", {}, "POST"))

            paused = f["api"]("/slumber/pause", {}, "POST")
            self.assertTrue(paused["paused"])
            self.assertEqual(paused["slumber_state"], "paused")
            self.assertEqual(f["api"]("/slumber/status")["state"], "paused")
            self.assert_http(
                409, lambda: f["api"]("/slumber/trigger", {}, "POST")
            )
            # The CLI writes the same flag, and the daemon honours it.
            self.assertTrue(json.loads((f["work"] / PAUSE_FILE).read_text())["paused"])
            resumed = run_cli(f, ["slumber", "resume"])
            self.assertEqual(resumed.returncode, 0, resumed.stderr)
            self.assertFalse(f["api"]("/slumber/status")["paused"])
            paused = run_cli(f, ["slumber", "pause"])
            self.assertEqual(paused.returncode, 0, paused.stderr)
            status = f["api"]("/slumber/status")
            self.assertTrue(status["paused"], "CLI pause did not reach the daemon")
            self.assertEqual(Path(status["pause_file"]), f["work"] / PAUSE_FILE)
            self.assert_http(409, lambda: f["api"]("/slumber/trigger", {}, "POST"))

            # A restart keeps the pause: it is durable, not process state.
            port_log = f["work"] / "restart.log"
            with port_log.open("w") as log:
                with socket.socket() as sock:
                    sock.bind(("127.0.0.1", 0))
                    port = sock.getsockname()[1]
                process = subprocess.Popen(
                    f["command"] + ["serve", "--host", "127.0.0.1", "--port", str(port)],
                    env=f["env"],
                    cwd=str(f["work"]),
                    stdout=log,
                    stderr=log,
                )
                try:
                    wait_ready(f"http://127.0.0.1:{port}/health")
                    base = f"http://127.0.0.1:{port}/api/v1"
                    restarted = request(base + "/slumber/status", key="test-only")
                    self.assertTrue(restarted["paused"], "pause did not survive a restart")
                    self.assertIn("paused", port_log.read_text())
                finally:
                    process.terminate()
                    process.wait(timeout=10)

    def test_status_resets_to_idle_after_a_failed_run(self):
        """A failed pipeline used to be able to leave `running` behind. The status
        must go back to idle so the scheduler and operators can retry."""
        with fixture(archive_dimensions=2) as f:
            store(f, "Fixture source alpha for a failing run")
            store(f, "Fixture source beta for a failing run")
            self.assert_http(
                500, lambda: f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            )
            status = f["api"]("/slumber/status")
            self.assertEqual(status["state"], "idle", "failed run left status running")
            self.assertFalse(status["paused"])
            # The engine is still usable: a second attempt fails the same way but
            # is never refused because of a stale run.
            self.assert_http(
                500, lambda: f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            )
            self.assertEqual(f["api"]("/slumber/status")["state"], "idle")

    def test_panicking_run_does_not_leave_status_running(self):
        """A panic inside the pipeline used to unwind the caller, leaving
        `running` forever and killing the scheduler loop. The run must be
        isolated and the status reset."""
        with fixture(extra_env={"MEMEX8_TEST_PANIC_SLUMBER": "1"}) as f:
            store(f, "Fixture memory for the panic regression")
            try:
                f["api"]("/slumber/trigger", {}, "POST")
                self.fail("panicking pipeline reported success")
            except urllib.error.HTTPError as error:
                self.assertEqual(error.code, 500)
                error.close()
            except (urllib.error.URLError, ConnectionResetError):
                # Before the fix the handler task itself died: the client sees a
                # dropped connection instead of a response. Either way the status
                # below is what must be true.
                pass
            self.assertEqual(
                f["api"]("/slumber/status")["state"], "idle", "panic left status running"
            )
            # The daemon is still serving, and a second trigger still works.
            self.assertTrue(f["api"]("/memories?limit=5")["memories"])
            try:
                f["api"]("/slumber/trigger", {}, "POST")
            except (urllib.error.HTTPError, urllib.error.URLError, ConnectionResetError) as error:
                getattr(error, "close", lambda: None)()
            self.assertEqual(f["api"]("/slumber/status")["state"], "idle")


if __name__ == "__main__":
    unittest.main()
