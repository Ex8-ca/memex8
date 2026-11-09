"""Recoverable consolidation against disposable Qdrant and a local mock LLM.

Run after cargo build: python3 tests/test_consolidation.py
No production services, files, models, or credentials are used.
"""
from contextlib import contextmanager
from datetime import datetime, timezone, timedelta
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import tarfile
import unittest
import urllib.error
import uuid

from test_certification import request, wait_ready


@contextmanager
def fixture(preserve=False, archive_dimensions=1, dedup_dry_run=True):
    repo = Path(__file__).resolve().parents[1]
    binary = os.environ.get("MEMEX8_TEST_BINARY", str(repo / "target/debug/memex8"))
    container = subprocess.check_output([
        "docker", "run", "--detach", "--rm", "--publish", "127.0.0.1::6333",
        "--publish", "127.0.0.1::6334", "--tmpfs", "/qdrant/storage", "qdrant/qdrant:latest",
    ], text=True).strip()
    process = mock_server = None
    try:
        ports = json.loads(subprocess.check_output([
            "docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container,
        ], text=True))
        qhttp = "http://127.0.0.1:" + ports["6333/tcp"][0]["HostPort"]
        qgrpc = "http://127.0.0.1:" + ports["6334/tcp"][0]["HostPort"]
        wait_ready(qhttp + "/readyz")
        request(qhttp + "/collections/consolidation_archive",
                {"vectors": {"size": archive_dimensions, "distance": "Dot"}}, "PUT")
        state = {"fail_embedding": None, "summaries": 0, "embeddings": 0, "embedding_vectors": {}}

        class MockLLM(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                status = 200
                if self.path.endswith("/embeddings"):
                    state["embeddings"] += 1
                    inputs = body["input"]
                    if isinstance(inputs, str):
                        inputs = [inputs]
                    if state["fail_embedding"] in inputs:
                        status, response = 500, {"error": "injected restore embedding failure"}
                    else:
                        response = {"data": [{"index": i, "embedding": state["embedding_vectors"].get(text, [1.] + [0.] * 63)}
                                             for i, text in enumerate(inputs)]}
                else:
                    state["summaries"] += 1
                    response = {"choices": [{"message": {"content": f"Consolidation fixture summary {state['summaries']}."}}]}
                data = json.dumps(response).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        mock_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), MockLLM)
        threading.Thread(target=mock_server.serve_forever, daemon=True).start()
        llm_url = "http://127.0.0.1:" + str(mock_server.server_port)
        with tempfile.TemporaryDirectory(prefix="memex8-consolidation-") as directory:
            work = Path(directory)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            api = f"http://127.0.0.1:{port}/api/v1"
            env = {**os.environ, "QDRANT_URL": qgrpc, "OPENAI_API_KEY": "test-only",
                   "EMBEDDING_PROVIDER": "openai", "EMBEDDING_DIMENSIONS": "64",
                   "OPENAI_BASE_URL": llm_url + "/v1", "LOCAL_LLM_URL": llm_url,
                   "MEMEX8_API_KEY": "test-only", "LOCAL_LLM_API_KEY": "test-only",
                   "TURBOVEC_INDEX_PATH": str(work / "test.tv"),
                   "TURBOVEC_ID_MAP_PATH": str(work / "test-ids.json")}
            config = subprocess.check_output([binary, "--config", str(work / "missing"), "config-show"],
                                             env=env, text=True, stderr=subprocess.DEVNULL)
            config = config.replace('base_url = "https://api.openai.com/v1"',
                                    'base_url = ' + json.dumps(llm_url + "/v1"))
            config = config.replace('backend = "openai"', 'backend = "local"')
            config = config.replace('[memex8_md]\nenabled = true', '[memex8_md]\nenabled = false')
            config = config.replace('[digest_md]\nenabled = true', '[digest_md]\nenabled = false')
            config = config.replace('[verification]\nenabled = true', '[verification]\nenabled = false')
            assert '[memex8_md]\nenabled = false' in config
            assert '[digest_md]\nenabled = false' in config
            assert '[verification]\nenabled = false' in config
            config = config.replace('dedup_dry_run = true', 'dedup_dry_run = ' + str(dedup_dry_run).lower())
            config = config.replace('preserve_originals = true', 'preserve_originals = ' + str(preserve).lower())
            config = config.replace('idle_timeout = "10m"', 'idle_timeout = "24h"')
            config_path = work / "config.toml"
            config_path.write_text(config)
            command = [binary, "--config", str(config_path)]
            with (work / "server.log").open("w+") as log:
                process = subprocess.Popen(command + ["serve", "--host", "127.0.0.1", "--port", str(port)],
                                           env=env, cwd=directory, stdout=log, stderr=log)
                try:
                    wait_ready(f"http://127.0.0.1:{port}/health")
                    yield {
                        "api": lambda path, body=None, method="GET": request(api + path, body, method, "test-only"),
                        "q": lambda path, body=None, method="GET": request(qhttp + path, body, method),
                        "state": state, "command": command, "env": env, "work": work, "api_url": api,
                    }
                except Exception:
                    log.seek(0)
                    print(log.read()[-6000:])
                    raise
    finally:
        if process is not None:
            process.terminate()
            process.wait(timeout=10)
        if mock_server is not None:
            mock_server.shutdown()
            mock_server.server_close()
        subprocess.run(["docker", "stop", container], check=True, stdout=subprocess.DEVNULL)


