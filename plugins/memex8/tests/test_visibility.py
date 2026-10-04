"""Tests for the memex8 plugin's visibility kwarg.

Run with: ``python -m pytest plugins/memex8/tests/test_visibility.py``
or       ``python plugins/memex8/tests/test_visibility.py`` (self-runs).

These tests exercise the plugin in isolation — they mock the HTTP
client so no live memex8 server is required. The point is to lock
the contract that:

  * ``_Client.store(visibility="public")`` sends ``visibility`` in
    the JSON body;
  * ``_Client.store(visibility=None)`` (default) does NOT send
    the field, so the server's safe default of ``"private"`` kicks in;
  * the ``REMEMBER_SCHEMA`` exposes ``visibility`` as an enum;
  * the slash command parses ``--public`` without eating content
    that contains the word "public" mid-sentence;
  * import round-trip preserves a memory's visibility field.
"""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import unittest
from unittest import mock


HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN_PATH = os.path.normpath(os.path.join(HERE, os.pardir, "__init__.py"))


def _load_plugin_module():
    """Import plugins/memex8/__init__.py with the agent stubs Hermes
    normally provides, so the module loads outside a live host."""
    # Stub the two top-level imports the module does unconditionally.
    agent_stub = mock.MagicMock()
    agent_stub.memory_provider.MemoryProvider = object
    tools_stub = mock.MagicMock()
    tools_stub.registry.tool_error = lambda msg: json.dumps({"error": msg})

    saved = {k: sys.modules.get(k) for k in ("agent.memory_provider", "tools.registry")}
    sys.modules["agent"] = agent_stub
    sys.modules["agent.memory_provider"] = agent_stub.memory_provider
    sys.modules["tools"] = tools_stub
    sys.modules["tools.registry"] = tools_stub.registry
    try:
        spec = importlib.util.spec_from_file_location("memex8_plugin", PLUGIN_PATH)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        for k, v in saved.items():
            if v is None:
                sys.modules.pop(k, None)
            else:
                sys.modules[k] = v


PLUGIN = _load_plugin_module()


class VisibilityKwargTests(unittest.TestCase):
    def test_store_with_public_visibility_sends_field(self):
        client = PLUGIN._Client(api_key="x", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={"id": "abc"}) as req:
            client.store("hello", visibility="public")
        self.assertEqual(req.call_count, 1)
        method, path, kwargs = req.call_args[0][0], req.call_args[0][1], req.call_args[1]
        self.assertEqual(method, "POST")
        self.assertEqual(path, "/api/v1/memories")
        self.assertEqual(kwargs["json_body"]["visibility"], "public")

    def test_store_without_visibility_omits_field(self):
        client = PLUGIN._Client(api_key="x", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={"id": "abc"}) as req:
            client.store("hello")
        body = req.call_args[1]["json_body"]
        self.assertNotIn("visibility", body)

    def test_store_with_private_visibility_still_sends_field(self):
        # Explicit "private" is honored: the field is in the body, but
        # the server's default would do the same thing.
        client = PLUGIN._Client(api_key="x", base_url="http://localhost:8080")
        with mock.patch.object(client, "request", return_value={"id": "abc"}) as req:
            client.store("hello", visibility="private")
        body = req.call_args[1]["json_body"]
        self.assertEqual(body["visibility"], "private")


class RememberSchemaTests(unittest.TestCase):
    def test_remember_schema_exposes_visibility_enum(self):
        schema = PLUGIN.REMEMBER_SCHEMA
        vis = schema["parameters"]["properties"].get("visibility")
        self.assertIsNotNone(vis, "visibility must be in the schema")
        self.assertEqual(set(vis["enum"]), {"private", "public"})
        self.assertEqual(vis["default"], "private")
        # visibility must be optional, not required
        self.assertNotIn("visibility", schema["parameters"]["required"])
        # The schema's description should warn that public is a deliberate
        # opt-in, not a default the model should reach for. A model that
        # picks "public" by default is leaking data.
        self.assertIn("public", vis["description"].lower())
        self.assertIn("private", vis["description"].lower())


class SlashCommandTests(unittest.TestCase):
    """The /memex8 remember subcommand is a thin wrapper around
    _Client.store, but it has its own arg-parsing logic that we
    want to keep correct."""

    def test_remember_without_flag_is_private(self):
        with mock.patch.object(PLUGIN, "_MEMEX8_PROVIDER") as provider:
            provider._get_client.return_value.store.return_value = {"id": "abc"}
            PLUGIN._memex8_remember(["remember", "hello world"])
        kwargs = provider._get_client.return_value.store.call_args.kwargs
        self.assertIsNone(kwargs.get("visibility"))
        self.assertEqual(provider._get_client.return_value.store.call_args.args[0], "hello world")

    def test_remember_with_public_flag(self):
        with mock.patch.object(PLUGIN, "_MEMEX8_PROVIDER") as provider:
            provider._get_client.return_value.store.return_value = {"id": "abc"}
            PLUGIN._memex8_remember(["remember", "--public", "shareable fact"])
        kwargs = provider._get_client.return_value.store.call_args.kwargs
        self.assertEqual(kwargs.get("visibility"), "public")
        self.assertEqual(
            provider._get_client.return_value.store.call_args.args[0],
            "shareable fact",
        )

    def test_remember_with_public_in_content_not_flagged(self):
        with mock.patch.object(PLUGIN, "_MEMEX8_PROVIDER") as provider:
            provider._get_client.return_value.store.return_value = {"id": "abc"}
            # "public" appears mid-sentence, not as a leading flag.
            PLUGIN._memex8_remember(["remember", "this is a public fact"])
        kwargs = provider._get_client.return_value.store.call_args.kwargs
        self.assertIsNone(kwargs.get("visibility"))
        self.assertEqual(
            provider._get_client.return_value.store.call_args.args[0],
            "this is a public fact",
        )

    def test_remember_usage_message_documents_flag(self):
        out = PLUGIN._memex8_remember(["remember"])
        self.assertIn("--public", out)


class ImportRoundTripTests(unittest.TestCase):
    def test_import_preserves_visibility_kwarg(self):
        # Drive import_memories() through a tempdir export bundle that
        # contains a public memory. Verify the public field is passed
        # through to client.store().
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            bundle = {
                "version": PLUGIN._PLUGIN_VERSION,
                "exported_at": "2026-01-01T00:00:00Z",
                "realm": None,
                "count": 1,
                "page_size": 100,
                "memories": [
                    {
                        "id": "abc",
                        "content": "hello",
                        "visibility": "public",
                        "memory_type": "general",
                        "tags": ["t"],
                    }
                ],
            }
            path = os.path.join(td, "bundle.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(bundle, f)

            provider = PLUGIN.Memex8MemoryProvider()
            provider._config = {"enable_export_import": "true"}
            with mock.patch.object(provider, "_get_client") as gc:
                gc.return_value.store.return_value = {"id": "abc"}
                n = provider.import_memories(path)
        self.assertEqual(n, 1)
        kwargs = gc.return_value.store.call_args.kwargs
        self.assertEqual(kwargs.get("visibility"), "public")


if __name__ == "__main__":
    unittest.main(verbosity=2)
