"""Index-coherence probes: ingest, restored-pair, mixed-pair and rebuild failure.

Each probe fails against the pre-fix tree and passes against the fixed one. They
drive the real server binary over HTTP against a disposable Qdrant (docker) and a
local mock embedding/LLM server, exactly like tests/test_index_freshness.py.

Run after cargo build: python3 tests/test_index_coherence.py
Point MEMEX8_TEST_BINARY at another build to compare revisions.
"""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import socket
import subprocess
import unittest

from test_certification import request, wait_ready
from test_consolidation import fixture, points

# The pair plus everything that describes it: the `.tv`, the id map, and (on the
# fixed tree) the fingerprint sidecar. Copies are byte-exact stand-ins for a
# backup restore or for an unlink that never happened.
PAIR_FILES = ("test.tv", "test-ids.json", "test.tv.meta.json")


def search(f, query="zqxv", **options):
    body = {"query": query, "limit": 100, "min_score": 0.0, **options}
    return f["api"]("/memories/search", body, "POST")["results"]


def store(f, content):
    return f["api"]("/memories", {"content": content, "memory_type": "fact"}, "POST")["id"]


@contextmanager
def restarted(f, name="restart"):
    """Serve again from the same data directory: same Qdrant, same index files."""
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    log_path = f["work"] / f"{name}.log"
    with log_path.open("w") as log:
        process = subprocess.Popen(
            f["command"] + ["serve", "--host", "127.0.0.1", "--port", str(port)],
            env=f["env"],
            cwd=f["work"],
            stdout=log,
            stderr=log,
        )
        try:
            wait_ready(f"http://127.0.0.1:{port}/health")
            base = f"http://127.0.0.1:{port}/api/v1"
            yield {
                "api": lambda path, body=None, method="GET": request(
                    base + path, body, method, "test-only"
                ),
                "log_path": log_path,
            }
        finally:
            process.terminate()
            process.wait(timeout=10)


def log_text(path):
    return Path(path).read_text()


def snapshot(f):
    return {
        name: (f["work"] / name).read_bytes()
        for name in PAIR_FILES
        if (f["work"] / name).exists()
    }


def restore(f, files):
    for name, data in files.items():
        (f["work"] / name).write_bytes(data)


