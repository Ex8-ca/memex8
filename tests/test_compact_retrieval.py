"""Compact retrieval contracts using disposable Qdrant and mock embeddings only."""
import json
import subprocess
import unittest
import urllib.error
import uuid

from test_consolidation import fixture
from test_certification import request

COMPACT_KEYS = {"id", "snippet", "realm_name", "score", "ingested_at", "memory_type", "verification_status"}
FULL_KEYS = {"id", "content", "heading", "realm_name", "importance", "score", "last_accessed",
             "access_count", "upvotes", "related_memory_ids", "association_strengths"}


def mcp(f, calls):
    messages = [{"jsonrpc": "2.0", "id": i + 1, "method": "tools/call",
                 "params": {"name": name, "arguments": args}}
                for i, (name, args) in enumerate(calls)]
    process = subprocess.run(f["command"] + ["mcp"], input="\n".join(map(json.dumps, messages)) + "\n",
                             env=f["env"], cwd=f["work"], capture_output=True, text=True,
                             check=True, timeout=60)
    return [json.loads(line) for line in process.stdout.splitlines()]


class CompactRetrievalTests(unittest.TestCase):
    def assert_http(self, code, call):
        with self.assertRaises(urllib.error.HTTPError) as error:
            call()
        self.assertEqual(error.exception.code, code)
        error.exception.close()

    def test_rest_search_recall_batch_and_both_index_paths(self):
        with fixture() as f:
            contents = [("Retrieval context about this project. " * 100) + str(i) for i in range(3)]
            ids = [f["api"]("/memories", {"content": content, "tags": ["retrieval"],
                    "memory_type": "fact", "certified": True}, "POST")["id"] for content in contents]
            snippets = [("🦀界é" * 100)[:160], contents[1][:160], contents[2][:160]]
            # Exercise summary preference, missing/blank summaries, metadata, and full payload retention.
            for i, id_ in enumerate(ids):
                f["q"]("/collections/memories/points/payload?wait=true", {"points": [id_], "payload": {
                    "summary": "🦀界é" * 100 if i == 0 else (None if i == 1 else "  \n"),
                    "ingested_at": f"2026-11-0{i + 1}T00:00:00Z", "source_file": f"context-{i}.md",
                    "consolidated_from": ["source-a"], "access_count": 3 - i,
                    "importance": .9 - .1 * i,
                }}, "POST")
            query = {"query": "project context", "min_score": 0, "limit": 3}
            full = f["api"]("/memories/search", query, "POST")
            compact = f["api"]("/memories/search", {**query, "compact": True}, "POST")
            self.assertEqual(len(full["results"]), 3)
            self.assertEqual({r["id"] for r in compact["results"]}, set(ids))
            self.assertTrue(all(set(r) == FULL_KEYS for r in full["results"]))
            self.assertEqual(set(full), {"results", "total", "limit", "offset", "coverage"})
            for row in compact["results"]:
                self.assertEqual(set(row), COMPACT_KEYS)
                self.assertEqual(row["snippet"], snippets[ids.index(row["id"])])
                self.assertEqual(row["verification_status"], "certified")
                self.assertEqual(row["memory_type"], "fact")
            sizes = [len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())
                     for value in [full, compact]]
            print(f"Compact fixture response bytes: full={sizes[0]}, compact={sizes[1]} ({100*sizes[1]/sizes[0]:.1f}%)")
            self.assertLessEqual(sizes[1], .25 * sizes[0])
            tagged = f["api"]("/memories/search", {**query, "tags": ["retrieval"], "compact": True}, "POST")
            self.assertEqual({r["id"] for r in tagged["results"]}, set(ids))
            get_search = f["api"]("/memories/search?query=context&compact=true&min_score=0&limit=3")
            self.assertTrue(all(set(r) == COMPACT_KEYS for r in get_search["results"]))
            self.assertEqual(f["api"]("/memories/search", {**query, "realm": "absent", "compact": True}, "POST")["results"], [])
            self.assertTrue(all(set(r) == FULL_KEYS for r in f["api"]("/memories/recall")["results"]))
            recall = f["api"]("/memories/recall?compact=true&limit=1&offset=1")
            self.assertEqual(len(recall["results"]), 1)
            self.assertEqual(set(recall["results"][0]), COMPACT_KEYS)
            missing = str(uuid.uuid4())
            ordered = [ids[2], ids[0], missing, ids[1], ids[2], "not-an-id"]
            batch = f["api"]("/memories/batch-get", {"ids": ordered}, "POST")
            self.assertEqual([m["id"] for m in batch["memories"]], [ids[2], ids[0], ids[1], ids[2]])
            self.assertEqual(batch["not_found"], [missing, "not-an-id"])
            self.assertEqual(batch["memories"][1]["content"], contents[0])
            self.assertEqual(batch["memories"][1]["consolidated_from"], ["source-a"])
            self.assertEqual(batch["memories"][0]["source_file"], "context-2.md")
            self.assertEqual(len(f["api"]("/memories/batch-get", {"ids": [ids[0]] * 50}, "POST")["memories"]), 50)
            for invalid in [[], [ids[0]] * 51]:
                self.assert_http(400, lambda: f["api"]("/memories/batch-get", {"ids": invalid}, "POST"))
            self.assert_http(401, lambda: request(f["api_url"] + "/memories/batch-get", {"ids": ids}, "POST"))
            self.assert_http(401, lambda: request(f["api_url"] + "/memories/search?query=context&compact=true"))
            # Slumber rebuilds a disk TurboVec index; a fresh stdio process loads it.
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertTrue((f["work"] / "test.tv").exists())
            replies = mcp(f, [
                ("memex8_search", {**query, "compact": True}),
                ("memex8_recall", {"compact": True, "limit": 3}),
                ("memex8_get", {"ids": ordered}),
                ("memex8_get", {"memory_id": ids[0]}),
                ("memex8_get", {"id": ids[1]}),
                ("memex8_get", {"id": ids[0], "ids": ids}),
                ("memex8_search", {"query": "context", "compact": "false"}),
                ("memex8_search", query),
            ])
            for response in replies[:2]:
                self.assertEqual({r["id"] for r in response["result"]["results"]}, set(ids))
                self.assertTrue(all(set(r) == COMPACT_KEYS for r in response["result"]["results"]))
            self.assertEqual([m["id"] for m in replies[2]["result"]["memories"]], [ids[2], ids[0], ids[1], ids[2]])
            self.assertEqual(replies[2]["result"]["not_found"], [missing, "not-an-id"])
            self.assertEqual(replies[3]["result"]["memory"]["content"], contents[0])
            self.assertEqual(replies[4]["result"]["memory"]["id"], ids[1])
            self.assertIn("error", replies[5])
            self.assertIn("error", replies[6])
            self.assertTrue(all("content" in r and "snippet" not in r for r in replies[7]["result"]["results"]))


if __name__ == "__main__":
    unittest.main()
