"""Certification tool contracts; no running Hermes or memex8 required."""
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class CertificationTests(unittest.TestCase):
    def setUp(self):
        self.client = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")

    def test_ordinary_capture_omits_certification(self):
        with mock.patch.object(self.client, "request", return_value={"id": "id"}) as req:
            self.client.store("Ordinary capture")
        self.assertNotIn("certified", req.call_args.kwargs["json_body"])

    def test_explicitly_confirmed_store_is_one_request(self):
        with mock.patch.object(self.client, "request", return_value={"id": "id"}) as req:
            self.client.store("Confirmed", certified=True)
        req.assert_called_once_with("POST", "/api/v1/memories",
                                    json_body={"content": "Confirmed", "certified": True})

    def test_string_flags_do_not_accidentally_certify_or_revoke(self):
        for value in ["false", "true", 1, None]:
            with self.assertRaises(ValueError):
                self.client.store("Ordinary capture", certified=value)
            with self.assertRaises(ValueError):
                self.client.certify("id", revoke=value)

    def test_certification_keeps_note_and_attribution(self):
        with mock.patch.object(self.client, "request", return_value={}) as req:
            self.client.certify("id", by="marc", note="Confirmed")
        req.assert_called_once_with("POST", "/api/v1/memories/id/certify",
                                    json_body={"by": "marc", "note": "Confirmed"})

    def test_revocation_uses_delete(self):
        with mock.patch.object(self.client, "request", return_value={}) as req:
            self.client.certify("id", revoke=True)
        req.assert_called_once_with("DELETE", "/api/v1/memories/id/certify")

    def test_provider_dispatch_and_schema_agree(self):
        provider = PLUGIN.Memex8MemoryProvider()
        schemas = {schema["name"]: schema for schema in provider.get_tool_schemas()}
        certify = schemas["memex8_certify"]["parameters"]
        self.assertEqual(certify["required"], ["memory_id"])
        self.assertFalse(certify["properties"]["revoke"]["default"])
        self.assertFalse(schemas["memex8_remember"]["parameters"]["properties"]["certified"]["default"])
        client = mock.Mock()
        provider._dispatch(client, "memex8_certify", {"memory_id": "id", "by": "marc", "note": "yes"})
        client.certify.assert_called_once_with("id", revoke=False, by="marc", note="yes")
        provider._dispatch(client, "memex8_remember", {"content": "Confirmed", "certified": True})
        self.assertTrue(client.store.call_args.kwargs["certified"])
        self.assertEqual(provider._dispatch(client, "memex8_certify", {}), {"error": "memory_id is required"})


if __name__ == "__main__":
    unittest.main()
