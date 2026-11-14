"""Dreams end to end against disposable Qdrant and a mock LLM.

Run after cargo build: python3 tests/test_dreams.py

Proves the hard rule: a dream is stored outside the memories collection and no
search surface (search, recall, list, timeline) can return it.
"""
import json
import unittest
import urllib.error

from test_consolidation import fixture

PHRASE = "velvet lighthouse"


def memories_total(f):
    return f["api"]("/stats")["total_memories"]


def dream_reply(ids, title="The Lantern Tide"):
    return json.dumps({
        "title": title,
        "dream": f"A {PHRASE} hums while the tide files itself into drawers.",
        "waking_note": "Two of these memories are about the same project.",
        "waking_note_ids": ids + ["not-a-seed-id"],
    })


def seed(f, n=6):
    return [f["api"]("/memories", {"content": f"Project note number {i}: the build uses cargo"}, "POST")["id"]
            for i in range(n)]


class DreamTests(unittest.TestCase):
    def test_generate_quarantine_keep_and_discard(self):
        with fixture() as f:
            ids = seed(f)
            total = memories_total(f)
            f["state"]["chat_response"] = dream_reply(ids[:2])

            d = f["api"]("/dreams", {}, "POST")
            self.assertEqual(d["status"], "pending")
            self.assertEqual(d["title"], "The Lantern Tide")
            self.assertEqual(d["waking_note_ids"], ids[:2])  # the invented id is dropped
            self.assertTrue(set(d["seed_ids"]) <= set(ids))
            self.assertEqual(f["api"]("/memories/" + d["waking_note_ids"][0])["id"], ids[0])

            # Stored as its own file on the data volume, not as a memory.
            files = list((f["work"] / "dreams").glob("*.json"))
            self.assertEqual(len(files), 1)
            self.assertEqual(memories_total(f), total)

            # Quarantine: no search surface returns dream text.
            hit = f["api"]("/memories/search", {"query": PHRASE, "limit": 50, "min_score": 0.0}, "POST")
            self.assertFalse(any(PHRASE in (r.get("content") or "") for r in hit["results"]))
            recall = json.dumps(f["api"]("/memories/recall?limit=100"))
            listing = json.dumps(f["api"]("/memories?limit=500"))
            self.assertNotIn(PHRASE, recall)
            self.assertNotIn(PHRASE, listing)
            self.assertNotIn("Lantern Tide", listing)

            # API: list, filter, get.
            lst = f["api"]("/dreams")
            self.assertEqual(lst["pending"], 1)
            self.assertEqual([x["id"] for x in lst["dreams"]], [d["id"]])
            self.assertIn(PHRASE, lst["dreams"][0]["preview"])
            self.assertEqual(len(f["api"]("/dreams?status=pending")["dreams"]), 1)
            self.assertEqual(len(f["api"]("/dreams?status=kept")["dreams"]), 0)
            with self.assertRaises(urllib.error.HTTPError) as bad:
                f["api"]("/dreams?status=bogus")
            self.assertEqual(bad.exception.code, 400)
            self.assertEqual(f["api"]("/dreams/" + d["id"])["dream"], d["dream"])

            # Keep it.
            kept = f["api"](f"/dreams/{d['id']}/keep", {}, "POST")
            self.assertEqual(kept["status"], "kept")
            self.assertEqual(f["api"]("/dreams")["pending"], 0)
            self.assertEqual(memories_total(f), total)  # keeping does not remember it

            # A second dream, discarded.
            f["state"]["chat_response"] = dream_reply(ids[2:3], title="Second")
            d2 = f["api"]("/dreams", {}, "POST")
            self.assertEqual(len(list((f["work"] / "dreams").glob("*.json"))), 2)
            f["api"]("/dreams/" + d2["id"], None, "DELETE")
            self.assertEqual(len(list((f["work"] / "dreams").glob("*.json"))), 1)
            with self.assertRaises(urllib.error.HTTPError) as gone:
                f["api"]("/dreams/" + d2["id"])
            self.assertEqual(gone.exception.code, 404)
            with self.assertRaises(urllib.error.HTTPError) as gone2:
                f["api"](f"/dreams/{d2['id']}/keep", {}, "POST")
            self.assertEqual(gone2.exception.code, 404)
            self.assertEqual(memories_total(f), total)

    def test_too_few_memories_is_a_clear_error_and_writes_nothing(self):
        with fixture() as f:
            seed(f, 3)
            f["state"]["chat_response"] = dream_reply([])
            with self.assertRaises(urllib.error.HTTPError) as e:
                f["api"]("/dreams", {}, "POST")
            self.assertEqual(e.exception.code, 422)
            self.assertIn("need at least 4", e.exception.read().decode())
            self.assertFalse(list((f["work"] / "dreams").glob("*.json")))

    def test_bad_model_reply_writes_nothing(self):
        with fixture() as f:
            seed(f)
            f["state"]["chat_response"] = "I refuse to dream."
            with self.assertRaises(urllib.error.HTTPError) as e:
                f["api"]("/dreams", {}, "POST")
            self.assertEqual(e.exception.code, 422)
            self.assertIn("wasn't a usable dream", e.exception.read().decode())
            self.assertFalse(list((f["work"] / "dreams").glob("*.json")))

    def test_settings_expose_dreams_as_off_with_a_choice_field(self):
        with fixture() as f:
            fields = {x["key"]: x for x in f["api"]("/settings")["fields"]}
            self.assertEqual(fields["dream.enabled"]["value"], False)
            self.assertEqual(fields["dream.frequency"]["type"], "choice")
            self.assertEqual(fields["dream.frequency"]["options"], ["daily", "weekly"])
            self.assertEqual(fields["dream.enabled"]["group"], "Dreams")


if __name__ == "__main__":
    unittest.main()
