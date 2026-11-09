"""Cron turns store only real findings; cron prompts and silent runs never land."""
import threading
import unittest
from unittest import mock

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN

HINT = (
    "[IMPORTANT: You are running as a scheduled cron job. DELIVERY: Your final "
    "response will be automatically delivered to the user ...]\n\n"
    "[CURRENT RUN TIME: Monday]\n\nCheck Costco for deals."
)
SID = "cron_7aa658a3d27f_20261109_120000"


def _inline_thread(target, name=None):
    return threading.Thread(target=target, name=name)


class CronFilterTests(unittest.TestCase):
    def setUp(self):
        self.provider = PLUGIN.Memex8MemoryProvider()
        self.provider._config = {"auto_sync": True}
        self.client = mock.Mock()
        self.provider._get_client = lambda: self.client
        self.provider._is_breaker_open = lambda: False
        p = mock.patch.object(PLUGIN, "spawn_context_thread", side_effect=_inline_thread)
        p.start()
        self.addCleanup(p.stop)

    def _sync(self, user, assistant, sid=SID):
        self.provider.sync_turn(user, assistant, session_id=sid)
        if self.provider._sync_thread:
            self.provider._sync_thread.join(5)

    def test_silent_variants_dropped(self):
        for reply in ["[SILENT]", "  [silent]  ", "SILENT", "NO_REPLY",
                      "[SILENT] nothing new this tick",
                      "Checked the queue, nothing pending.\n\n[SILENT]",
                      "[CRON_FAILURE]\nTRMNL upload failed", ""]:
            self.client.reset_mock()
            self._sync(HINT, reply)
            self.client.store.assert_not_called()

    def test_finding_stored_without_prompt(self):
        self._sync(HINT, "Costco has the Beelink GTR9 at $899 (was $1,099).")
        self.client.store.assert_called_once()
        content = self.client.store.call_args.args[0]
        kwargs = self.client.store.call_args.kwargs
        self.assertEqual(content, "Costco has the Beelink GTR9 at $899 (was $1,099).")
        self.assertNotIn("IMPORTANT", content)
        self.assertNotIn("## User", content)
        self.assertIn("cron-finding", kwargs["tags"])
        self.assertIn("cron-job:7aa658a3d27f", kwargs["tags"])
        self.assertEqual(kwargs["source"], "hermes-cron")

    def test_detected_by_hint_without_cron_session_id(self):
        # skill-loading jobs put skill text ahead of the hint
        self._sync("Skill: deals\n\n" + HINT, "[SILENT]", sid="abc")
        self.client.store.assert_not_called()
        self._sync("Skill: deals\n\n" + HINT, "Found one deal.", sid="abc")
        self.assertEqual(self.client.store.call_args.args[0], "Found one deal.")

    def test_normal_turn_unchanged(self):
        self._sync("What did Warren and I bet on last weekend?", "Lions -3.5.", sid="20261109_x")
        self.client.store.assert_called_once()
        self.assertIn("## User", self.client.store.call_args.args[0])

    def test_session_end_skips_cron(self):
        self.client.ingest_conversation = mock.Mock()
        msgs = [{"role": "user", "content": HINT}, {"role": "assistant", "content": "Found a deal"}]
        self.provider.on_session_end(msgs)
        self.client.ingest_conversation.assert_not_called()

    def test_pre_compress_optional_skips_required_archives(self):
        self.client.ingest_conversation = mock.Mock(return_value="arch-1")
        msgs = [{"role": "user", "content": HINT}, {"role": "assistant", "content": "x"}]
        self.assertEqual(self.provider.on_pre_compress(msgs), "")
        self.client.ingest_conversation.assert_not_called()
        self.provider.on_pre_compress(msgs, require_checkpoint=True)
        self.client.ingest_conversation.assert_called_once()

    def test_realms_tool_strips_centroid(self):
        self.client.realms.return_value = [
            {"id": "a", "name": "A", "memory_count": 3, "centroid": [0.1] * 1536}]
        out = self.provider._dispatch(self.client, "memex8_realms", {})
        self.assertEqual(out, [{"id": "a", "name": "A", "memory_count": 3}])


if __name__ == "__main__":
    unittest.main()
