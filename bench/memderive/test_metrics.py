import unittest

from bm25_ref import BM25, tokenize
from metrics import full_witness, obsolete_hit, summarize, support_recall, witness_recall

G = {
    "valid_support": ["e1", "e2", "e3", "e4"],
    "witness": ["e3", "e4"],
    "alternative_witnesses": [["e1", "e9"]],
    "invalid_evidence": ["e7"],
}


class Metrics(unittest.TestCase):
    def test_support_recall(self):
        self.assertEqual(support_recall(["e1", "e3", "x"], G), 0.5)

    def test_witness_recall_takes_best_witness(self):
        self.assertEqual(witness_recall(["e3", "e1"], G), 0.5)
        self.assertEqual(witness_recall(["e3", "e4"], G), 1.0)

    def test_full_witness_accepts_alternative(self):
        self.assertTrue(full_witness(["e1", "e9", "zz"], G))
        self.assertFalse(full_witness(["e1", "e3"], G))

    def test_obsolete_hit(self):
        self.assertTrue(obsolete_hit(["e7"], G))
        self.assertFalse(obsolete_hit(["e1"], G))

    def test_empty_gold_is_vacuous(self):
        self.assertEqual(support_recall([], {}), 1.0)
        self.assertEqual(witness_recall([], {}), 1.0)
        self.assertTrue(full_witness([], {}))

    def test_summarize_by_family(self):
        rows = [
            {"family": "temporal", "support": 1.0, "witness": 1.0, "full": True, "obsolete": False},
            {"family": "rules", "support": 0.0, "witness": 0.5, "full": False, "obsolete": True},
        ]
        s = summarize(rows)
        self.assertEqual(s["all"]["support_recall"], 50.0)
        self.assertEqual(s["all"]["n"], 2)
        self.assertEqual(s["rules"]["obsolete_hit"], 100.0)
        self.assertEqual(s["temporal"]["full_witness"], 100.0)


class Bm25Ref(unittest.TestCase):
    def test_tokenize_drops_stopwords_keeps_digits(self):
        self.assertEqual(tokenize("The Trip-Grus to Corby 2"), ["trip", "grus", "corby", "2"])

    def test_rare_term_ranks_first(self):
        idx = BM25({
            "a": "meeting notes about the project budget",
            "b": "Zephyr router flashed with OpenWrt firmware",
            "c": "project budget meeting again",
        })
        self.assertEqual(idx.search("zephyr openwrt", 3)[0], "b")

    def test_no_match_returns_empty(self):
        self.assertEqual(BM25({"a": "alpha"}).search("omega", 5), [])

    def test_ties_are_deterministic(self):
        idx = BM25({"b": "alpha", "a": "alpha"})
        self.assertEqual(idx.search("alpha", 2), ["a", "b"])


if __name__ == "__main__":
    unittest.main()
