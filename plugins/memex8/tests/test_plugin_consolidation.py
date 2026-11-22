"""Hermes schema, transport, and dispatch contracts for consolidation undo."""
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class ConsolidationTests(unittest.TestCase):
    def test_client_uses_authenticated_rest_transport(self):
        client = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={"restored": 2}) as request:
            self.assertEqual(client.unconsolidate("summary-id"), {"restored": 2})
        request.assert_called_once_with("POST", "/api/v1/memories/summary-id/unconsolidate")

    def test_schema_and_dispatch_agree(self):
        provider = PLUGIN.Memex8MemoryProvider()
        schemas = {s["name"]: s for s in provider.get_tool_schemas()}
        self.assertEqual(schemas["memex8_unconsolidate"]["parameters"]["required"], ["memory_id"])
        client = mock.Mock()
        provider._dispatch(client, "memex8_unconsolidate", {"memory_id": "summary-id"})
        client.unconsolidate.assert_called_once_with("summary-id")
        self.assertEqual(provider._dispatch(client, "memex8_unconsolidate", {}),
                         {"error": "memory_id is required"})


if __name__ == "__main__":
    unittest.main()
