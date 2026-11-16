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


    def test_delete_prunes_graph_edges_and_related_ids(self):
        with fixture() as f:
            texts = [
                "The service uses Qdrant for vector storage.",
                "The service runs the nightly maintenance job on the homelab server.",
                "The service prefers Rust for its background workers.",
            ]
            ids = [store(f, text) for text in texts]
            authored, doomed, survivor = ids[0], ids[1], ids[2]
            f["api"]("/slumber/trigger", {}, "POST")
            built = f["api"]("/graph/build", {}, "POST")
            self.assertEqual(built["status"], "built")
            self.assertTrue(edges(f), "graph build produced no edges to prune")
            related = f["api"]("/memories/" + survivor)["related_memory_ids"]
            self.assertIn(doomed, related, "fixture never linked the memories")
            before = f["api"](f"/graph/traverse?memory_id={authored}&depth=2")
            self.assertTrue(
                any(step["to_id"] == doomed or step["from_id"] == doomed
                    for result in before["results"] for step in result["path"]),
                "fixture traversal never reached the memory about to be deleted",
            )

            f["api"]("/memories/" + doomed, method="DELETE")

            after = f["api"]("/memories/" + survivor)
            self.assertNotIn(doomed, after["related_memory_ids"], "stale related_memory_ids")
            self.assertEqual(
                len(after["related_memory_ids"]), len(after["association_strengths"])
            )
            traversal = f["api"](f"/graph/traverse?memory_id={authored}&depth=2")
            named = {
                step[key]
                for result in traversal["results"]
                for step in result["path"]
                for key in ("from_id", "to_id")
            }
            named |= {result["memory_id"] for result in traversal["results"]}
            self.assertNotIn(doomed, named, "graph traversal still names a deleted memory")
            self.assertFalse(
                [p for p in edges(f) if doomed in (p["payload"]["from_memory_id"],
                                                   p["payload"]["to_memory_id"])],
                "graph edge survived the delete",
            )

    def test_unconsolidate_prunes_summary_references(self):
        with fixture() as f:
            certified = store(
                f, "CERTIFIED_ONLY: my confirmed preference survives maintenance.", certified=True
            )
            sources = [store(f, "Consolidated source alpha"), store(f, "Consolidated source beta")]
            same_realm(f, [certified] + sources)
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summary = [
                p for p in points(f) if p["payload"].get("chunk_type") == "consolidated"
            ]
            self.assertEqual(len(summary), 1, "consolidation did not replace the sources")
            summary_id = summary[0]["id"]
            self.assertTrue(
                summary_id in f["api"]("/memories/" + certified)["related_memory_ids"],
                "fixture never linked the survivor to the summary",
            )

            f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")

            survivor = f["api"]("/memories/" + certified)
            self.assertNotIn(
                summary_id, survivor["related_memory_ids"], "stale related id after undo"
            )
            self.assertFalse(
                [p for p in edges(f) if summary_id in (p["payload"]["from_memory_id"],
                                                       p["payload"]["to_memory_id"])],
                "graph edge to the deleted summary survived the undo",
            )


    def test_interrupted_source_deletion_resumes_the_same_plan(self):
        """A failure inside the source-deletion loop used to abort with the journal
        at `prepared`, the summary stored and some sources gone. The next run then
        planned over the survivors, whose fingerprint no longer matched, producing
        a second overlapping summary. The journal must record the interruption and
        the next run must finish that same plan."""
        with fixture() as f:
            ids = [store(f, "Interrupted source alpha"), store(f, "Interrupted source beta")]
            same_realm(f, ids)

            # Fail the second delete of the run: the first source is already gone
            # when the pipeline aborts.
            first = run_cli(f, ["slumber", "trigger"], {"MEMEX8_TEST_FAIL_NTH_DELETE": "2"})
            self.assertNotEqual(first.returncode, 0, "injected failure was silently swallowed")
            summaries = [
                p for p in points(f) if p["payload"].get("chunk_type") == "consolidated"
            ]
            self.assertEqual(len(summaries), 1, "summary was not stored before the deletion phase")
            journal = operation(f, summaries[0]["id"])
            self.assertEqual(journal["state"], "deleting", "interruption was not recorded")
            live = {p["id"] for p in points(f)}
            self.assertEqual(len(live & set(ids)), 1, "expected exactly one source to remain")
            remaining = next(id_ for id_ in ids if id_ in live)

            # Retrying while the failure persists must not plan a second summary:
            # the unfinished journal is resumed first and its failure aborts.
            chats = f["state"]["summaries"]
            second = run_cli(f, ["slumber", "trigger"], {"MEMEX8_TEST_FAIL_NTH_DELETE": "1"})
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(f["state"]["summaries"], chats, "re-planned over the survivors")
            self.assertEqual(
                len([p for p in points(f) if p["payload"].get("chunk_type") == "consolidated"]),
                1,
            )
            self.assertEqual(operation(f, summaries[0]["id"])["state"], "deleting")

            # With the failure cleared the same plan finishes: one summary.
            third = run_cli(f, ["slumber", "trigger"])
            self.assertEqual(third.returncode, 0, third.stderr)
            self.assertEqual(operation(f, summaries[0]["id"])["state"], "complete")
            self.assertEqual({p["id"] for p in points(f)}, {summaries[0]["id"]})

            # Undo restores exactly the sources this run deleted.
            restored = f["api"](f"/memories/{summaries[0]['id']}/unconsolidate", method="POST")
            self.assertEqual(restored["restored"], 2)
            self.assertEqual({p["id"] for p in points(f)}, set(ids))
            self.assertIn(remaining, {p["id"] for p in points(f)})


    def test_archive_refuses_certified_but_explicit_delete_is_allowed(self):
        with fixture() as f:
            ordinary = store(f, "Ordinary memory to archive")
            self.assertEqual(f["api"]("/memories/" + ordinary)["importance"], 1.0)
            f["api"](f"/memories/{ordinary}/archive", {}, "POST")
            self.assertAlmostEqual(f["api"]("/memories/" + ordinary)["importance"], 0.01)

            certified = store(f, "CERTIFIED_ONLY: never decay this importance.", certified=True)
            self.assert_http(409, lambda: f["api"](f"/memories/{certified}/archive", {}, "POST"))
            kept = f["api"]("/memories/" + certified)
            self.assertEqual(kept["verification_status"], "certified")
            self.assertEqual(kept["importance"], 1.0, "archive decayed a certified memory")

            # A concurrent certification wins the write itself, not just the check:
            # slumber's decay path (same filtered primitive) leaves it untouched.
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertEqual(f["api"]("/memories/" + certified)["importance"], 1.0)

            # Explicit user deletion stays allowed for certified memories.
            f["api"]("/memories/" + certified, method="DELETE")
            self.assert_http(404, lambda: f["api"]("/memories/" + certified))


if __name__ == "__main__":
    unittest.main()
