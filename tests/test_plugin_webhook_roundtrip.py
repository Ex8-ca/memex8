"""Real-webhook round trip: plugin archive -> memex8 -> disposable Qdrant.

Proves the two properties the offline tests can only assert against a mock:
the server accepts every write (one write = one embedding input, so a chunk
over the model's token cap would 400 and kill the checkpoint), and the bytes
it stores can be reassembled back into the exact transcript.

Requires `cargo build` (target/debug/memex8) plus Docker for a throwaway
Qdrant. Loopback only: no production service, key, or data is touched.
"""
import contextlib
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading

import pytest

from test_certification import request, wait_ready
from test_prefetch import plugin, provider

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("MEMEX8_TEST_BINARY", str(REPO / "target/debug/memex8")))
API_KEY = "test-only"

pytestmark = pytest.mark.skipif(
    not BINARY.exists() or shutil.which("docker") is None,
    reason="needs a cargo-built target/debug/memex8 and docker for a disposable qdrant",
)


@contextlib.contextmanager
def memex8_server():
    container = subprocess.check_output([
        "docker", "run", "--detach", "--rm", "--publish", "127.0.0.1::6333",
        "--publish", "127.0.0.1::6334", "--tmpfs", "/qdrant/storage",
        "qdrant/qdrant:latest",
    ], text=True).strip()
    process = mock = None
    try:
        ports = json.loads(subprocess.check_output([
            "docker", "inspect", "--format", "{{json .NetworkSettings.Ports}}", container,
        ], text=True))
        qhttp = "http://127.0.0.1:" + ports["6333/tcp"][0]["HostPort"]
        qgrpc = "http://127.0.0.1:" + ports["6334/tcp"][0]["HostPort"]
        wait_ready(qhttp + "/readyz")

        # Embeddings only: nothing in this test summarises or verifies.
        class MockLLM(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                inputs = body.get("input") or []
                if isinstance(inputs, str):
                    inputs = [inputs]
                response = {"data": [{"index": i, "embedding": [1.0] + [0.0] * 63}
                                     for i, _ in enumerate(inputs)]}
                data = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        mock = http.server.ThreadingHTTPServer(("127.0.0.1", 0), MockLLM)
        threading.Thread(target=mock.serve_forever, daemon=True).start()
        llm_url = "http://127.0.0.1:" + str(mock.server_port)

        with tempfile.TemporaryDirectory(prefix="memex8-roundtrip-") as directory:
            work = Path(directory)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            api = f"http://127.0.0.1:{port}/api/v1"
            env = {**os.environ, "QDRANT_URL": qgrpc, "OPENAI_API_KEY": "test-only",
                   "EMBEDDING_PROVIDER": "openai", "EMBEDDING_DIMENSIONS": "64",
                   "OPENAI_BASE_URL": llm_url + "/v1", "LOCAL_LLM_URL": llm_url,
                   "MEMEX8_API_KEY": API_KEY, "LOCAL_LLM_API_KEY": "test-only",
                   "TURBOVEC_INDEX_PATH": str(work / "test.tv"),
                   "TURBOVEC_ID_MAP_PATH": str(work / "test-ids.json")}
            config = subprocess.check_output(
                [str(BINARY), "--config", str(work / "missing"), "config-show"],
                env=env, text=True, stderr=subprocess.DEVNULL)
            config = config.replace('base_url = "https://api.openai.com/v1"',
                                    "base_url = " + json.dumps(llm_url + "/v1"))
            config = config.replace('backend = "openai"', 'backend = "local"')
            for feature in ("memex8_md", "digest_md", "verification"):
                config = config.replace(f"[{feature}]\nenabled = true",
                                        f"[{feature}]\nenabled = false")
                assert f"[{feature}]\nenabled = false" in config
            config_path = work / "config.toml"
            config_path.write_text(config)
            with (work / "server.log").open("w+") as log:
                process = subprocess.Popen(
                    [str(BINARY), "--config", str(config_path), "serve",
                     "--host", "127.0.0.1", "--port", str(port)],
                    env=env, cwd=directory, stdout=log, stderr=log)
                try:
                    wait_ready(f"http://127.0.0.1:{port}/health")
                    yield {
                        "base": f"http://127.0.0.1:{port}",
                        "api": api,
                        "call": lambda path, body=None, method="GET": request(
                            api + path, body, method, API_KEY),
                    }
                except Exception:
                    log.seek(0)
                    print(log.read()[-6000:])
                    raise
    finally:
        if process is not None:
            process.terminate()
            process.wait(timeout=10)
        if mock is not None:
            mock.shutdown()
            mock.server_close()
        subprocess.run(["docker", "stop", container], check=True,
                       stdout=subprocess.DEVNULL)


def get_memory(server, memory_id):
    return server["call"]("/memories/" + memory_id)


def test_checkpoint_round_trips_through_the_real_webhook():
    messages = [
        {"role": "user", "content": "Deployment review: the queue drains slowly. "
                                    "<private>operator note, do not archive</private>"},
        {"role": "tool", "tool_call_id": "call-1", "content": {
            "argv": ["--drain", "--batch", "500"],
            "stdout": "processed\n" * 1800,
            "path": "/srv/memex8/queue/worker.log",
        }},
        {"role": "assistant", "content": [{"type": "text", "text": "Rolling restart it."}]},
        {"role": "user", "content": "界" * 3000 + " END_OF_EVIDENCE"},
    ]
    expected = plugin._transcript(messages)
    assert len(expected.encode("utf-8")) > 3 * plugin._WEBHOOK_CHUNK_BYTES

    with memex8_server() as server:
        p = provider(plugin._Client(API_KEY, server["base"]))
        marker = p.on_pre_compress(messages, require_checkpoint=True)
        assert marker.startswith("checkpoint: ")
        root_id = marker.split(": ", 1)[1]

        root = get_memory(server, root_id)
        page = json.loads(plugin._unframe(root["content"], None))

        def fetch(memory_id):
            return get_memory(server, memory_id)["content"]

        # Exact reassembly across every chunk and manifest level, with the
        # recorded sha256 verified against what the server actually stored.
        assert plugin.reassemble_transcript(page, fetch) == expected

        # Provenance: durable evidence, not an ordinary conversation memory.
        assert root["memory_type"] == "artifact"
        assert "checkpoint" in root["tags"] and "transcript-archive" in root["tags"]
        assert "platform:cli" in root["tags"]
        assert "source:hermes-pre-compress-checkpoint" in root["tags"]
        assert root["visibility"] == "private"

        # The private span never reached the store.
        assert "operator note" not in root["content"]
        assert all("operator note" not in fetch(node["id"])
                   for node in page["chunks"])

        # A caller that sends neither tags nor memory_type keeps the historical
        # defaults: existing integrations are unchanged.
        default = request(
            server["api"] + "/webhooks/conversation",
            {"summary": "A plain conversation summary from another caller.",
             "source": "openclaw", "platform": "cli"},
            "POST", API_KEY)
        stored = get_memory(server, default["id"])
        assert stored["tags"] == ["conversation", "platform:cli"]
        assert stored["memory_type"] == "general"
        assert stored["content"] == "# Conversation Summary\n\nA plain conversation summary from another caller.\n\n"
