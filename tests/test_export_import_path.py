"""Tests for resolve_bundle_path (path confinement) and is_available.

Locks in the security behavior of the export/import path gate from
c60ec42: chat-supplied paths must stay inside
$HERMES_HOME/plugin-data/memex8/ unless enable_export_import=true,
including symlink/..-traversal/absolute-path attacks.

Run: python3 -m pytest tests/test_export_import_path.py -v
     (or python3 tests/test_export_import_path.py directly)
"""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

# Make the plugin module importable without a full Hermes install.
_TESTS_DIR = Path(__file__).resolve().parent
_PLUGIN_DIR = _TESTS_DIR.parent / "plugins" / "memex8"
sys.path.insert(0, str(_PLUGIN_DIR))

# Stub the Hermes host imports that the plugin module pulls in at module
# level — we only exercise pure-Python logic here, not the host contract.
import types as _types_mod

_hostStub = _types_mod.ModuleType("agent")
_mpStub = _types_mod.ModuleType("agent.memory_provider")


class _MemoryProvider:
    pass


def _spawn_context_thread(fn, name=""):
    raise NotImplementedError("not exercised in unit tests")


_mpStub.MemoryProvider = _MemoryProvider
_mpStub.spawn_context_thread = _spawn_context_thread
sys.modules["agent"] = _hostStub
sys.modules["agent.memory_provider"] = _mpStub

_toolsStub = _types_mod.ModuleType("tools")
_regStub = _types_mod.ModuleType("tools.registry")
_regStub.tool_error = lambda msg: {"error": msg}
sys.modules["tools"] = _toolsStub
sys.modules["tools.registry"] = _regStub

# Import the plugin module by file path — 'plugins/memex8/__init__.py' isn't
# a real package name, so a plain `import __init__` inside this tests dir
# would be ambiguous. load with importlib explicitly.
import importlib.util as _ilu

_spec = _ilu.spec_from_file_location(
    "memex8_plugin_under_test", _PLUGIN_DIR / "__init__.py"
)
assert _spec is not None and _spec.loader is not None, "plugin file missing"
plugin = _ilu.module_from_spec(_spec)
_spec.loader.exec_module(plugin)


def _make_provider(tmp_home: str, export_enabled: bool = False):
    """Build a Memex8MemoryProvider pointed at a throwaway HERMES_HOME."""
    p = plugin.Memex8MemoryProvider.__new__(plugin.Memex8MemoryProvider)
    p._hermes_home = tmp_home
    p._config = {"enable_export_import": "true" if export_enabled else "false"}
    return p


def _base(tmp_home: str) -> Path:
    return (Path(tmp_home) / "plugin-data" / "memex8").resolve()


class TestResolveBundlePath(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="memex8-test-")

    def test_plain_path_allowed(self):
        p = _make_provider(self.tmp)
        out = p.resolve_bundle_path("dump.json")
        assert out == str(_base(self.tmp) / "dump.json")

    def test_nested_path_allowed_and_dirs_created(self):
        """Regression: nested paths must not fail on open() because the
        intermediate directories were never created (c60ec42 created only
        the base dir)."""
        p = _make_provider(self.tmp)
        raw = "backups/weekly/dump.json"
        out = p.resolve_bundle_path(raw)
        expected = _base(self.tmp) / raw
        assert out == str(expected)
        # The directory chain must now exist so export's open() succeeds.
        assert expected.parent.is_dir(), f"{expected.parent} was not created"
        # And a real write through the returned path must succeed.
        Path(out).write_text("{}")

    def test_dotdot_escape_rejected(self):
        p = _make_provider(self.tmp)
        with self.assertRaises(ValueError):
            p.resolve_bundle_path("../escape.json")

    def test_absolute_path_rejected(self):
        p = _make_provider(self.tmp)
        #AbsolutePathBehaviour: joining an absolute path with base
        # replaces base entirely, so /etc/passwd escapes and must be rejected.
        with self.assertRaises(ValueError):
            p.resolve_bundle_path("/etc/passwd")

    def test_symlink_escape_rejected(self):
        """Regression: a pre-planted symlink inside base pointing outside
        must be rejected — resolve() collapses the target before the
        ancestry check."""
        base = _base(self.tmp)
        base.mkdir(parents=True, exist_ok=True)
        outside = Path(tempfile.mkdtemp(prefix="memex8-outside-")) / "target.json"
        outside.write_text("pwned")
        link = base / "innocent.json"
        os.symlink(outside, link)
        p = _make_provider(self.tmp)
        with self.assertRaises(ValueError):
            p.resolve_bundle_path("innocent.json")

    def test_deep_dotdot_still_inside_allowed(self):
        """Traversal that lands back inside base is fine."""
        p = _make_provider(self.tmp)
        out = p.resolve_bundle_path("sub/../fine.json")
        assert out == str(_base(self.tmp) / "fine.json")

    def test_enabled_bypass_returns_raw(self):
        """With enable_export_import=true the operator takes full
        responsibility — raw path passed through untouched."""
        p = _make_provider(self.tmp, export_enabled=True)
        assert p.resolve_bundle_path("/etc/passwd") == "/etc/passwd"

    def test_disabled_string_variants(self):
        """The wizard stores choice strings; every non-'true' value must
        count as disabled (bare truthiness made 'false' count as enabled)."""
        for val in ("false", "False", "TRUE" if False else " true ", "", "1", "yes"):
            p = _make_provider(self.tmp)
            p._config["enable_export_import"] = val
            if val.strip().lower() == "true":
                self.assertEqual(p.resolve_bundle_path("/etc/passwd"), "/etc/passwd")
            else:
                with self.assertRaises(ValueError):
                    p.resolve_bundle_path("../evil")


class TestIsAvailable(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="memex8-avail-")
        self._env_patcher = patch.dict(os.environ, {}, clear=False)
        self._env_patcher.start()
        os.environ.pop("MEMEX8_API_KEY", None)

    def tearDown(self):
        self._env_patcher.stop()

    def test_env_var_true(self):
        os.environ["MEMEX8_API_KEY"] = "k"
        p = plugin.Memex8MemoryProvider()
        self._modify_home(p, self.tmp)
        assert p.is_available() is True

    def test_no_env_no_json_false(self):
        p = plugin.Memex8MemoryProvider()
        self._modify_home(p, self.tmp)
        assert p.is_available() is False

    def test_json_fallback_true(self):
        """Regression: config-file-only users were reported unavailable
        even though initialize() would have found the key."""
        p = plugin.Memex8MemoryProvider()
        self._modify_home(p, self.tmp)
        (Path(self.tmp) / "memex8.json").write_text(json.dumps({"api_key": "k"}))
        assert p.is_available() is True

    def test_json_with_empty_key_false(self):
        p = plugin.Memex8MemoryProvider()
        self._modify_home(p, self.tmp)
        (Path(self.tmp) / "memex8.json").write_text(json.dumps({"api_key": ""}))
        assert p.is_available() is False

    def test_json_corrupt_false_not_crash(self):
        p = plugin.Memex8MemoryProvider()
        self._modify_home(p, self.tmp)
        (Path(self.tmp) / "memex8.json").write_text("{not json")
        assert p.is_available() is False

    @staticmethod
    def _modify_home(p, home: str):
        p._hermes_home = home


if __name__ == "__main__":
    unittest.main(verbosity=2)
