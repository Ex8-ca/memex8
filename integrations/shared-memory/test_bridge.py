import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError

SCRIPT = Path(__file__).with_name("bridge.py")
spec = importlib.util.spec_from_file_location("bridge", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.vault = Path(self.temp.name) / "My Vault"
        self.vault.mkdir()
        (self.vault / "Deploy.md").write_text("# Deploy\nUse the staging service.\n", encoding="utf-8")
        self.bridge = module.Bridge("http://localhost:8080", "secret", "codex", "Ex8-ca/memex8", self.vault, "test-session")
        self.memories = {}
        self.requests = []

    def backend(self, method, path, body=None):
        self.requests.append((method, path, body))
        if path == "/memories" and method == "POST":
            self.memories["1"] = {"id": "1", **body, "source_file": body["source"]}
            return {"id": "1", "status": "stored"}
        if path == "/memories/search":
            # Intentionally return everything: adapter must also check scope.
            return {"results": list(self.memories.values())}
        return self.memories[path.rsplit("/", 1)[-1]]

    def call(self, name, args):
        return self.bridge.dispatch({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})["result"]

    def test_round_trip_across_agents_and_note_reference(self):
        with patch.object(self.bridge, "request", side_effect=self.backend):
            saved = self.call("memex8_remember", {"content": "Deployment decision", "note_paths": ["Deploy.md"]})
            self.assertFalse(saved["isError"])
            self.assertEqual(self.memories["1"]["visibility"], "private")
            self.assertEqual(self.memories["1"]["source"], "codex:test-session")
            self.assertIn("project:Ex8-ca/memex8", self.memories["1"]["tags"])
            self.assertIn("obsidian://open?vault=My%20Vault&file=Deploy.md", self.memories["1"]["content"])
            self.bridge.agent = "claude-code"
            result = self.call("memex8_search", {"query": "Deployment"})
            self.assertFalse(result["isError"])
            data = json.loads(result["content"][0]["text"])
            self.assertEqual(data["results"][0]["source"], "codex:test-session")
            self.assertEqual(self.requests[-2][2]["tags"], ["project:Ex8-ca/memex8"])

    def test_scope_rejects_other_project_and_explicit_all_reads_hermes(self):
        self.memories["1"] = {"id": "1", "content": "old Hermes knowledge", "tags": []}
        with patch.object(self.bridge, "request", side_effect=self.backend):
            self.assertTrue(self.call("memex8_get", {"id": "1"})["isError"])
            self.assertTrue(self.call("memex8_search", {"query": "knowledge"})["isError"])
            self.assertFalse(self.call("memex8_search", {"query": "knowledge", "scope": "all"})["isError"])
            self.assertFalse(self.call("memex8_get", {"id": "1", "scope": "all"})["isError"])

    def test_old_backend_write_does_not_claim_scoped_success(self):
        def old_backend(method, path, body=None):
            result = self.backend(method, path, body)
            if path == "/memories":
                self.memories["1"]["tags"] = []
            return result
        with patch.object(self.bridge, "request", side_effect=old_backend):
            result = self.call("memex8_remember", {"content": "decision"})
        self.assertTrue(result["isError"])
        self.assertIn("do not retry", result["content"][0]["text"])
        self.assertEqual(len(self.memories), 1)

    def test_compact_search_and_full_get(self):
        self.memories["1"] = {"id": "1", "content": "a" * 5000, "tags": ["project:Ex8-ca/memex8"]}
        with patch.object(self.bridge, "request", side_effect=self.backend):
            search = json.loads(self.call("memex8_search", {"query": "a"})["content"][0]["text"])
            get = json.loads(self.call("memex8_get", {"id": "1"})["content"][0]["text"])
        self.assertEqual(len(search["results"][0]["preview"]), 500)
        self.assertEqual(len(get["content"]), 5000)

    def test_vault_search_and_current_content(self):
        result = self.bridge.call("obsidian_search", {"query": "staging"})
        self.assertEqual(result["results"][0]["path"], "Deploy.md")
        (self.vault / "Deploy.md").write_text("Changed", encoding="utf-8")
        self.assertEqual(self.bridge.call("obsidian_get", {"path": "Deploy.md"})["content"], "Changed")

    def test_hidden_traversal_and_symlink_notes_are_excluded(self):
        outside = Path(self.temp.name) / "outside.md"
        outside.write_text("private staging")
        (self.vault / "linked.md").symlink_to(outside)
        (self.vault / ".hidden").mkdir()
        (self.vault / ".hidden" / "secret.md").write_text("private staging")
        for path in ("../outside.md", str(outside), "linked.md", ".hidden/secret.md"):
            self.assertTrue(self.call("obsidian_get", {"path": path})["isError"])
        hits = self.bridge.call("obsidian_search", {"query": "private"})
        self.assertEqual(hits["results"], [])

    def test_note_pagination(self):
        (self.vault / "Long.md").write_text("é" * 15000, encoding="utf-8")
        first = self.bridge.call("obsidian_get", {"path": "Long.md"})
        second = self.bridge.call("obsidian_get", {"path": "Long.md", "offset": first["next_offset"]})
        self.assertEqual(len(first["content"] + second["content"]), 15000)
        self.assertIsNone(second["next_offset"])

    def test_bad_arguments_and_notifications(self):
        for args in ({"query": "x", "limit": True}, {"query": "x", "limit": 0}, {"query": "x", "scope": "bad"}, {"query": []}, {"query": "x", "extra": 1}):
            self.assertTrue(self.call("memex8_search", args)["isError"])
        self.assertIsNone(self.bridge.dispatch({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        self.assertEqual(self.bridge.dispatch([])["error"]["code"], -32600)

    def test_auth_and_sanitized_errors(self):
        with patch.object(self.bridge.opener, "open", return_value=io.BytesIO(b'{"id":"1"}')) as opened:
            self.assertEqual(self.bridge.request("GET", "/memories/1")["id"], "1")
            request = opened.call_args.args[0]
            self.assertEqual(request.get_header("Authorization"), "Bearer secret")
            self.assertEqual(request.full_url, "http://localhost:8080/api/v1/memories/1")
        for error in (HTTPError("url", 401, "secret", {}, None), URLError("secret")):
            with patch.object(self.bridge.opener, "open", side_effect=error):
                result = self.call("memex8_search", {"query": "x"})
                self.assertTrue(result["isError"])
                self.assertNotIn("secret", str(result))
            if isinstance(error, HTTPError):
                error.close()

    def test_stdio_handshake_parse_error_and_tool_list(self):
        messages = ["{broken", json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}), json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})]
        proc = subprocess.run([sys.executable, str(SCRIPT), "--agent", "codex", "--project", "test"], input="\n".join(messages) + "\n", text=True, capture_output=True, env={**os.environ, "MEMEX8_API_KEY": "secret", "MEMEX8_OBSIDIAN_VAULT": ""}, timeout=10)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        replies = [json.loads(line) for line in proc.stdout.splitlines()]
        self.assertEqual(len(replies), 3)
        self.assertEqual(replies[0]["error"]["code"], -32700)
        self.assertEqual(replies[1]["result"]["protocolVersion"], "2025-06-18")
        self.assertEqual(len(replies[2]["result"]["tools"]), 3)

    def test_lan_http_requires_explicit_private_address(self):
        for host in ("192.168.1.3", "10.0.0.10", "172.16.2.3", "[fd00::3]"):
            with self.subTest(host=host):
                url = "http://" + host + ":8080"
                with self.assertRaises(ValueError):
                    module.Bridge(url, "secret", "codex", "test")
                bridge = module.Bridge(url, "secret", "codex", "test", allow_lan_http=True)
                self.assertEqual(bridge.base_url, url)
        for host in ("8.8.8.8", "0.0.0.0", "169.254.169.254", "192.0.2.1", "memex8.example", "192.168.1.3.example"):
            with self.subTest(host=host), self.assertRaises(ValueError):
                module.Bridge("http://" + host, "secret", "codex", "test", allow_lan_http=True)
        module.Bridge("https://memory.example", "secret", "codex", "test")

    def test_lan_url_auth_and_no_proxy_or_redirect(self):
        with patch.dict(os.environ, {"http_proxy": "http://unwanted.example:8080"}):
            bridge = module.Bridge("http://192.168.1.3:8080", "secret", "codex", "test", allow_lan_http=True)
        self.assertFalse(any(isinstance(h, module.ProxyHandler) and h.proxies for h in bridge.opener.handlers))
        self.assertIsNone(module.NoRedirect().redirect_request(None, None, 302, "", {}, "http://other.example"))
        with patch.object(bridge.opener, "open", return_value=io.BytesIO(b'{"memories":[]}')) as opened:
            result = bridge.check()
        req = opened.call_args.args[0]
        self.assertEqual(req.full_url, "http://192.168.1.3:8080/api/v1/memories?limit=1")
        self.assertEqual(req.get_header("Authorization"), "Bearer secret")
        self.assertEqual(result["api"], "reachable")
        self.assertNotIn("memories", result)

    def test_two_devices_share_memory_and_resolve_local_vault_paths(self):
        other_vault = Path(self.temp.name) / "Laptop Vault"
        other_vault.mkdir()
        (other_vault / "Deploy.md").write_text("Synced note on laptop", encoding="utf-8")
        desktop = module.Bridge("http://192.168.1.3:8080", "secret", "codex", "Ex8-ca/memex8", self.vault, "desktop-session", True, "desktop")
        laptop = module.Bridge("http://192.168.1.3:8080", "secret", "claude-code", "Ex8-ca/memex8", other_vault, "laptop-session", True, "laptop")
        with patch.object(desktop, "request", side_effect=self.backend), patch.object(laptop, "request", side_effect=self.backend):
            desktop.call("memex8_remember", {"content": "A shared decision", "note_paths": ["Deploy.md"]})
            recalled = laptop.call("memex8_get", {"id": "1"})
            self.assertIn("device:desktop", recalled["tags"])
            self.assertIn("Deploy.md", recalled["content"])
            self.assertEqual(laptop.call("obsidian_get", {"path": "Deploy.md"})["content"], "Synced note on laptop")
            laptop.call("memex8_remember", {"content": "Laptop follow-up"})
            self.assertIn("device:laptop", self.memories["1"]["tags"])

    def test_check_rejects_wrong_service_response(self):
        with patch.object(self.bridge, "request", return_value={"status": "healthy"}):
            with self.assertRaises(ValueError):
                self.bridge.check()


if __name__ == "__main__":
    unittest.main()
