"""Hermes timeline schema, validation, response, and transport contracts."""
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class TimelineTests(unittest.TestCase):
    def test_schema_dispatch_and_transport(self):
        provider, client = PLUGIN.Memex8MemoryProvider(), mock.Mock()
        schema = next(s for s in provider.get_tool_schemas() if s["name"] == "memex8_timeline")
        self.assertEqual(schema["parameters"]["properties"]["before"]["maximum"], 50)
        self.assertEqual(len(schema["parameters"]["oneOf"]), 3)
        args = {"at": "2026-11-08T00:00:00Z", "before": 0, "after": 50,
                "realm": "A", "visibility": "public"}
        client.timeline.return_value = {"results": [], "truncated": True, "no_match": False}
        self.assertEqual(provider._dispatch(client, "memex8_timeline", args), client.timeline.return_value)
        client.timeline.assert_called_once_with(**args)
        transport = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")
        with mock.patch.object(transport, "request", return_value={}) as request:
            transport.timeline(**args)
            request.assert_called_once_with("GET", "/api/v1/memories/timeline", params=args)
        for args in [{}, {"id": "a", "query": "b"}, {"query": " "},
                     {"query": "a", "before": True}, {"query": "a", "after": 51},
                     {"query": "a", "before": -1}, {"query": "a", "after": 1.5},
                     {"query": "a", "visibility": "unknown"}]:
            client.reset_mock()
            self.assertIn("error", provider._dispatch(client, "memex8_timeline", args))
            client.timeline.assert_not_called()


if __name__ == "__main__":
    unittest.main()
