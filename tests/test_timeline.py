"""Timeline contracts with disposable Qdrant and local provider mocks."""
from datetime import datetime
import unittest
import urllib.error
import uuid
from urllib.parse import urlencode

from test_consolidation import fixture, points
from test_compact_retrieval import COMPACT_KEYS, mcp


def uid(n):
    return str(uuid.UUID(int=n))


def seed(f, rows):
    original = f["api"]("/memories", {"content": "Timeline fixture"}, "POST")["id"]
    template = next(p for p in points(f) if p["id"] == original)
    f["q"]("/collections/memories/points/delete?wait=true", {"points": [original]}, "POST")
    for start in range(0, len(rows), 500):
        data = []
        for n, time, realm, visibility in rows[start:start + 500]:
            data.append({"id": uid(n), "vector": template["vector"], "payload": {
                **template["payload"], "id": uid(n), "ingested_at": time,
                "realm_name": realm, "realm_id": None, "visibility": visibility,
                "summary": "🦀" * 180, "content": "Timeline fixture " + str(n)}})
        f["q"]("/collections/memories/points?wait=true", {"points": data}, "PUT")


def timeline(f, **args):
    return f["api"]("/memories/timeline?" + urlencode(args))


class TimelineTests(unittest.TestCase):
    def assert_http(self, status, call):
        with self.assertRaises(urllib.error.HTTPError) as error:
            call()
        self.assertEqual(error.exception.code, status)
        error.exception.close()

    def test_chronology_pagination_scope_and_interfaces(self):
        with fixture() as f:
            rows = [(n, "2026-11-08T02:00:00+02:00", "A", "private") for n in range(1, 242)]
            rows += [(300, "2026-11-07T23:59:59Z", "A", "private"),
                     (301, "2026-11-08T00:00:01Z", "A", "private"),
                     (302, "2026-11-08T00:00:00Z", "B", "public")]
            seed(f, rows)
            index = f["q"]("/collections/memories")["result"]["payload_schema"]["ingested_at"]
            self.assertEqual(index["data_type"], "datetime")
            embeddings = f["state"]["embeddings"]
            result = timeline(f, id=uid(120), before=3, after=3, realm="A")
            self.assertEqual([r["id"] for r in result["results"]], [uid(n) for n in range(117, 124)])
            self.assertFalse(result["truncated"])
            self.assertEqual(result["anchor_at"], "2026-11-08T00:00:00Z")
            for row in result["results"]:
                self.assertEqual(set(row), COMPACT_KEYS)
                self.assertEqual(row["ingested_at"], result["anchor_at"])
                self.assertEqual(len(row["snippet"]), 160)
                self.assertEqual(row["score"], 0)
            # Nearest strict-side boundary contains >2 scroll pages of ties.
            later = timeline(f, id=uid(301), before=3, after=5, realm="A")
            self.assertEqual([r["id"] for r in later["results"]], [uid(239), uid(240), uid(241), uid(301)])
            earlier = timeline(f, id=uid(300), before=5, after=3, realm="A")
            self.assertEqual([r["id"] for r in earlier["results"]], [uid(300), uid(1), uid(2), uid(3)])
            timed = timeline(f, at="2026-11-08T02:00:00+02:00", before=5, after=2, realm="A")
            self.assertIsNone(timed["anchor_id"])
            self.assertEqual([r["id"] for r in timed["results"]], [uid(300), uid(1), uid(2)])
            only = timeline(f, id=uid(120), before=0, after=0)
            self.assertEqual([r["id"] for r in only["results"]], [uid(120)])
            self.assertEqual(timeline(f, at="2026-01-01T00:00:00Z", before=0, after=0)["results"], [])
            self.assertEqual(f["state"]["embeddings"], embeddings)
            self.assert_http(404, lambda: timeline(f, id=uid(120), realm="B"))
            self.assert_http(404, lambda: timeline(f, id=uid(120), visibility="public"))
            self.assert_http(404, lambda: timeline(f, id=uid(999)))
            for args in [{}, {"id": "bad"}, {"at": "bad"}, {"query": " "},
                         {"id": uid(120), "query": "a"}, {"id": uid(120), "before": 51},
                         {"id": uid(120), "after": -1}, {"id": uid(120), "visibility": "other"}]:
                self.assert_http(400, lambda args=args: timeline(f, **args))
            replies = mcp(f, [("memex8_timeline", {"id": uid(120), "before": 3, "after": 3, "realm": "A"}),
                              ("memex8_timeline", {"id": uid(120), "before": 51}),
                              ("memex8_timeline", {"id": uid(120), "query": "both"})])
            self.assertEqual(replies[0]["result"], result)
            self.assertIn("error", replies[1])
            self.assertIn("error", replies[2])
            # Query anchor is chosen inside the requested visibility/realm.
            queried = timeline(f, query="fixture", realm="B", visibility="public", before=0, after=0)
            self.assertEqual(queried["anchor_id"], uid(302))
            self.assertEqual(f["state"]["embeddings"], embeddings + 1)
            f["state"]["embedding_vectors"]["nonsense"] = [0., 1.] + [0.] * 62
            miss = timeline(f, query="nonsense", realm="A")
            self.assertTrue(miss["no_match"])
            self.assertEqual(miss["results"], [])
            self.assertIsNone(miss["anchor_at"])
            # Legacy corrupt anchor is explicit, never silently assigned a timestamp.
            f["q"]("/collections/memories/points/payload?wait=true",
                   {"points": [uid(120)], "payload": {"ingested_at": "invalid"}}, "POST")
            self.assert_http(409, lambda: timeline(f, id=uid(120)))

    def test_submicrosecond_timestamps_keep_true_chronology(self):
        with fixture() as f:
            # Qdrant indexes microseconds; sort by the original parsed timestamps.
            times = ["2026-11-08T00:00:00.000000100Z",
                     "2026-11-08T00:00:00.000000200Z",
                     "2026-11-08T00:00:00.000000300Z",
                     "2026-11-08T00:00:00.000001100Z"]
            seed(f, [(n, time, "A", "private") for n, time in zip([4, 3, 2, 1], times)])
            result = timeline(f, id=uid(3), before=2, after=2)
            self.assertEqual([r["id"] for r in result["results"]], [uid(4), uid(3), uid(2), uid(1)])
            result = timeline(f, at=times[1], before=2, after=2)
            self.assertEqual([r["id"] for r in result["results"]], [uid(4), uid(3), uid(2)])

    def test_work_limit_reports_partial_tie_scan(self):
        with fixture() as f:
            seed(f, [(n, "2026-11-08T00:00:00Z", "A", "private") for n in range(1, 10003)])
            result = timeline(f, id=uid(10002), before=5, after=5)
            self.assertTrue(result["truncated"])
            self.assertIn(uid(10002), [r["id"] for r in result["results"]])
            self.assertLessEqual(len(result["results"]), 11)
            keys = [(datetime.fromisoformat(r["ingested_at"].replace("Z", "+00:00")), r["id"])
                    for r in result["results"]]
            self.assertEqual(keys, sorted(keys))


if __name__ == "__main__":
    unittest.main()
