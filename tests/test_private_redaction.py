"""Privacy boundary tests: inspect provider requests, disposable Qdrant and captured logs."""
import hashlib
import json
import os
import subprocess
import unittest
import urllib.error
import uuid
from pathlib import Path

from test_consolidation import fixture, points, seed_sources
from test_compact_retrieval import mcp

SECRET = "DO_NOT_RETAIN_redaction_sentinel_8921"


class PrivateRedactionTests(unittest.TestCase):
    def assert_clean(self, f):
        for value in [f["state"]["requests"], points(f), (f["work"] / "server.log").read_text()]:
            self.assertNotIn(SECRET, json.dumps(value))

    def assert_http(self, code, call):
        with self.assertRaises(urllib.error.HTTPError) as error:
            call()
        self.assertEqual(error.exception.code, code)
        body = json.load(error.exception)
        error.exception.close()
        return body

    def test_store_metadata_empty_inputs_mcp_and_provider_failure_logs(self):
        with fixture() as f:
            raw = f"Public é🦀 <PrIvAtE>{SECRET}\n<private>nested</private> tail</PRIVATE> conclusion"
            id_ = f["api"]("/memories", {"content": raw, "tags": [f"tag<private>{SECRET}</private>"],
                                      "source": f"source<private>{SECRET}</private>"}, "POST")["id"]
            memory = f["api"]("/memories/" + id_)
            self.assertEqual(memory["content"], "Public é🦀 [private] conclusion")
            self.assertEqual(memory["tags"], ["tag[private]"])
            self.assertEqual(memory["source_file"], "source[private]")
            before = len(f["state"]["requests"])
            for content in [f"<private>{SECRET}</private>", f"<PRIVATE>{SECRET}", "[private] \n[private]"]:
                error = self.assert_http(422, lambda: f["api"]("/memories", {"content": content}, "POST"))
                self.assertEqual(error["error"], "content empty after redaction")
            self.assertEqual(len(f["state"]["requests"]), before)
            self.assertEqual(len(points(f)), 1)
            replies = mcp(f, [("memex8_remember", {"content": raw}),
                              ("memex8_remember", {"content": f"<private>{SECRET}</private>"})])
            self.assertIn("id", replies[0]["result"])
            self.assertIn("content empty after redaction", replies[1]["error"]["message"])
            self.assertNotIn(SECRET, json.dumps(replies))
            self.assert_clean(f)
            # An upstream failure body is untrusted and must not be reflected into logs/errors.
            f["state"]["fail_embedding"] = "public failure"
            f["state"]["error_body"] = f"<private>{SECRET}</private>"
            self.assert_http(500, lambda: f["api"]("/memories", {"content": "public failure"}, "POST"))
            self.assert_clean(f)

    def test_file_redaction_precedes_chunking_and_hashing(self):
        with fixture() as f:
            path = f["work"] / "cross-boundary.md"
            raw = "# Public\n\nIntroduction.\n\n<private>" + (SECRET + "\n\n# Hidden heading\n") * 1500 + "</private>\n\nPublic ending."
            clean = "# Public\n\nIntroduction.\n\n[private]\n\nPublic ending."
            path.write_text(raw)
            f["api"]("/memories/ingest", {"path": str(path), "chunk_by": "section"}, "POST")
            stored = points(f)
            self.assertTrue(stored)
            self.assertTrue(all(p["payload"]["source_hash"] == hashlib.sha256(clean.encode()).hexdigest() for p in stored))
            self.assertIn("Public ending", json.dumps(stored))
            before = len(f["state"]["requests"])
            path.write_text(f"<private>{SECRET}")
            f["api"]("/memories/ingest", {"path": str(path), "chunk_by": "paragraph"}, "POST")
            self.assertEqual(len(f["state"]["requests"]), before)
            self.assertEqual(len(points(f)), len(stored))
            self.assert_clean(f)

    def test_session_and_webhook_hide_cross_message_spans_before_llm(self):
        with fixture() as f:
            f["state"]["session_response"] = json.dumps({"session_id": "safe-session",
                "summary": f"Useful summary <private>{SECRET}</private>", "decisions": [
                    {"item_type": "decision", "content": f"Useful choice <private>{SECRET}</private>",
                     "source_message_idx": 0, "confidence": .9},
                    {"item_type": "decision", "content": f"<private>{SECRET}</private>",
                     "source_message_idx": 0, "confidence": .9}],
                "follow_ups": [], "insights": [], "raw_response": ""})
            messages = [{"role": "user", "content": f"Visible <private>{SECRET}"}]
            messages += [{"role": "assistant", "content": SECRET} for _ in range(51)]
            messages += [{"role": "user", "content": f"{SECRET}</private> public decision"}]
            report = f["api"]("/sessions/end", {"session_id": "safe-session", "messages": messages}, "POST")
            self.assertIsNotNone(report["summary_memory_id"])
            self.assertEqual(report["decisions_stored"], 1)
            self.assertTrue(any(r["path"] == "/generate" for r in f["state"]["requests"]))
            self.assert_clean(f)
            before = len(f["state"]["requests"])
            empty = f["api"]("/sessions/end", {"session_id": "all-private", "messages": [
                {"role": "user", "content": f"<private>{SECRET}</private>"}]}, "POST")
            self.assertIsNone(empty["summary_memory_id"])
            self.assertEqual(empty["status"], "skipped")
            self.assertEqual(empty["reason"], "content empty after redaction")
            self.assertEqual(len(f["state"]["requests"]), before)
            f["api"]("/webhooks/conversation", {"messages": messages}, "POST")
            self.assert_http(422, lambda: f["api"]("/webhooks/conversation", {"summary": f"<private>{SECRET}</private>"}, "POST"))
            self.assert_clean(f)

    def test_import_reembeds_changed_vectors_and_skips_empty_records(self):
        with fixture() as f:
            id_ = f["api"]("/memories", {"content": "Template memory"}, "POST")["id"]
            template = f["api"]("/memories/" + id_)
            imported_id = str(uuid.uuid4())
            payload = {**template, "id": imported_id, "content": f"Imported public <private>{SECRET}</private>",
                       "summary": f"Summary <private>{SECRET}</private>", "heading": f"Heading <private>{SECRET}</private>"}
            bundle = f["work"] / "import.json"
            bundle.write_text(json.dumps([{**payload, "vector": [0., 1.] + [0.] * 62},
                {**payload, "id": str(uuid.uuid4()), "content": f"<private>{SECRET}</private>", "vector": [0., 1.] + [0.] * 62}]))
            before = f["state"]["embeddings"]
            result = subprocess.run(f["command"] + ["import", str(bundle), "--reuse-vectors"], env=f["env"],
                                    cwd=f["work"], capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn(SECRET, result.stdout + result.stderr)
            self.assertEqual(f["state"]["embeddings"] - before, 1)
            memory = f["api"]("/memories/" + imported_id)
            self.assertEqual(memory["content"], "Imported public [private]")
            point = next(p for p in points(f) if p["id"] == imported_id)
            self.assertAlmostEqual(point["vector"][0], 1.)
            self.assertEqual(len(points(f)), 2)
            # CLI edits use the same sanitizer before embedding and payload replacement.
            editor = f["work"] / "edit.py"
            editor.write_text("#!/usr/bin/env python3\nimport sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text(" + repr(f"Edited public <private>{SECRET}</private>") + ")\n")
            editor.chmod(0o700)
            result = subprocess.run(f["command"] + ["edit", id_], env={**f["env"], "EDITOR": str(editor)},
                                    cwd=f["work"], capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(f["api"]("/memories/" + id_)["content"], "Edited public [private]")
            self.assert_clean(f)

    def test_consolidated_llm_output_is_redacted_before_embedding(self):
        with fixture(preserve=True) as f:
            seed_sources(f)
            f["state"]["chat_response"] = f"Derived public <private>{SECRET}</private> conclusion."
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summaries = [p for p in points(f) if p["payload"].get("chunk_type") == "consolidated"]
            self.assertEqual(len(summaries), 1)
            self.assertEqual(summaries[0]["payload"]["content"], "Derived public [private] conclusion.")
            self.assert_clean(f)


if __name__ == "__main__":
    unittest.main()
