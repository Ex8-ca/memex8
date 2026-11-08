"""Hermes duplicate-recovery schema and transport contracts."""
import unittest
from unittest import mock
try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class DuplicateRestoreTests(unittest.TestCase):
    def test_client_uses_existing_authenticated_transport(self):
        client = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={"restored": 1}) as request:
            self.assertEqual(client.restore_duplicate("operation-id"), {"restored": 1})
        request.assert_called_once_with("POST", "/api/v1/dedup/operation-id/restore")

    def test_schema_and_dispatch_agree(self):
        provider = PLUGIN.Memex8MemoryProvider()
        schemas = {s["name"]: s for s in provider.get_tool_schemas()}
        self.assertEqual(schemas["memex8_restore_duplicate"]["parameters"]["required"], ["operation_id"])
        client = mock.Mock()
        provider._dispatch(client, "memex8_restore_duplicate", {"operation_id": "operation-id"})
        client.restore_duplicate.assert_called_once_with("operation-id")
        self.assertEqual(provider._dispatch(client, "memex8_restore_duplicate", {}),
                         {"error": "operation_id is required"})


if __name__ == "__main__":
    unittest.main()
