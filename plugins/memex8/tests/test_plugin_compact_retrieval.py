"""Hermes compact retrieval transport, dispatch and compatibility contracts."""
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class CompactRetrievalTests(unittest.TestCase):
    def test_client_preserves_defaults_and_transmits_opt_in(self):
        client = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={}) as request:
            client.search("context")
            self.assertNotIn("compact", request.call_args.kwargs["json_body"])
            client.search("context", compact=True)
            self.assertIs(request.call_args.kwargs["json_body"]["compact"], True)
            client.recall()
            self.assertNotIn("compact", request.call_args.kwargs["params"])
            client.recall(compact=True)
            self.assertEqual(request.call_args.kwargs["params"]["compact"], "true")
            client.batch_get(["b", "a", "b"])
            request.assert_called_with("POST", "/api/v1/memories/batch-get", json_body={"ids": ["b", "a", "b"]})

    def test_dispatch_and_schema_match(self):
        provider, client = PLUGIN.Memex8MemoryProvider(), mock.Mock()
        schemas = {s["name"]: s for s in provider.get_tool_schemas()}
        for tool in ["search", "recall"]:
            schema = schemas["memex8_" + tool]
            self.assertIs(schema["parameters"]["properties"]["compact"]["default"], False)
            self.assertIn("memex8_get(ids=", schema["description"])
            getattr(client, tool).return_value = {"results": [{"id": "a", "snippet": "summary"}]}
            args = {"query": "context", "compact": True}
            self.assertEqual(provider._dispatch(client, "memex8_" + tool, args), [{"id": "a", "snippet": "summary"}])
            self.assertIs(getattr(client, tool).call_args.kwargs["compact"], True)
            self.assertIn("error", provider._dispatch(client, "memex8_" + tool, {"compact": "false"}))
        get = schemas["memex8_get"]["parameters"]
        self.assertEqual(get["properties"]["ids"]["maxItems"], 50)
        self.assertEqual(len(get["oneOf"]), 3)
        for alias in ["id", "memory_id"]:
            provider._dispatch(client, "memex8_get", {alias: "a"})
            client.get_memory.assert_called_with("a")
        provider._dispatch(client, "memex8_get", {"ids": ["b", "a", "b"]})
        client.batch_get.assert_called_once_with(["b", "a", "b"])
        for args in [{}, {"ids": []}, {"ids": ["a"]*51}, {"ids": [1]}, {"ids": "a"},
                     {"id": "a", "ids": ["b"]}, {"memory_id": "a", "id": "a"}]:
            self.assertIn("error", provider._dispatch(client, "memex8_get", args))


if __name__ == "__main__":
    unittest.main()
