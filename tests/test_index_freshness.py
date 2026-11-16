"""Derived-index coherence: restart, write, edit and undo lifecycles.

The compressed (TurboVec) index and the BM25 keyword index are caches derived
from Qdrant. Every test here fails when a cache is allowed to describe a corpus
that has already changed: summaries that consolidation inserted, memories stored
after the index was loaded, edits that changed a stored vector, and sources
restored by an undo.

Run after cargo build: python3 tests/test_index_freshness.py
No production services, files, models, or credentials are used.
"""
from contextlib import contextmanager
import json
from pathlib import Path
import socket
import subprocess
import unittest
import uuid

from test_certification import request, wait_ready
from test_consolidation import fixture, points


def search(f, query="zqxv", **options):
    body = {"query": query, "limit": 100, "min_score": 0.0, **options}
    return f["api"]("/memories/search", body, "POST")["results"]


def store(f, content):
    return f["api"](
        "/memories", {"content": content, "memory_type": "fact"}, "POST"
    )["id"]


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


class IndexFreshnessTests(unittest.TestCase):
    def test_consolidated_summary_is_searchable_after_restart(self):
        """Phase 6 deletes the sources and inserts a summary after the index was
        written. The vector count no longer matches, but a count-only freshness
        check accepted the index anyway and the summary stayed invisible."""
        with fixture() as f:
            ids = [store(f, "Original source alpha"), store(f, "Original source beta")]
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            live = {p["id"] for p in points(f)}
            summaries = [
                p["id"]
                for p in points(f)
                if p["payload"].get("chunk_type") == "consolidated"
            ]
            self.assertEqual(len(summaries), 1)
            self.assertEqual(live, set(summaries), "consolidation did not replace sources")
            self.assertTrue((f["work"] / "test.tv").exists())
            with restarted(f) as second:
                found = {r["id"] for r in search(second)}
            # The index really was loaded; the failure is that it described the
            # pre-consolidation corpus.
            self.assertIn("Loaded TurboVec index", log_text(second["log_path"]))
            self.assertIn(summaries[0], found, "summary hidden by the reloaded index")
            self.assertFalse(set(ids) & found)

    def test_stale_index_is_rejected_and_search_falls_back_to_qdrant(self):
        """Same vector count, different IDs: the pair no longer matches the corpus
        it was built from. It must be refused (its id map also disagrees with the
        sidecar that describes the pair) and search must still return the live
        corpus from Qdrant rather than a truncated index result."""
        with fixture() as f:
            ids = [store(f, "Fallback alpha source"), store(f, "Fallback beta source")]
            f["api"]("/slumber/trigger", {}, "POST")
            ids_path = f["work"] / "test-ids.json"
            self.assertEqual(set(json.loads(ids_path.read_text())), set(ids))
            replacement = [str(uuid.uuid4()) for _ in ids]
            ids_path.write_text(json.dumps(replacement))
            with restarted(f) as second:
                found = {r["id"] for r in search(second)}
            text = log_text(second["log_path"])
            self.assertNotIn("Loaded TurboVec index", text)
            self.assertIn("was replaced after the sidecar was written", text)
            self.assertEqual(found, set(ids), "live corpus not served from Qdrant")

    def test_memory_stored_after_the_index_was_loaded_is_searchable(self):
        with fixture() as f:
            store(f, "Sentinel baseline fact")
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertTrue((f["work"] / "test.tv").exists())
            with restarted(f) as second:
                self.assertIn("Loaded TurboVec index", log_text(second["log_path"]))
                new_id = second["api"](
                    "/memories",
                    {"content": "Brand new zebra fact", "memory_type": "fact"},
                    "POST",
                )["id"]
                found = {r["id"] for r in search(second)}
                # The stale index must not survive the write either on disk...
                self.assertFalse((f["work"] / "test.tv").exists())
            self.assertIn(new_id, found, "new memory invisible while an index was loaded")

    def test_edit_invalidates_the_loaded_index(self):
        """A content edit re-embeds the memory. A reloaded index would still hold
        the old vector, so the edit has to drop it."""
        with fixture() as f:
            original = "Zqxv original source"
            edited = "Zqxv rewritten source"
            f["state"]["embedding_vectors"][original] = [0.0, 1.0] + [0.0] * 62
            f["state"]["embedding_vectors"][edited] = [1.0, 0.0] + [0.0] * 62
            id_ = store(f, original)
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertTrue((f["work"] / "test.tv").exists())
            editor = f["work"] / "editor.sh"
            editor.write_text("#!/bin/sh\nprintf '%s' " + json.dumps(edited) + " > \"$1\"\n")
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
            self.assertFalse((f["work"] / "test.tv").exists(), "edit kept a stale index")
            with restarted(f) as second:
                found = {r["id"] for r in search(second, min_score=0.5)}
            self.assertIn(id_, found, "edited memory ranked by its old vector")

    def test_undo_resyncs_keyword_matches(self):
        """Undo deletes the summary and restores its sources. The keyword index
        still held the summary and not the sources, so BM25-only hits (orthogonal
        embeddings) stayed invisible."""
        with fixture() as f:
            texts = ["Zqxv alpha source", "Zqxv beta source"]
            for text in texts:
                f["state"]["embedding_vectors"][text] = [0.0, 1.0] + [0.0] * 62
            ids = [store(f, text) for text in texts]
            # Orthogonal to the query vector, so only the keyword bonus surfaces
            # them: this is the BM25 path under test.
            self.assertEqual({r["id"] for r in search(f, min_score=0.1)}, set(ids))
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summaries = [
                p["id"]
                for p in points(f)
                if p["payload"].get("chunk_type") == "consolidated"
            ]
            self.assertEqual(len(summaries), 1)
            self.assertEqual(
                {r["id"] for r in search(f, min_score=0.1)}, {summaries[0]}
            )
            undone = f["api"](f"/memories/{summaries[0]}/unconsolidate", method="POST")
            self.assertEqual(undone["restored"], 2)
            found = {r["id"] for r in search(f, min_score=0.1)}
            self.assertEqual(found, set(ids), "restored sources missing from keyword search")


if __name__ == "__main__":
    unittest.main()
