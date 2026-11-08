"""Archived duplicate removal and recovery against isolated Qdrant/mock models."""
from datetime import datetime, timedelta, timezone
import json
import subprocess
import unittest
import urllib.error
import uuid

from test_consolidation import fixture, points, operation, write_operation
from test_certification import request


def seed(f, vectors=None):
    vectors = vectors or [[1., 0.], [1., .1], [1., -.1], [0., 1.]]
    ids = []
    for i, vector in enumerate(vectors):
        id_ = f["api"]("/memories", {"content": f"Duplicate fixture {i}", "memory_type": "fact"}, "POST")["id"]
        memory = f["api"]("/memories/" + id_)
        memory.pop("id")
        memory.update(realm_id=None, realm_name="Fixture", importance=.8, upvotes=20-i,
                      access_count=i+1, tags=[f"tag-{i}"], heading=f"heading-{i}")
        f["q"]("/collections/memories/points?wait=true", {"points": [{"id": id_,
                "payload": memory, "vector": vector + [0.] * (64-len(vector))}]}, "PUT")
        ids.append(id_)
    return ids


def run_slumber(f):
    f["api"]("/slumber/trigger", {}, "POST")
    return f["api"]("/slumber/status")["last_report"]


def journals(f):
    return [p["payload"]["operation"] for p in points(f, "consolidation_archive")
            if p["payload"]["operation"].get("kind") == "dedup"]


