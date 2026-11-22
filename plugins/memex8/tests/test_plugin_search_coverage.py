"""Coverage survives Hermes dispatch and appears before result bodies."""
import json
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN


class SearchCoverageTests(unittest.TestCase):
    def test_search_preserves_coverage_and_formats_first_line(self):
        provider, client = PLUGIN.Memex8MemoryProvider(), mock.Mock()
        response = {"coverage": {"level": "none", "best_score": .31, "results_above_min": 0,
                    "suggested_realms": ["Realm A"], "hint": "Try broader terms."}, "results": []}
        client.search.return_value = response
        self.assertEqual(provider._dispatch(client, "memex8_search", {"query": "missing"}), response)
        with mock.patch.object(provider, "_get_client", return_value=client):
            text = provider.handle_tool_call("memex8_search", {"query": "missing"})
        first, body = text.split("\n", 1)
        self.assertTrue(first.startswith("coverage=none best=0.31"))
        self.assertIn("try realms: Realm A", first)
        self.assertIn("retrieval heuristic", first)
        self.assertEqual(json.loads(body), response)
        response["coverage"]["best_score"] = None
        self.assertIn("best=null", PLUGIN._coverage_first_line(response["coverage"]))

    def test_filter_transport_and_legacy_fallback(self):
        client = PLUGIN._Client(api_key="test-only", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={}) as request:
            client.search("context", tags=["project:a"], visibility="public")
            self.assertEqual(request.call_args.kwargs["json_body"]["tags"], ["project:a"])
            self.assertEqual(request.call_args.kwargs["json_body"]["visibility"], "public")
        provider, client = PLUGIN.Memex8MemoryProvider(), mock.Mock()
        client.search.return_value = {"results": [{"id": "old-server"}]}
        result = provider._dispatch(client, "memex8_search", {
            "query": "context", "tags": ["project:a"], "visibility": "public", "compact": True})
        self.assertEqual(result, [{"id": "old-server"}])
        self.assertEqual(client.search.call_args.kwargs["tags"], ["project:a"])
        self.assertEqual(client.search.call_args.kwargs["visibility"], "public")
        self.assertIn("heuristic", PLUGIN.SEARCH_SCHEMA["description"])
        self.assertIn("tags", PLUGIN.SEARCH_SCHEMA["parameters"]["properties"])


if __name__ == "__main__":
    unittest.main()
