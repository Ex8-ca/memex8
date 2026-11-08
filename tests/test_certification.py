"""Certification lifecycle against disposable Qdrant and a local mock LLM.

Run after cargo build: python3 tests/test_certification.py
Uses only a temporary Docker container, local ports, and temporary files.
"""
import http.server
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
import uuid


def request(url, body=None, method="GET", key=None):
    headers = {}
    if body is not None:
        headers["Content-Type"] = "application/json"
    if key:
        headers["Authorization"] = "Bearer " + key
    req = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(),
                                 method=method, headers=headers)
    with urllib.request.urlopen(req, timeout=60) as response:
        return json.load(response)


def wait_ready(url):
    for attempt in range(100):
        try:
            with urllib.request.urlopen(url, timeout=2) as response:
                response.read()
            return
        except OSError:
            if attempt == 99:
                raise
            time.sleep(.1)


class CertificationLifecycleTest(unittest.TestCase):
    def test_certification_survives_two_slumbers_and_revocation_resumes_lifecycle(self):
        repo = Path(__file__).resolve().parents[1]
        binary = os.environ.get("MEMEX8_TEST_BINARY", str(repo / "target/debug/memex8"))
        container = subprocess.check_output([
            "docker", "run", "--detach", "--rm", "--publish", "127.0.0.1::6333",
            "--publish", "127.0.0.1::6334", "--tmpfs", "/qdrant/storage", "qdrant/qdrant:latest",
        ], text=True).strip()
        process = None
        mock_server = None
        try:
            ports = json.loads(subprocess.check_output([
                "docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container,
            ], text=True))
            qhttp = "http://127.0.0.1:" + ports["6333/tcp"][0]["HostPort"]
            qgrpc = "http://127.0.0.1:" + ports["6334/tcp"][0]["HostPort"]
            wait_ready(qhttp + "/readyz")
            prompts = []
            late_certification = {"id": None}

            class MockLLM(http.server.BaseHTTPRequestHandler):
                def do_POST(self):
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    if self.path.endswith("/embeddings"):
                        inputs = body["input"]
                        if isinstance(inputs, str):
                            inputs = [inputs]
                        response = {"data": [{"index": i, "embedding": [1.] + [0.] * 63}
                                             for i, _ in enumerate(inputs)]}
                    else:
                        prompt = body["messages"][-1]["content"]
                        prompts.append(prompt)
                        if "You are verifying an AI memory store" in prompt:
                            ids = re.findall(r"- id: ([0-9a-f-]+)", prompt)
                            if late_certification["id"] in ids:
                                request(qhttp + "/collections/memories/points/payload?wait=true", {
                                    "points": [late_certification["id"]],
                                    "payload": {"verification_status": "certified",
                                                "certified_at": "2026-11-08T00:00:00Z",
                                                "certified_by": "concurrent-client"},
                                }, "POST")
                            content = json.dumps([{"id": id_, "confidence": .1} for id_ in ids])
                        else:
                            content = "Fixture summary of ordinary sources."
                        response = {"choices": [{"message": {"content": content}}]}
                    data = json.dumps(response).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)

                def log_message(self, *args):
                    pass

            mock_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), MockLLM)
            threading.Thread(target=mock_server.serve_forever, daemon=True).start()
            llm_url = "http://127.0.0.1:" + str(mock_server.server_port)
            with tempfile.TemporaryDirectory(prefix="memex8-certification-") as directory:
                work = Path(directory)
                with socket.socket() as sock:
                    sock.bind(("127.0.0.1", 0))
                    api_port = sock.getsockname()[1]
                api = f"http://127.0.0.1:{api_port}/api/v1"
                env = {**os.environ, "QDRANT_URL": qgrpc, "OPENAI_API_KEY": "test-only",
                       "EMBEDDING_PROVIDER": "openai", "EMBEDDING_DIMENSIONS": "64",
                       "OPENAI_BASE_URL": llm_url + "/v1", "LOCAL_LLM_URL": llm_url,
                       "MEMEX8_API_KEY": "test-only", "LOCAL_LLM_API_KEY": "test-only",
                       "TURBOVEC_INDEX_PATH": str(work / "test.tv"),
                       "TURBOVEC_ID_MAP_PATH": str(work / "test-ids.json")}
                config = subprocess.check_output([binary, "--config", str(work / "missing"),
                                                  "config-show"], env=env, text=True,
                                                 stderr=subprocess.DEVNULL)
                config = config.replace('base_url = "https://api.openai.com/v1"',
                                        'base_url = ' + json.dumps(llm_url + "/v1"))
                config = config.replace('backend = "openai"', 'backend = "local"')
                config = config.replace('[memex8_md]\nenabled = true', '[memex8_md]\nenabled = false')
                config = config.replace('[digest_md]\nenabled = true', '[digest_md]\nenabled = false')
                self.assertIn('[memex8_md]\nenabled = false', config)
                self.assertIn('[digest_md]\nenabled = false', config)
                config = config.replace('idle_timeout = "10m"', 'idle_timeout = "24h"')
                config_path = work / "config.toml"
                config_path.write_text(config)
                command = [binary, "--config", str(config_path)]
                with (work / "server.log").open("w+") as log:
                    process = subprocess.Popen(command + ["serve", "--host", "127.0.0.1", "--port", str(api_port)],
                                               env=env, cwd=directory, stdout=log, stderr=log)
                    try:
                        wait_ready(f"http://127.0.0.1:{api_port}/health")
                    except OSError:
                        log.seek(0)
                        self.fail("Test server failed to start:\n" + log.read())

                    def api_request(path, body=None, method="GET"):
                        return request(api + path, body, method, "test-only")

                    def payload(id_, **values):
                        request(qhttp + "/collections/memories/points/payload?wait=true",
                                {"points": [id_], "payload": values}, "POST")

                    # Authentication applies to the new endpoints, too.
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        request(api + f"/memories/{uuid.uuid4()}/certify", {}, "POST")
                    self.assertEqual(error.exception.code, 401)
                    error.exception.close()
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        api_request(f"/memories/{uuid.uuid4()}/certify", {}, "POST")
                    self.assertEqual(error.exception.code, 404)
                    error.exception.close()

                    content = "CERTIFIED_ONLY: My confirmed preference must survive maintenance."
                    id_ = api_request("/memories", {"content": content, "certified": True,
                                                  "memory_type": "event", "tags": ["confirmed"]}, "POST")["id"]
                    created = api_request("/memories/" + id_)
                    self.assertEqual(created["verification_status"], "certified")
                    self.assertIsNotNone(created["certified_at"])
                    payload(id_, importance=.2, ingested_at="2000-01-01T00:00:00Z",
                            last_accessed="2000-01-01T00:00:00Z", chunk_type="session_summary")
                    confirmed = api_request(f"/memories/{id_}/certify", {"by": "marc", "note": "Confirmed"}, "POST")
                    # Empty body is supported and repeated certification is stable.
                    repeated = api_request(f"/memories/{id_}/certify", method="POST")
                    self.assertEqual(confirmed["certified_at"], repeated["certified_at"])
                    self.assertEqual(repeated["certification_note"], "Confirmed")
                    self.assertEqual(repeated["certified_by"], "marc")

                    for text in ["Ordinary source A", "Ordinary source B"]:
                        result = api_request("/memories", {"content": text,
                                             "realm_hint": confirmed["realm_name"]}, "POST")
                        self.assertEqual(api_request("/memories/" + result["id"])["verification_status"], "unverified")
                    protected_fields = ["content", "importance", "tags", "ingested_at", "memory_type",
                                        "verification_status", "last_verified", "verification_confidence",
                                        "certified_at", "certified_by", "certification_note"]
                    for _ in range(2):
                        api_request("/slumber/trigger?force_consolidation=true", {}, "POST")
                        after = api_request("/memories/" + id_)
                        self.assertEqual({k: after[k] for k in protected_fields},
                                         {k: confirmed[k] for k in protected_fields})
                    self.assertTrue(prompts, "mock must actually receive consolidation/verification calls")
                    self.assertFalse(any("CERTIFIED_ONLY" in prompt for prompt in prompts))
                    memories = api_request("/memories?limit=50")["memories"]
                    self.assertTrue(any(m["consolidated_from"] for m in memories))
                    self.assertTrue(all(id_ not in m["consolidated_from"] for m in memories))
                    self.assertEqual(api_request("/memories/verification-summary")["certified"], 1)
                    self.assertEqual((request(qhttp + "/collections/memories")["result"]
                                      ["payload_schema"]["verification_status"]["data_type"]), "keyword")

                    # Old certified memories keep full recall recency, plus the 1.05 multiplier.
                    recall = api_request("/memories/recall?limit=50")["results"]
                    row = next(m for m in recall if m["id"] == id_)
                    self.assertAlmostEqual(row["score"], confirmed["importance"] * 1.05, places=5)

                    revoked = api_request(f"/memories/{id_}/certify", method="DELETE")
                    self.assertEqual(revoked["verification_status"], "unverified")
                    for field in ["certified_at", "certified_by", "certification_note"]:
                        self.assertIsNone(revoked[field])
                    api_request("/slumber/trigger", {}, "POST")
                    ordinary = api_request("/memories/" + id_)
                    self.assertNotEqual(ordinary["importance"], revoked["importance"])
                    self.assertEqual(ordinary["verification_status"], "contradicted")
                    self.assertIsNotNone(ordinary["last_verified"])

                    # A different client certifies a point while the LLM is rating it.
                    late_id = api_request("/memories", {"content": "Late confirmation fixture"}, "POST")["id"]
                    late_certification["id"] = late_id
                    api_request("/slumber/trigger", {}, "POST")
                    late = api_request("/memories/" + late_id)
                    self.assertEqual(late["verification_status"], "certified")
                    self.assertEqual(late["certified_by"], "concurrent-client")
                    self.assertIsNone(late["last_verified"], "stale verification write bypassed protection")

                    # Exercise real stdio tool dispatch and advertised schemas.
                    calls = [{"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
                             {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                                 "name": "memex8_certify", "arguments": {"memory_id": late_id, "revoke": True}}},
                             {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                                 "name": "memex8_remember", "arguments": {"content": "Explicitly remembered", "certified": True}}}]
                    result = subprocess.run(command + ["mcp"], input="\n".join(map(json.dumps, calls)) + "\n",
                                            text=True, capture_output=True, env=env, cwd=directory, check=True, timeout=60)
                    replies = [json.loads(line) for line in result.stdout.splitlines()]
                    tools = {tool["name"] for tool in replies[0]["result"]["tools"]}
                    self.assertTrue({"memex8_certify", "memex8_remember", "memex8_store"} <= tools)
                    self.assertNotIn("error", replies[1])
                    remembered = api_request("/memories/" + replies[2]["result"]["id"])
                    self.assertEqual(remembered["verification_status"], "certified")

                    # Legacy/manual certified statuses may lack timestamp metadata.
                    payload(late_id, verification_status="certified", certified_at=None)
                    repaired = api_request(f"/memories/{late_id}/certify", {}, "POST")
                    self.assertIsNotNone(repaired["certified_at"])
                    payload(late_id, certified_at=None)
                    legacy_revoked = api_request(f"/memories/{late_id}/certify", method="DELETE")
                    self.assertEqual(legacy_revoked["verification_status"], "unverified")

                    # Import a complete certified snapshot under a new ID, preserving metadata.
                    imported = {**remembered, "id": str(uuid.uuid4()), "certified_by": "restore-test",
                                "certification_note": "Restored note", "vector": [1.] + [0.] * 63,
                                "consolidated_from": ["source-a"], "quantized_bit_width": 4.0}
                    path = work / "import.json"
                    path.write_text(json.dumps([imported]))
                    subprocess.run(command + ["import", str(path), "--reuse-vectors"], env=env, cwd=directory,
                                   capture_output=True, text=True, check=True, timeout=30)
                    restored = api_request("/memories/" + imported["id"])
                    self.assertEqual(restored["certification_note"], "Restored note")
                    self.assertEqual(restored["certified_at"], remembered["certified_at"])
                    self.assertEqual(restored["certified_by"], "restore-test")
                    self.assertEqual(restored["consolidated_from"], ["source-a"])
                    self.assertEqual(restored["quantized_bit_width"], 4.0)
                    # Replacing a certified point through import is rejected, without losing content.
                    imported["content"] = "Unauthorized replacement"
                    path.write_text(json.dumps([imported]))
                    replacement = subprocess.run(command + ["import", str(path), "--reuse-vectors"], env=env,
                                                 cwd=directory, capture_output=True, text=True, timeout=30)
                    self.assertNotEqual(replacement.returncode, 0)
                    self.assertEqual(api_request("/memories/" + imported["id"])["content"], restored["content"])
        finally:
            if process is not None:
                process.terminate()
                process.wait(timeout=10)
            if mock_server is not None:
                mock_server.shutdown()
                mock_server.server_close()
            subprocess.run(["docker", "stop", container], check=True, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    unittest.main()
