"""Tests for the pure parts of recheck_legacy_summaries.py (no network)."""
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import recheck_legacy_summaries as r  # noqa: E402

ORIGINALS = {
    "o1": "The router runs OpenWrt 23.05 and listens on port 4324.",
    "o2": "Backups go to the NAS every night at 04:00.",
}


class ParseAnswer(unittest.TestCase):
    def test_plain_json(self):
        a = r.parse_answer('{"facts":[{"fact":"x","quote":"y"}]}')
        self.assertEqual(a, [{"fact": "x", "quote": "y"}])

    def test_code_fence_and_chatter(self):
        text = 'Sure!\n```json\n{"facts":[{"fact":"x","quote":"MISSING"}]}\n```\nDone.'
        self.assertEqual(r.parse_answer(text), [{"fact": "x", "quote": "MISSING"}])

    def test_garbage_returns_none(self):
        self.assertIsNone(r.parse_answer("no json here"))
        self.assertIsNone(r.parse_answer('{"facts": "oops"}'))
        self.assertIsNone(r.parse_answer('{"other": []}'))


class QuoteSupport(unittest.TestCase):
    def test_exact_and_normalised(self):
        self.assertTrue(r.quote_supported("listens on port 4324", ORIGINALS))
        self.assertTrue(r.quote_supported("Listens  ON port\n4324", ORIGINALS))

    def test_invented_quote_is_rejected(self):
        self.assertFalse(r.quote_supported("listens on port 9999", ORIGINALS))

    def test_tiny_quotes_never_count(self):
        # "NAS" appears in an original, but a 3-char quote proves nothing.
        self.assertFalse(r.quote_supported("NAS", ORIGINALS))
        self.assertFalse(r.quote_supported("", ORIGINALS))

    def test_missing_marker(self):
        self.assertFalse(r.quote_supported("MISSING", ORIGINALS))


class Classify(unittest.TestCase):
    def test_no_originals_is_orphan(self):
        self.assertEqual(r.classify([], {})["class"], "orphan")

    def test_all_quoted_is_covered(self):
        facts = [
            {"fact": "router OS", "quote": "runs OpenWrt 23.05"},
            {"fact": "backup time", "quote": "every night at 04:00"},
        ]
        c = r.classify(facts, ORIGINALS)
        self.assertEqual(c["class"], "covered")
        self.assertEqual(c["missing"], [])

    def test_any_missing_is_partial(self):
        facts = [
            {"fact": "router OS", "quote": "runs OpenWrt 23.05"},
            {"fact": "owner prefers tea", "quote": "MISSING"},
        ]
        c = r.classify(facts, ORIGINALS)
        self.assertEqual(c["class"], "partial")
        self.assertEqual(c["missing"], ["owner prefers tea"])

    def test_unsupported_quote_counts_as_missing(self):
        facts = [{"fact": "port", "quote": "listens on port 9999"}]
        c = r.classify(facts, ORIGINALS)
        self.assertEqual(c["class"], "partial")
        self.assertEqual(c["missing"], ["port"])

    def test_empty_fact_list_is_not_covered(self):
        # An LLM that returns no facts proved nothing.
        self.assertEqual(r.classify([], ORIGINALS)["class"], "partial")


class Selection(unittest.TestCase):
    def mem(self, **kw):
        base = {"id": "a", "chunk_type": "consolidated", "consolidated_from": [], "memory_type": "profile"}
        return {**base, **kw}

    def test_legacy_summary_detection(self):
        self.assertTrue(r.is_legacy_summary(self.mem()))
        self.assertTrue(r.is_legacy_summary(self.mem(consolidated_from=None)))
        self.assertFalse(r.is_legacy_summary(self.mem(consolidated_from=["x"])))
        self.assertFalse(r.is_legacy_summary(self.mem(chunk_type="manual")))

    def test_certified_summaries_are_never_removed(self):
        self.assertTrue(r.is_certified(self.mem(certified_at="2026-11-01T00:00:00Z")))
        self.assertFalse(r.is_certified(self.mem(certified_at=None)))
        self.assertFalse(r.is_certified(self.mem()))

    def test_order_puts_profile_and_preference_first(self):
        mems = [self.mem(id="g", memory_type="general"), self.mem(id="p", memory_type="preference"),
                self.mem(id="f", memory_type="profile")]
        self.assertEqual([m["id"] for m in r.order_for_review(mems)], ["f", "p", "g"])


class OriginalsFilter(unittest.TestCase):
    """Regression: search results carry no chunk_type, so summaries slipped in as 'originals'
    and covered each other (both then removed). The filter must use the store's own id list."""

    RESULTS = [{"id": "s1"}, {"id": "o1"}, {"id": "s2"}, {"id": "self"}, {"id": "o2"}]

    def test_summaries_and_self_are_excluded_by_id(self):
        got = r.usable_originals(self.RESULTS, summary_ids={"s1", "s2", "self"}, self_id="self")
        self.assertEqual(sorted(got), ["o1", "o2"])

    def test_results_without_chunk_type_still_filtered(self):
        # none of RESULTS has a chunk_type key; filtering must not depend on it
        self.assertTrue(all("chunk_type" not in x for x in self.RESULTS))
        self.assertEqual(len(r.usable_originals(self.RESULTS, {"s1", "s2"}, "self")), 2)

    def test_apply_time_guard_rejects_missing_or_summary_originals(self):
        live = {"o1": {"chunk_type": "manual"}, "o2": {"chunk_type": "consolidated"}}
        self.assertTrue(r.originals_still_valid(["o1"], live))
        self.assertFalse(r.originals_still_valid(["o1", "gone"], live))
        self.assertFalse(r.originals_still_valid(["o1", "o2"], live))
        self.assertFalse(r.originals_still_valid([], live))


class BackupFormat(unittest.TestCase):
    def test_backup_is_a_json_array_memex8_import_reads(self):
        import tempfile
        recs = [{"id": "a", "content": "c"}, {"id": "b", "content": "d"}]
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "b.json")
            r.write_backup(p, recs)
            self.assertEqual(json.load(open(p)), recs)
            self.assertEqual(os.stat(p).st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