class IndexCoherenceTests(unittest.TestCase):
    def test_ingest_while_an_index_is_loaded_is_searchable(self):
        """A new file ingested over HTTP is not in a loaded index, so the write has
        to drop that index: otherwise the memories live in Qdrant and are invisible
        to search until the next restart."""
        with fixture() as f:
            baseline = store(f, "Sentinel baseline fact")
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertTrue((f["work"] / "test.tv").exists())
            document = f["work"] / "note.md"
            document.write_text(
                "# Coherence probe\n\n"
                "The zqxv ledger probe records a note ingested while an index was loaded.\n"
            )
            with restarted(f) as second:
                self.assertIn("Loaded TurboVec index", log_text(second["log_path"]))
                second["api"](
                    "/memories/ingest",
                    {"path": str(document), "chunk_by": "section"},
                    "POST",
                )
                live = {p["id"] for p in points(f)}
                ingested = live - {baseline}
                self.assertEqual(len(ingested), 1, live)
                found = {r["id"] for r in search(second)}
                self.assertEqual(
                    found,
                    live,
                    f"ingested memory hidden by the loaded index "
                    f"({len(found)} of {len(live)} searchable)",
                )
                # The stale index must not survive the write either on disk...
                self.assertFalse(
                    (f["work"] / "test.tv").exists(), "ingest kept a stale index"
                )

    def test_pair_restored_after_an_edit_is_rejected(self):
        """An edit re-embeds a memory without changing its ID or `source_hash`.
        Restoring the pre-edit pair (a backup, or an unlink that failed) therefore
        passes every ID-set check while holding the old vector, so the pair has to
        be rejected on content."""
        with fixture() as f:
            original = "Zqxv original ledger text"
            edited = "Zqxv rewritten ledger text"
            f["state"]["embedding_vectors"][original] = [0.0, 1.0] + [0.0] * 62
            f["state"]["embedding_vectors"][edited] = [1.0, 0.0] + [0.0] * 62
            id_ = store(f, original)
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertTrue((f["work"] / "test.tv").exists())
            before = snapshot(f)
            editor = f["work"] / "editor.sh"
            editor.write_text(
                "#!/bin/sh\nprintf '%s' " + json.dumps(edited) + ' > "$1"\n'
            )
            editor.chmod(0o755)
            result = subprocess.run(
                f["command"] + ["edit", id_],
                env={**f["env"], "EDITOR": str(editor)},
                cwd=f["work"],
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(id_, {p["id"] for p in points(f)})
            self.assertFalse((f["work"] / "test.tv").exists(), "edit kept a stale index")
            # Put the pre-edit pair back, byte for byte.
            restore(f, before)
            with restarted(f) as second:
                text = log_text(second["log_path"])
                self.assertNotIn(
                    "Loaded TurboVec index",
                    text,
                    "a pre-edit pair was accepted for the edited corpus",
                )
                # Only the post-edit vector can reach this score.
                found = {r["id"] for r in search(second, min_score=0.5)}
            self.assertIn(id_, found, "edited memory ranked by its old vector")

    def test_foreign_index_beside_a_matching_id_map_is_rejected(self):
        """The `.tv` and the id map are renamed into place separately. A stale
        `.tv` paired with a fresh map of the same length resolves every slot to
        the wrong memory and passes an ID-set check, so the pair must be refused."""
        with fixture() as f:
            # Distinct embeddings, so the two pairs really do hold different codes.
            f["state"]["embedding_vectors"]["Ledger alpha fact"] = [1.0, 0.0] + [0.0] * 62
            f["state"]["embedding_vectors"]["Ledger beta fact"] = [0.0, 1.0] + [0.0] * 62
            first = [store(f, "Ledger alpha fact"), store(f, "Ledger beta fact")]
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertEqual(len(json.loads((f["work"] / "test-ids.json").read_text())), 2)
            foreign = (f["work"] / "test.tv").read_bytes()
            for id_ in first:
                f["api"](f"/memories/{id_}", method="DELETE")
            self.assertEqual(points(f), [])
            second_ids = [store(f, "Ledger gamma fact"), store(f, "Ledger delta fact")]
            f["api"]("/slumber/trigger", {}, "POST")
            ids_now = json.loads((f["work"] / "test-ids.json").read_text())
            self.assertEqual(set(ids_now), set(second_ids))
            self.assertNotEqual((f["work"] / "test.tv").read_bytes(), foreign)
            # Same length as the fresh map: the count check cannot help here.
            (f["work"] / "test.tv").write_bytes(foreign)
            with restarted(f) as second:
                text = log_text(second["log_path"])
                self.assertNotIn(
                    "Loaded TurboVec index",
                    text,
                    "a foreign .tv was accepted beside a same-length id map",
                )
                self.assertIn("was replaced after the sidecar was written", text)
                found = {r["id"] for r in search(second)}
            self.assertEqual(found, set(second_ids))

    def test_failed_unlink_poisons_the_pair(self):
        """If the pair cannot be unlinked, the leftover files must not stay
        loadable: a marker is written and the load path refuses the pair."""
        with fixture() as f:
            first = store(f, "Ledger poison alpha")
            f["api"]("/slumber/trigger", {}, "POST")
            pair = snapshot(f)
            # Make one unlink fail: a directory where the id map lives.
            (f["work"] / "test-ids.json").unlink()
            (f["work"] / "test-ids.json").mkdir()
            second_id = store(f, "Ledger poison beta")
            self.assertFalse((f["work"] / "test.tv").exists())
            self.assertTrue(
                (f["work"] / "test.tv.stale").exists(),
                "a pair that survived its invalidation was left loadable",
            )
            # Put the pair back, byte for byte; the marker must still win.
            (f["work"] / "test-ids.json").rmdir()
            restore(f, pair)
            self.assertTrue((f["work"] / "test.tv").exists())
            with restarted(f) as second:
                text = log_text(second["log_path"])
                self.assertNotIn("Loaded TurboVec index", text)
                self.assertIn("marked stale", text)
                found = {r["id"] for r in search(second)}
            self.assertEqual(found, {first, second_id})

    def test_failed_rebuild_is_visible_in_the_report(self):
        """`quantized == 0` is also what an empty corpus reports, so a failed final
        rebuild must be distinguishable: an error log line and a report field."""
        with fixture() as f:
            store(f, "Ledger rebuild fact")
            # A directory where the writer wants its temp file makes the save fail.
            (f["work"] / "test.tv.tmp").mkdir()
            f["api"]("/slumber/trigger", {}, "POST")
            report = f["api"]("/slumber/status")["last_report"]
            self.assertEqual(report["quantized"], 0)
            self.assertTrue(
                report.get("compressed_index_rebuild_error"),
                "a failed rebuild is reported as an empty corpus",
            )
            self.assertIn(
                "Compressed index rebuild FAILED", (f["work"] / "server.log").read_text()
            )
            self.assertFalse((f["work"] / "test.tv").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
