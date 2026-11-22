"""memex8_dreams tool contracts; no running Hermes or memex8 required."""
import json
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class DreamToolTests(unittest.TestCase):
    def setUp(self):
        self.client = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")
        self.provider = PLUGIN.Memex8MemoryProvider()

    # ---- schema ----

    def test_schema_lists_the_three_actions(self):
        schemas = {s["name"]: s for s in self.provider.get_tool_schemas()}
        props = schemas["memex8_dreams"]["parameters"]["properties"]
        self.assertEqual(props["action"]["enum"], ["pending", "keep", "discard"])
        self.assertEqual(schemas["memex8_dreams"]["parameters"]["required"], ["action"])
        self.assertFalse(props["remember"]["default"])

    # ---- client ----

    def test_pending_returns_the_newest_in_full_and_the_count(self):
        listing = {"dreams": [{"id": "d2"}, {"id": "d1"}], "pending": 2, "enabled": True}
        full = {"id": "d2", "title": "T", "dream": "text", "waking_note": "note", "created_at": "c"}
        with mock.patch.object(self.client, "request", side_effect=[listing, full]) as req:
            out = self.client.dreams_pending()
        self.assertEqual(req.call_args_list[0].args, ("GET", "/api/v1/dreams"))
        self.assertEqual(req.call_args_list[0].kwargs["params"], {"status": "pending", "limit": 5})
        self.assertEqual(req.call_args_list[1].args, ("GET", "/api/v1/dreams/d2"))
        self.assertEqual(out["pending"], 2)
        self.assertEqual(out["dream"]["waking_note"], "note")

    def test_pending_with_nothing_waiting_is_empty(self):
        with mock.patch.object(self.client, "request", return_value={"dreams": [], "pending": 0}):
            out = self.client.dreams_pending()
        self.assertEqual(out, {"pending": 0, "dream": None})

    def test_server_without_a_dreams_api_gets_a_clear_message(self):
        err = RuntimeError("memex8 GET /api/v1/dreams failed (404): Not Found")
        with mock.patch.object(self.client, "request", side_effect=err):
            out = self.client.dreams_pending()
        self.assertIn("no dreams API", out["error"])
        self.assertIn("upgrade", out["error"].lower())

    def test_other_errors_are_not_disguised(self):
        err = RuntimeError("memex8 GET /api/v1/dreams failed (500): boom")
        with mock.patch.object(self.client, "request", side_effect=err):
            with self.assertRaises(RuntimeError):
                self.client.dreams_pending()

    def test_keep_sends_remember_only_as_a_real_boolean(self):
        with mock.patch.object(self.client, "request", return_value={"status": "kept"}) as req:
            self.client.dream_keep("d1", remember=True)
            self.client.dream_keep("d1")
        self.assertEqual(req.call_args_list[0].args, ("POST", "/api/v1/dreams/d1/keep"))
        self.assertEqual(req.call_args_list[0].kwargs["json_body"], {"remember": True})
        self.assertEqual(req.call_args_list[1].kwargs["json_body"], {"remember": False})
        for bad in ["true", "false", 1, None]:
            with self.assertRaises(ValueError):
                self.client.dream_keep("d1", remember=bad)

    def test_discard_uses_delete(self):
        with mock.patch.object(self.client, "request", return_value={"deleted": "d1"}) as req:
            self.client.dream_discard("d1")
        req.assert_called_once_with("DELETE", "/api/v1/dreams/d1")

    # ---- dispatch ----

    def test_dispatch_routes_each_action(self):
        client = mock.Mock()
        self.provider._dispatch(client, "memex8_dreams", {"action": "pending"})
        client.dreams_pending.assert_called_once_with()
        self.provider._dispatch(client, "memex8_dreams", {"action": "keep", "dream_id": "d1", "remember": True})
        client.dream_keep.assert_called_once_with("d1", remember=True)
        self.provider._dispatch(client, "memex8_dreams", {"action": "discard", "dream_id": "d1"})
        client.dream_discard.assert_called_once_with("d1")

    def test_dispatch_rejects_missing_id_and_unknown_action(self):
        client = mock.Mock()
        self.assertEqual(self.provider._dispatch(client, "memex8_dreams", {"action": "keep"}),
                         {"error": "dream_id is required for keep"})
        self.assertEqual(self.provider._dispatch(client, "memex8_dreams", {"action": "discard"}),
                         {"error": "dream_id is required for discard"})
        out = self.provider._dispatch(client, "memex8_dreams", {"action": "wake"})
        self.assertIn("action must be one of", out["error"])
        client.dream_keep.assert_not_called()
        client.dream_discard.assert_not_called()

    def test_tool_call_output_is_json(self):
        with mock.patch.object(PLUGIN.Memex8MemoryProvider, "_get_client") as gc:
            gc.return_value.dreams_pending.return_value = {"pending": 0, "dream": None}
            out = self.provider.handle_tool_call("memex8_dreams", {"action": "pending"})
        self.assertEqual(json.loads(out), {"pending": 0, "dream": None})


if __name__ == "__main__":
    unittest.main()