def canonical(value):
    # Qdrant JSON and serde's direct f32 JSON renderer choose different precision.
    if isinstance(value, float):
        return round(value, 6)
    if isinstance(value, list):
        return [canonical(v) for v in value]
    if isinstance(value, dict):
        return {k: canonical(v) for k, v in value.items()}
    return value


def points(f, collection="memories"):
    return f["q"](f"/collections/{collection}/points/scroll", {
        "limit": 100, "with_payload": True, "with_vector": True,
    }, "POST")["result"]["points"]


def seed_sources(f):
    ids = [f["api"]("/memories", {"content": text, "memory_type": "fact",
                             "tags": ["restore-fixture"], "visibility": "private"}, "POST")["id"]
           for text in ["Original source alpha", "Original source beta"]]
    # Stable explicit fixture payloads make full metadata preservation observable.
    for i, id_ in enumerate(ids):
        f["q"]("/collections/memories/points/payload?wait=true", {"points": [id_], "payload": {
            "realm_name": "Recovery fixture", "realm_id": None, "upvotes": i + 1,
            "importance": .8, "access_count": 10 + i, "source_file": f"source-{i}.md",
            "summary": "Original snippet", "heading": "Source heading", "reaction_score": .5,
            "last_verified": "2026-11-01T00:00:00Z", "verification_confidence": .9,
            "verification_status": "verified", "quantized_bit_width": 4.,
        }}, "POST")
    return ids


def operation(f, id_):
    return f["q"]("/collections/consolidation_archive/points/" + id_)["result"]["payload"]["operation"]


def write_operation(f, op):
    f["q"]("/collections/consolidation_archive/points/payload?wait=true",
           {"points": [op["summary_id"]], "payload": {"operation": op}}, "POST")


