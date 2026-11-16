"""Truthful directory-ingest outcomes, end to end against disposable Qdrant.

Run after cargo build: python3 tests/test_ingest_outcomes.py
No production services, files, models, or credentials are used: the fixture
starts a throwaway Qdrant container and a local mock embedding server.
"""
import json
import os
import unittest
import urllib.error

from test_consolidation import fixture, points


def issue(report, path):
    found = next((i for i in report["issues"] if i["path"] == path), None)
    if found is None:
        raise AssertionError(f"no issue for {path!r} in {report['issues']}")
    return found


class IngestOutcomeTests(unittest.TestCase):
    def test_mixed_tree_reports_partial_and_never_claims_success(self):
        with fixture() as f:
            root = f["work"] / "docs"
            (root / "sub").mkdir(parents=True)
            (root / "good.md").write_text("# Good\n\nReadable body text for the good file.\n")
            (root / "bad.md").write_bytes(b"# Bad\n\n" + b"\xff\xfe" + b"\n")
            outside = f["work"] / "outside"
            outside.mkdir()
            (outside / "secret.md").write_text("# Secret\n\nOutside the root.\n")
            (root / "escape.md").symlink_to(outside / "secret.md")
            (root / "sub" / "loop").symlink_to(root)

            report = f["api"]("/memories/ingest", {"path": str(root), "chunk_by": "section"}, "POST")

            self.assertEqual(report["status"], "partial", report)
            self.assertEqual(report["path"], str(root), "compatible path field kept")
            self.assertEqual(report["attempted"], 2)
            self.assertEqual(report["stored"], 1)
            self.assertEqual(report["failed"], 1)
            self.assertGreaterEqual(report["skipped"], 2)
            self.assertGreaterEqual(report["chunks_stored"], 1)
            self.assertEqual(issue(report, "bad.md")["outcome"], "failed")
            self.assertEqual(issue(report, "bad.md")["reason"], "invalid_utf8")
            self.assertEqual(issue(report, "escape.md")["reason"], "symlink")
            self.assertEqual(issue(report, "sub/loop")["reason"], "symlink")

            # Per-path detail is relative and carries no raw OS/store error text.
            details = json.dumps(report["issues"])
            for needle in ["os error", "Permission denied", "No such file", str(f["work"])]:
                self.assertNotIn(needle, details)

            stored = points(f)
            self.assertTrue(stored)
            self.assertTrue(all(p["payload"]["source_file"].endswith("good.md") for p in stored))
            self.assertNotIn("Outside the root", json.dumps(stored))

    def test_unreadable_subtree_is_reported_not_hidden(self):
        with fixture() as f:
            root = f["work"] / "docs-locked"
            root.mkdir()
            (root / "good.md").write_text("# Good\n\nReadable body text here.\n")
            locked = root / "locked"
            locked.mkdir()
            (locked / "hidden.md").write_text("# Hidden\n\nSecret body.\n")
            locked.chmod(0o000)
            try:
                if os.access(locked, os.R_OK):
                    self.skipTest("read bypass in this environment")
                report = f["api"]("/memories/ingest", {"path": str(root)}, "POST")
            finally:
                locked.chmod(0o755)

            self.assertEqual(report["status"], "partial", report)
            self.assertEqual(report["stored"], 1)
            self.assertEqual(report["failed"], 1)
            self.assertEqual(issue(report, "locked")["outcome"], "failed")
            self.assertEqual(issue(report, "locked")["reason"], "unreadable")
            self.assertNotIn("Permission denied", json.dumps(report))

    def test_clean_tree_is_ingested_and_empty_tree_is_empty(self):
        with fixture() as f:
            root = f["work"] / "clean"
            root.mkdir()
            (root / "a.md").write_text("# A\n\nBody text one.\n")
            (root / "b.md").write_text("# B\n\nBody text two.\n")
            report = f["api"]("/memories/ingest", {"path": str(root)}, "POST")
            self.assertEqual(report["status"], "ingested", report)
            self.assertEqual(report["attempted"], 2)
            self.assertEqual(report["stored"], 2)
            self.assertEqual(report["failed"], 0)
            self.assertEqual(report["skipped"], 0)
            self.assertEqual(report["issues"], [])

            empty = f["work"] / "empty"
            empty.mkdir()
            report = f["api"]("/memories/ingest", {"path": str(empty)}, "POST")
            self.assertEqual(report["status"], "empty", report)
            self.assertEqual(report["stored"], 0)

    def test_missing_path_is_an_api_error(self):
        with fixture() as f:
            missing = f["work"] / "no-such-dir"
            with self.assertRaises(urllib.error.HTTPError) as caught:
                f["api"]("/memories/ingest", {"path": str(missing)}, "POST")
            self.assertEqual(caught.exception.code, 500)
            body = json.load(caught.exception)
            caught.exception.close()
            # The internal error text (including the requested path) stays server-side.
            self.assertEqual(body["error"], "Internal server error")


if __name__ == "__main__":
    unittest.main()
