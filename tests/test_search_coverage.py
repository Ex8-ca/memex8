"""Coverage calibration, empty-page semantics and filtered realm hints with disposable Qdrant."""
import json
import math
import unittest
import uuid

from test_consolidation import fixture, canonical
from test_compact_retrieval import mcp


def vector(*values):
    return list(values) + [0.] * (64 - len(values))


class SearchCoverageTests(unittest.TestCase):
    def test_raw_coverage_filters_empty_pages_and_compressed_path(self):
        with fixture() as f:
            f["state"]["embedding_vectors"].update({
                "known fact": vector(1, 0, 0),
                "partial fact": vector(.45, 0, math.sqrt(1-.45**2)),
                "nonsense zyxw": vector(0, 0, 1),
            })
            def search(query="known fact", **options):
                return f["api"]("/memories/search", {"query": query, **options}, "POST")

            empty = search()["coverage"]
            self.assertEqual(empty["level"], "none")
            self.assertIsNone(empty["best_score"])
            self.assertEqual(empty["results_above_min"], 0)
            self.assertEqual(empty["suggested_realms"], [])
            realms = ["Alpha public", "Beta private", "Gamma public", "Orphan", "Invalid"]
            vectors = [vector(1, 0), vector(.3, math.sqrt(.91)), vector(.6, .8), vector(1, 0), vector(0, 0)]
            realm_ids = [str(uuid.uuid4()) for _ in realms]
            ids = []
            for i in range(3):
                id_ = f["api"]("/memories", {"content": f"Certified source {i}", "certified": True}, "POST")["id"]
                ids.append(id_)
                f["q"]("/collections/memories/points?wait=true", {"points": [{"id": id_, "vector": vectors[i],
                    "payload": {**f["api"]("/memories/" + id_), "realm_name": realms[i], "realm_id": realm_ids[i],
                                "visibility": "private" if i == 1 else "public", "tags": [f"scope:{i}"],
                                "upvotes": 20 if i == 0 else 0}}]}, "PUT")
            f["q"]("/collections/realms/points?wait=true", {"points": [
                {"id": id_, "vector": vec, "payload": {"name": name, "centroid": vec,
                 "memory_count": 999, "is_user_pinned": True}}
                for id_, vec, name in zip(realm_ids, vectors, realms)]}, "PUT")

            before = f["state"]["embeddings"]
            hit = search()
            self.assertEqual(f["state"]["embeddings"] - before, 1)  # reuse the query embedding
            coverage = hit["coverage"]
            self.assertEqual(coverage["level"], "high")
            self.assertAlmostEqual(coverage["best_score"], 1., places=5)
            self.assertEqual(coverage["results_above_min"], 3)
            self.assertEqual(coverage["suggested_realms"], [realms[0], realms[2], realms[1]])
            self.assertGreater(hit["results"][0]["score"], 1.)  # coverage is independent of boosts
            for options in [{"compact": True}, {"offset": 99}, {"limit": 1}]:
                response = search(**options)
                self.assertEqual(response["coverage"], coverage)
                if "offset" in options:
                    self.assertEqual(response["results"], [])
            cutoff = search(min_score=1.1)
            self.assertEqual(cutoff["results"], [])
            self.assertEqual(cutoff["coverage"]["level"], "high")
            self.assertEqual(cutoff["coverage"]["results_above_min"], 0)
            partial = search("partial fact", min_score=.5)
            self.assertEqual(partial["results"], [])
            self.assertEqual(partial["coverage"]["level"], "partial")
            self.assertAlmostEqual(partial["coverage"]["best_score"], .45, places=5)
            miss = search("nonsense zyxw")
            self.assertEqual(miss["results"], [])
            self.assertEqual(miss["coverage"]["level"], "none")
            self.assertAlmostEqual(miss["coverage"]["best_score"], 0., places=5)
            self.assertEqual(len(miss["coverage"]["suggested_realms"]), 3)
            self.assertIn("configured digest", miss["coverage"]["hint"])
            for options, expected in [({"visibility": "public"}, [realms[0], realms[2]]),
                                      ({"tags": ["scope:1"]}, [realms[1]]),
                                      ({"realm": realms[2]}, [realms[2]]),
                                      ({"tags": ["absent"]}, []),
                                      ({"visibility": "public", "tags": ["scope:1"]}, [])]:
                response = search(**options)
                self.assertEqual(response["coverage"]["suggested_realms"], expected)
                if not expected:
                    self.assertEqual(response["results"], [])
                    self.assertIsNone(response["coverage"]["best_score"])
            private = search(tags=["scope:1"])
            self.assertEqual(private["coverage"]["level"], "none")
            self.assertAlmostEqual(private["coverage"]["best_score"], .3, places=5)
            self.assertEqual(search(realm="missing")["coverage"]["level"], "none")
            get = f["api"]("/memories/search?query=known%20fact&compact=true")
            self.assertEqual(get["coverage"], coverage)

            # Build disk TurboVec; the serving engine stays on Qdrant while a new
            # stdio engine loads the compressed index. Compare the coverage verbatim.
            f["api"]("/slumber/trigger", {}, "POST")
            self.assertTrue((f["work"] / "test.tv").exists())
            calls = [("memex8_search", {"query": query, "compact": True}) for query in
                     ["known fact", "partial fact", "nonsense zyxw"]]
            for (name, args), reply in zip(calls, mcp(f, calls)):
                expected = search(**args)["coverage"]
                self.assertEqual(canonical(reply["result"]["coverage"]), canonical(expected))
                line = reply["result"]["content"][0]["text"].splitlines()[0]
                self.assertTrue(line.startswith("coverage=" + expected["level"]))
                self.assertIn("retrieval heuristic", line)

            # The count is bounded at 100, independent of result-page size.
            payload = f["api"]("/memories/" + ids[0])
            f["q"]("/collections/memories/points?wait=true", {"points": [
                {"id": str(uuid.uuid4()), "vector": vector(1, 0), "payload": {**payload, "tags": ["bulk"]}}
                for _ in range(105)]}, "PUT")
            bounded = search(tags=["bulk"], limit=1, offset=200)
            self.assertEqual(bounded["results"], [])
            self.assertEqual(bounded["coverage"]["results_above_min"], 100)
            # Missing realm data degrades hints only, not the raw signal or results.
            f["q"]("/collections/realms", method="DELETE")
            missing_centroids = search()
            self.assertEqual(missing_centroids["coverage"]["level"], "high")
            self.assertEqual(missing_centroids["coverage"]["suggested_realms"], [])
            self.assertTrue(missing_centroids["results"])


if __name__ == "__main__":
    unittest.main()
