"""Exercise the real CLI against disposable Qdrant, never the running service.

Run after cargo build: python3 tests/test_dedup_inspection.py
Requires Docker and the locally available qdrant/qdrant image.
"""
import json
import os
from pathlib import Path
import subprocess
import time
import unittest
import urllib.request
import uuid


class DuplicateInspectionTest(unittest.TestCase):
    def test_inspection_does_not_mutate_qdrant(self):
        container = subprocess.check_output([
            "docker", "run", "--detach", "--rm",
            "--publish", "127.0.0.1::6333", "--publish", "127.0.0.1::6334",
            "--tmpfs", "/qdrant/storage", "qdrant/qdrant:latest",
        ], text=True).strip()
        try:
            ports = json.loads(subprocess.check_output([
                "docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container
            ], text=True))
            http_url = "http://127.0.0.1:" + ports["6333/tcp"][0]["HostPort"]
            grpc_url = "http://127.0.0.1:" + ports["6334/tcp"][0]["HostPort"]

            def request(path, body=None, method="GET"):
                data = None if body is None else json.dumps(body).encode()
                req = urllib.request.Request(http_url + path, data=data, method=method,
                                             headers={"Content-Type": "application/json"})
                with urllib.request.urlopen(req, timeout=10) as response:
                    return json.load(response)

            for attempt in range(100):
                try:
                    with urllib.request.urlopen(http_url + "/readyz", timeout=2) as response:
                        response.read()
                    break
                except (OSError, ValueError):
                    if attempt == 99:
                        raise
                    time.sleep(0.1)

            request("/collections/memories", {"vectors": {"size": 2, "distance": "Cosine"}}, "PUT")
            ids = [str(uuid.UUID(int=i)) for i in range(1, 5)]
            points = [{
                "id": id_, "vector": vector,
                "payload": {"content": f"Fixture {i}", "realm_id": "r1", "realm_name": "Fixture",
                            "memory_type": "event", "source_hash": "same-file",
                            "importance": 1.0, "ingested_at": "2026-11-01T00:00:00Z"},
            } for i, (id_, vector) in enumerate(zip(ids, [[1, 0], [1, .1], [1, -.1], [0, 1]]))]
            request("/collections/memories/points?wait=true", {"points": points}, "PUT")

            def snapshot():
                return request("/collections/memories/points/scroll", {
                    "limit": 100, "with_payload": True, "with_vector": True,
                }, "POST")["result"]["points"]

            before = snapshot()
            repo = Path(__file__).resolve().parents[1]
            result = subprocess.run([
                str(repo / "target/debug/memex8"), "--config", "/nonexistent-memex8-test-config",
                "slumber", "inspect-duplicates",
            ], env={**os.environ, "QDRANT_URL": grpc_url}, capture_output=True, text=True, check=True)
            report = json.loads(result.stdout)
            self.assertTrue(report["dry_run"])
            self.assertEqual(report["deduplicated"], 0)
            self.assertEqual(report["memories_scanned"], 4)
            self.assertEqual({p["loser_id"] for p in report["candidates"]}, set(ids[1:3]))
            self.assertTrue(all(p["winner_id"] == ids[0] for p in report["candidates"]))
            self.assertEqual(snapshot(), before, "inspection changed payloads or vectors")
            collections = request("/collections")["result"]["collections"]
            self.assertEqual([c["name"] for c in collections], ["memories"],
                             "inspection must not initialize other collections")
        finally:
            subprocess.run(["docker", "stop", container], check=True, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    unittest.main()