class DedupRemovalTests(unittest.TestCase):
    def assert_http(self, code, call):
        with self.assertRaises(urllib.error.HTTPError) as error:
            call()
        try:
            self.assertEqual(error.exception.code, code)
        finally:
            error.exception.close()

    def test_four_vectors_remove_two_merge_once_and_restore(self):
        with fixture(dedup_dry_run=False) as f:
            ids = seed(f)
            # Seed references to a soon-to-be-removed source, including a certified
            # association owner; reference metadata may be repaired on certified points.
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[3]],
                    "payload": {"related_memory_ids": [ids[1]], "association_strengths": [.99]}}, "POST")
            edge_id = str(uuid.uuid4())
            f["q"]("/collections/graph_edges/points?wait=true", {"points": [{"id": edge_id,
                    "vector": [1.]+[0.]*63, "payload": {"from_memory_id": ids[3],
                    "to_memory_id": ids[1], "relation_type": "related", "weight": .99}}]}, "PUT")
            before = points(f)
            # Inspection is read-only even when full-slumber removal is enabled.
            inspection = subprocess.run(f["command"] + ["slumber", "inspect-duplicates"],
                                        env=f["env"], cwd=f["work"], capture_output=True,
                                        text=True, check=True, timeout=30)
            inspected = json.loads(inspection.stdout)
            self.assertTrue(inspected["dry_run"])
            self.assertEqual(inspected["deduplicated"], 0)
            self.assertEqual(len(inspected["candidates"]), 2)
            self.assertEqual(points(f), before)
            report = run_slumber(f)
            self.assertFalse(report["dedup_dry_run"])
            self.assertEqual(report["deduplicated"], 2)
            self.assertEqual(report["dedup_candidates"], 2)
            self.assertEqual({p["id"] for p in points(f)}, {ids[0], ids[3]})
            self.assertFalse(points(f, "graph_edges"), "dangling graph edge survived")
            self.assertTrue(all(not (set(p["payload"].get("related_memory_ids", [])) & {ids[1], ids[2]})
                                for p in points(f)), "dangling association survived")
            winner = f["api"]("/memories/" + ids[0])
            self.assertEqual(winner["access_count"], 6)
            self.assertEqual(set(winner["tags"]), {"tag-0", "tag-1", "tag-2"})
            self.assertEqual(set(winner["consolidated_from"]), {ids[1], ids[2]})
            self.assertEqual(winner["derived_by"], "dedup")
            self.assertEqual(winner["content"], "Duplicate fixture 0")
            ops = journals(f)
            self.assertEqual(len(ops), 2)
            self.assertEqual({o["summary_id"] for o in ops}, set(report["dedup_operation_ids"]))
            self.assertTrue(all(o["state"] == "complete" for o in ops))
            self.assertTrue(all(len(o["sources"][0]["vector"]) == 64 for o in ops))
            self.assertEqual(run_slumber(f)["deduplicated"], 0)
            self.assertEqual(f["api"]("/memories/" + ids[0])["access_count"], 6)
            # Both search paths must exclude removed IDs after maintenance.
            for query in [{"query": "fixture", "limit": 50, "min_score": 0.},
                          {"query": "fixture", "limit": 50, "min_score": 0., "tags": ["tag-0"]}]:
                found = f["api"]("/memories/search", query, "POST")["results"]
                self.assertFalse({r["id"] for r in found} & {ids[1], ids[2]})
            # Search itself records accesses; recovery must retain those new accesses.
            expected_count = f["api"]("/memories/" + ids[0])["access_count"]
            op = next(o for o in ops if o["sources"][0]["id"] == ids[1])
            operation_id = op["summary_id"]
            self.assert_http(401, lambda: request(f["api_url"] + f"/dedup/{operation_id}/restore", method="POST"))
            self.assert_http(404, lambda: f["api"](f"/dedup/{uuid.uuid4()}/restore", method="POST"))
            self.assert_http(409, lambda: f["api"](f"/memories/{operation_id}/unconsolidate", method="POST"))
            f["state"]["fail_embedding"] = op["sources"][0]["content"]
            self.assert_http(500, lambda: f["api"](f"/dedup/{operation_id}/restore", method="POST"))
            self.assertEqual(operation(f, operation_id)["state"], "restoring")
            self.assertEqual(f["api"]("/memories/" + ids[0])["access_count"], expected_count)
            f["state"]["fail_embedding"] = None
            restored = f["api"](f"/dedup/{operation_id}/restore", method="POST")
            self.assertEqual(restored["restored"], 1)
            source = f["api"]("/memories/" + ids[1])
            self.assertEqual(source["tags"], ["tag-1"])
            self.assertEqual(source["access_count"], 2)
            # Restoration is visible immediately, before another index rebuild.
            found = f["api"]("/memories/search", {"query": "fixture", "limit": 50, "min_score": 0.}, "POST")
            self.assertIn(ids[1], {row["id"] for row in found["results"]})
            expected_count = f["api"]("/memories/" + ids[0])["access_count"]
            self.assertGreater(datetime.fromisoformat(source["no_dedup_until"]),
                               datetime.now(timezone.utc)+timedelta(days=29))
            self.assertEqual(run_slumber(f)["deduplicated"], 0)
            self.assertIn(ids[1], {p["id"] for p in points(f)})
            # Expire cooldowns and dedupe the restored identity again without counting it twice.
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[1]],
                    "payload": {"no_dedup_until": "2000-01-01T00:00:00Z"}}, "POST")
            new_source_accesses = f["api"]("/memories/" + ids[1])["access_count"] - 2
            self.assertEqual(run_slumber(f)["deduplicated"], 1)
            expected_count += new_source_accesses
            self.assertEqual(f["api"]("/memories/" + ids[0])["access_count"], expected_count)
            # A completed recovery retry must not recreate a subsequently removed ID.
            self.assertTrue(f["api"](f"/dedup/{operation_id}/restore", method="POST")["already_restored"])
            self.assertNotIn(ids[1], {p["id"] for p in points(f)})

            # Backups preserve dedup journals and are independently recoverable.
            backup_dir = f["work"] / "backups"
            subprocess.run(f["command"] + ["backup", str(backup_dir)], env=f["env"], cwd=f["work"],
                           capture_output=True, text=True, check=True, timeout=60)
            backup = str(next(backup_dir.glob("*.tar.gz")))
            with fixture() as recovery:
                subprocess.run(recovery["command"] + ["restore", backup, "--force"], env=recovery["env"],
                               cwd=recovery["work"], capture_output=True, text=True, check=True, timeout=60)
                another = next(o for o in ops if o["sources"][0]["id"] == ids[2])
                self.assertEqual(recovery["api"](f"/dedup/{another['summary_id']}/restore", method="POST")["restored"], 1)
                self.assertEqual(recovery["api"]("/memories/" + ids[0])["access_count"], expected_count)
            calls = [{"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                     {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                         "name": "memex8_restore_duplicate", "arguments": {"operation_id": operation_id}}}]
            response = subprocess.run(f["command"] + ["mcp"], input="\n".join(map(json.dumps, calls))+"\n",
                                      env=f["env"], cwd=f["work"], capture_output=True, text=True, check=True, timeout=60)
            replies = [json.loads(line) for line in response.stdout.splitlines()]
            self.assertIn("memex8_restore_duplicate", {t["name"] for t in replies[0]["result"]["tools"]})
            self.assertTrue(json.loads(replies[1]["result"]["content"][0]["text"])["already_restored"])

    def test_archive_failure_has_no_merge_or_removal(self):
        with fixture(dedup_dry_run=False, archive_dimensions=2) as f:
            seed(f, [[1., 0.], [1., .1]])
            before = points(f)
            self.assert_http(500, lambda: run_slumber(f))
            self.assertEqual(points(f), before)
            self.assertEqual(journals(f), [])
            self.assertEqual(f["api"]("/slumber/status")["state"], "idle")

    def test_partial_deletion_failure_retries_without_double_counting(self):
        with fixture(dedup_dry_run=False) as f:
            ids = seed(f, [[1., 0.], [1., .1]])
            # Inject a deletion-filter mismatch. Typed legacy reading sees private,
            # but the Qdrant deletion requires that payload value to be present.
            f["q"]("/collections/memories/points/payload/delete?wait=true", {
                "points": [ids[1]], "keys": ["visibility"]}, "POST")
            self.assert_http(409, lambda: run_slumber(f))
            self.assertEqual({p["id"] for p in points(f)}, set(ids))
            winner = f["api"]("/memories/" + ids[0])
            self.assertEqual(winner["access_count"], 3)
            op = journals(f)[0]
            self.assertEqual(op["state"], "prepared")
            self.assertIn(op["summary_id"], winner["dedup_applied_operations"])
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[1]],
                    "payload": {"visibility": "private"}}, "POST")
            self.assertEqual(run_slumber(f)["deduplicated"], 1)
            self.assertEqual(f["api"]("/memories/" + ids[0])["access_count"], 3)
            self.assertEqual(operation(f, op["summary_id"])["state"], "complete")
            # Simulate interruption after deletion, before the final journal write.
            op = operation(f, op["summary_id"])
            op["state"] = "prepared"
            write_operation(f, op)
            self.assertEqual(run_slumber(f)["deduplicated"], 0)
            self.assertEqual(operation(f, op["summary_id"])["state"], "complete")
            self.assertEqual(f["api"]("/memories/" + ids[0])["access_count"], 3)

    def test_pending_operation_rechecks_vectors_and_certification(self):
        for change in ["vectors", "certification"]:
            with self.subTest(change=change), fixture(dedup_dry_run=False) as f:
                ids = seed(f, [[1., 0.], [1., .1]])
                f["q"]("/collections/memories/points/payload/delete?wait=true", {
                    "points": [ids[1]], "keys": ["visibility"]}, "POST")
                self.assert_http(409, lambda: run_slumber(f))
                op = journals(f)[0]
                f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[1]],
                        "payload": {"visibility": "private"}}, "POST")
                if change == "vectors":
                    f["q"]("/collections/memories/points/vectors?wait=true", {"points": [{
                        "id": ids[1], "vector": [0., 1.]+[0.]*62}]}, "PUT")
                else:
                    f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[1]],
                            "payload": {"content": "Newer corrected source"}}, "POST")
                    for id_ in ids:
                        f["api"](f"/memories/{id_}/certify", {"note": "confirmed during interruption"}, "POST")
                report = run_slumber(f)
                self.assertEqual(report["deduplicated"], 0)
                self.assertEqual(report["dedup_skipped"], 1)
                self.assertEqual(operation(f, op["summary_id"])["state"], "cancelled")
                self.assertEqual({p["id"] for p in points(f)}, set(ids))
                if change == "certification":
                    retained = f["api"](f"/dedup/{op['summary_id']}/restore", method="POST")
                    self.assertEqual(retained["retained"], 1)
                    loser = f["api"]("/memories/"+ids[1])
                    self.assertEqual(loser["content"], "Newer corrected source")
                    self.assertEqual(loser["verification_status"], "certified")
                    self.assertEqual(loser["certification_note"], "confirmed during interruption")

    def test_certified_winner_keeps_content_importance_and_certification(self):
        with fixture(dedup_dry_run=False) as f:
            ids = seed(f, [[1., 0.], [1., .1]])
            certified = f["api"](f"/memories/{ids[1]}/certify", {"by": "marc", "note": "confirmed"}, "POST")
            self.assertEqual(run_slumber(f)["deduplicated"], 1)
            self.assertEqual({p["id"] for p in points(f)}, {ids[1]})
            winner = f["api"]("/memories/"+ids[1])
            for field in ["content", "importance", "verification_status", "certified_at", "certified_by", "certification_note"]:
                self.assertEqual(winner[field], certified[field])
            self.assertEqual(winner["access_count"], 3)
            self.assertEqual(set(winner["tags"]), {"tag-0", "tag-1"})

    def test_consolidation_undo_survives_dedup_ancestry_and_prevents_immediate_rededup(self):
        with fixture(dedup_dry_run=False) as f:
            ids = seed(f, [[1., 0.], [.9, .435889894]])
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summary = next(p for p in points(f) if p["payload"].get("chunk_type") == "consolidated")
            summary_id = summary["id"]
            extra = f["api"]("/memories", {"content": "A duplicate of the consolidated summary",
                              "memory_type": "fact"}, "POST")["id"]
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [summary_id],
                    "payload": {"upvotes": 100}}, "POST")
            self.assertEqual(run_slumber(f)["deduplicated"], 1)
            winner = f["api"]("/memories/"+summary_id)
            self.assertEqual(set(winner["consolidated_from"]), set(ids+[extra]))
            self.assertEqual(winner["derived_by"], "consolidation")
            self.assertEqual(f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")["restored"], 2)
            self.assertEqual(run_slumber(f)["deduplicated"], 0)
            self.assertEqual({p["id"] for p in points(f)}, set(ids))
            journal = journals(f)[0]
            self.assertEqual(f["api"](f"/dedup/{journal['summary_id']}/restore", method="POST")["restored"], 1)
            self.assertEqual({p["id"] for p in points(f)}, set(ids+[extra]))

    def test_certification_scope_type_and_default_dry_run(self):
        with fixture() as f:
            ids = seed(f)
            report = run_slumber(f)
            self.assertTrue(report["dedup_dry_run"])
            self.assertEqual(report["deduplicated"], 0)
            self.assertEqual(journals(f), [])
            self.assertEqual({p["id"] for p in points(f)}, set(ids))
        with fixture(dedup_dry_run=False) as f:
            ids = seed(f, [[1., 0.]]*6)
            for id_ in ids[:2]:
                f["api"](f"/memories/{id_}/certify", {}, "POST")
            for id_, patch in zip(ids[2:], [{"memory_type": "profile"}, {"memory_type": "preference"},
                                            {"tags": ["project:one"]}, {"visibility": "public"}]):
                f["q"]("/collections/memories/points/payload?wait=true",
                        {"points": [id_], "payload": patch}, "POST")
            self.assertEqual(run_slumber(f)["deduplicated"], 0)
            self.assertEqual({p["id"] for p in points(f)}, set(ids))
            for id_ in ids[:2]:
                self.assertEqual(f["api"]("/memories/"+id_)["verification_status"], "certified")


if __name__ == "__main__":
    unittest.main()