class ConsolidationTests(unittest.TestCase):
    def assert_http(self, code, call):
        with self.assertRaises(urllib.error.HTTPError) as error:
            call()
        self.assertEqual(error.exception.code, code)
        error.exception.close()

    def test_archive_restore_retry_cooldown_and_contracts(self):
        with fixture() as f:
            ids = seed_sources(f)
            self.assert_http(401, lambda: request(f["api_url"] + f"/memories/{ids[0]}/unconsolidate", method="POST"))
            self.assert_http(404, lambda: f["api"](f"/memories/{uuid.uuid4()}/unconsolidate", method="POST"))
            self.assert_http(409, lambda: f["api"](f"/memories/{ids[0]}/unconsolidate", method="POST"))
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summaries = [p for p in points(f) if p["payload"].get("chunk_type") == "consolidated"]
            self.assertEqual(len(summaries), 1)
            summary_id = summaries[0]["id"]
            summary = f["api"]("/memories/" + summary_id)
            self.assertEqual(set(summary["consolidated_from"]), set(ids))
            self.assertEqual(summary["derived_by"], "consolidation")
            self.assertIsNotNone(summary["consolidated_at"])
            op = operation(f, summary_id)
            self.assertEqual(op["state"], "complete")
            self.assertEqual(op["embedding_dimensions"], 64)
            self.assertEqual(op["embedding_provider"], "openai")
            self.assertEqual({s["id"] for s in op["sources"]}, set(ids))
            self.assertTrue(all(len(s["vector"]) == 64 for s in op["sources"]))
            self.assertTrue(all(p["id"] not in ids for p in points(f)))
            # Existing CLI backups must carry the durable sources and operation state.
            backup_dir = f["work"] / "backups"
            subprocess.run(f["command"] + ["backup", str(backup_dir)], env=f["env"],
                           cwd=f["work"], capture_output=True, text=True, check=True, timeout=60)
            with tarfile.open(next(backup_dir.glob("*.tar.gz"))) as archive:
                member = next(m for m in archive.getmembers() if m.name.endswith("consolidations.json"))
                with archive.extractfile(member) as data:
                    exported = json.load(data)
            self.assertEqual(exported[0]["summary_id"], summary_id)
            self.assertEqual(canonical(exported[0]["sources"]), canonical(op["sources"]))
            backup_path = str(next(backup_dir.glob("*.tar.gz")))
            with fixture() as recovery:
                subprocess.run(recovery["command"] + ["restore", backup_path, "--force"],
                               env=recovery["env"], cwd=recovery["work"], capture_output=True,
                               text=True, check=True, timeout=60)
                self.assertEqual(operation(recovery, summary_id)["sources"], op["sources"])
                undone = recovery["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
                self.assertEqual(undone["restored"], 2)
                # An older backup must not downgrade a completed recovery journal.
                retry_backup = subprocess.run(recovery["command"] + ["restore", backup_path, "--force"],
                                              env=recovery["env"], cwd=recovery["work"], capture_output=True,
                                              text=True, timeout=60)
                self.assertNotEqual(retry_backup.returncode, 0)
                self.assertEqual({p["id"] for p in points(recovery)}, set(ids))

            # A certified summary must be explicitly revoked before undo.
            f["api"](f"/memories/{summary_id}/certify", {}, "POST")
            self.assert_http(409, lambda: f["api"](f"/memories/{summary_id}/unconsolidate", method="POST"))
            f["api"](f"/memories/{summary_id}/certify", method="DELETE")

            # Fail after the first restore. Summary and archive remain, with retry state.
            f["state"]["fail_embedding"] = op["sources"][1]["content"]
            self.assert_http(500, lambda: f["api"](f"/memories/{summary_id}/unconsolidate", method="POST"))
            self.assertEqual(operation(f, summary_id)["state"], "restoring")
            self.assertIsNotNone(f["api"]("/memories/" + summary_id))
            self.assertEqual(len([p for p in points(f) if p["id"] in ids]), 1)
            # A newer edit between attempts must survive retry.
            first_id = op["sources"][0]["id"]
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [first_id],
                   "payload": {"content": "Newer user edit", "tags": ["newer-edit"]}}, "POST")
            f["state"]["fail_embedding"] = None
            # A delayed retry still grants newly restored points a full 30 days.
            interrupted = operation(f, summary_id)
            interrupted["cooldown_until"] = "2000-01-01T00:00:00Z"
            write_operation(f, interrupted)
            result = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertEqual((result["restored"], result["retained"]), (1, 1))
            self.assertEqual(operation(f, summary_id)["state"], "restored")
            self.assert_http(404, lambda: f["api"]("/memories/" + summary_id))
            for snapshot in op["sources"]:
                restored = f["api"]("/memories/" + snapshot["id"])
                self.assertGreater(datetime.fromisoformat(restored["no_consolidate_until"]),
                                   datetime.now(timezone.utc) + timedelta(days=29))
                if snapshot["id"] == first_id:
                    self.assertEqual(restored["content"], "Newer user edit")
                    self.assertEqual(restored["tags"], ["newer-edit"])
                else:
                    expected = {k: v for k, v in snapshot.items() if k not in {"vector", "no_consolidate_until", "no_dedup_until"}}
                    actual = {k: v for k, v in restored.items() if k not in {"no_consolidate_until", "no_dedup_until"}}
                    for field in ["importance", "verification_confidence", "reaction_score", "quantized_bit_width"]:
                        self.assertAlmostEqual(actual.pop(field), expected.pop(field), places=6)
                    self.assertEqual(actual, expected)
            found = f["api"]("/memories/search", {"query": "Original source", "min_score": 0., "limit": 50}, "POST")
            self.assertEqual({p["id"] for p in found["results"]}, set(ids), "restored sources hidden by stale index")
            before = points(f)
            retry = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertTrue(retry["already_restored"])
            self.assertEqual(points(f), before)
            summaries_before = f["state"]["summaries"]
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            self.assertEqual(f["state"]["summaries"], summaries_before, "cooldown ignored")
            self.assertEqual({p["id"] for p in points(f)}, set(ids))

            # Completed undo retries must not resurrect later intentional deletions.
            f["api"]("/memories/" + ids[0], method="DELETE")
            self.assertTrue(f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")["already_restored"])
            self.assertEqual({p["id"] for p in points(f)}, {ids[1]})

            # Actual stdio tool schema and dispatch, including idempotent retry.
            calls = [{"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                     {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                         "name": "memex8_unconsolidate", "arguments": {"memory_id": summary_id}}}]
            response = subprocess.run(f["command"] + ["mcp"], input="\n".join(map(json.dumps, calls)) + "\n",
                                      text=True, capture_output=True, env=f["env"], cwd=f["work"], check=True, timeout=60)
            replies = [json.loads(line) for line in response.stdout.splitlines()]
            self.assertIn("memex8_unconsolidate", {t["name"] for t in replies[0]["result"]["tools"]})
            self.assertTrue(json.loads(replies[1]["result"]["content"][0]["text"])["already_restored"])

    def test_preserved_originals_and_missing_legacy_sources(self):
        with fixture(preserve=True) as f:
            ids = seed_sources(f)
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summary_id = next(p["id"] for p in points(f) if p["payload"].get("chunk_type") == "consolidated")
            self.assertTrue(operation(f, summary_id)["preserve_originals"])
            # Package C archives had neither of these fields; defaults retain undo.
            legacy_journal = operation(f, summary_id)
            legacy_journal.pop("kind", None)
            legacy_journal.pop("dedup_winner", None)
            write_operation(f, legacy_journal)
            # Refuse to discard an edited summary.
            original_summary = f["api"]("/memories/" + summary_id)["content"]
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [summary_id],
                   "payload": {"content": "Manually corrected summary"}}, "POST")
            self.assert_http(409, lambda: f["api"](f"/memories/{summary_id}/unconsolidate", method="POST"))
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [summary_id],
                   "payload": {"content": original_summary}}, "POST")
            # Current original may be certified/edited after consolidation; undo retains it.
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[0]],
                   "payload": {"content": "Updated original"}}, "POST")
            f["api"](f"/memories/{ids[0]}/certify", {"note": "new confirmation"}, "POST")
            result = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertEqual((result["restored"], result["retained"]), (0, 2))
            updated = f["api"]("/memories/" + ids[0])
            self.assertEqual(updated["content"], "Updated original")
            self.assertEqual(updated["verification_status"], "certified")
            self.assertEqual(updated["certification_note"], "new confirmation")
            # Legacy provenance without an archive must not invent lost sources.
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [ids[1]], "payload": {
                "consolidated_from": [str(uuid.uuid4())], "chunk_type": "consolidated"}}, "POST")
            self.assert_http(409, lambda: f["api"](f"/memories/{ids[1]}/unconsolidate", method="POST"))
            self.assertIsNotNone(f["api"]("/memories/" + ids[1]))


    def test_legacy_preserved_sources_and_prepared_journal_without_summary(self):
        with fixture() as f:
            ids = seed_sources(f)
            summary_id = f["api"]("/memories", {"content": "Legacy summary"}, "POST")["id"]
            f["q"]("/collections/memories/points/payload?wait=true", {"points": [summary_id],
                   "payload": {"consolidated_from": ids, "chunk_type": "consolidated"}}, "POST")
            # Legacy summaries may have no memory_type/source_hash keys at all.
            f["q"]("/collections/memories/points/payload/delete?wait=true", {
                "points": [summary_id], "keys": ["memory_type", "source_hash"],
            }, "POST")
            result = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertEqual((result["restored"], result["retained"]), (0, 2))
            # A prepared journal remains usable even if summary creation never succeeded.
            op = operation(f, summary_id)
            op["state"] = "prepared"
            write_operation(f, op)
            f["q"]("/collections/memories/points/delete?wait=true", {"points": [ids[0]]}, "POST")
            result = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertEqual((result["restored"], result["retained"]), (1, 1))
            self.assertEqual({p["id"] for p in points(f)}, set(ids))

    def test_archive_write_failure_never_deletes_sources(self):
        # Wrong vector schema injects a real archive write error without production failpoints.
        with fixture(archive_dimensions=2) as f:
            ids = seed_sources(f)
            self.assert_http(500, lambda: f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST"))
            self.assertEqual({p["id"] for p in points(f)}, set(ids))
            self.assertEqual(points(f, "consolidation_archive"), [])

    def test_recover_interrupted_source_deletion_and_final_state_write(self):
        with fixture(preserve=True) as f:
            ids = seed_sources(f)
            f["api"]("/slumber/trigger?force_consolidation=true", {}, "POST")
            summary_id = next(p["id"] for p in points(f) if p["payload"].get("chunk_type") == "consolidated")
            op = operation(f, summary_id)
            # Reproduce a process interruption after archiving and deleting only one source.
            op["state"] = "prepared"
            op["preserve_originals"] = False
            write_operation(f, op)
            f["q"]("/collections/memories/points/delete?wait=true", {"points": [ids[0]]}, "POST")
            result = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertEqual((result["restored"], result["retained"]), (1, 1))
            # Interruption after summary deletion but before the final journal update.
            op = operation(f, summary_id)
            op["state"] = "restoring"
            write_operation(f, op)
            retry = f["api"](f"/memories/{summary_id}/unconsolidate", method="POST")
            self.assertEqual((retry["restored"], retry["retained"]), (0, 2))
            self.assertEqual(operation(f, summary_id)["state"], "restored")
            self.assertEqual({p["id"] for p in points(f)}, set(ids))


if __name__ == "__main__":
    unittest.main()
