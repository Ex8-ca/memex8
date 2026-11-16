"""Conversation webhook idempotency against disposable Qdrant and a mock LLM.

Hosts re-send the same transcript: a long-lived session is archived in full at
every compaction or cache eviction, and an end-of-session hook can fire twice
for one rotation. Each send used to become another identical memory. The
webhook must store identical content once and return the existing ID, without
merging records that differ in type, tags or content.

Run after cargo build: python3 tests/test_webhook_idempotency.py
"""
import threading
import unittest

from test_consolidation import fixture


def total(f):
    return f["api"]("/stats")["total_memories"]


def post(f, summary, **extra):
    body = {"summary": summary, "source": "hermes", "platform": "cli", **extra}
    return f["api"]("/webhooks/conversation", body, "POST")


class WebhookIdempotencyTests(unittest.TestCase):
    def test_resending_a_transcript_stores_it_once(self):
        with fixture() as f:
            before = total(f)
            first = post(f, "User: archive this session\nAssistant: archived it.")
            again = post(f, "User: archive this session\nAssistant: archived it.")
            self.assertEqual(first["status"], "stored")
            self.assertFalse(first["deduplicated"])
            self.assertEqual(again["status"], "stored")
            self.assertTrue(again["deduplicated"])
            self.assertEqual(again["id"], first["id"])
            self.assertEqual(total(f), before + 1)

    def test_concurrent_identical_sends_store_one_memory(self):
        """Two hooks firing 0.4 s apart overlap: both lookups would miss before
        either write landed, so the lookup and write are serialized per hash."""
        with fixture() as f:
            before = total(f)
            summary = "User: rotate the session\nAssistant: rotated."
            results, errors = [], []
            gate = threading.Barrier(4)

            def send():
                try:
                    gate.wait()
                    results.append(post(f, summary))
                except Exception as e:  # surfaced below
                    errors.append(e)

            threads = [threading.Thread(target=send) for _ in range(4)]
            for t in threads:
                t.start()
            for t in threads:
                t.join(timeout=120)
            self.assertFalse(errors, errors)
            self.assertEqual(len({r["id"] for r in results}), 1, results)
            self.assertEqual(sum(not r["deduplicated"] for r in results), 1, results)
            self.assertEqual(total(f), before + 1)

    def test_records_that_differ_are_not_merged(self):
        with fixture() as f:
            before = total(f)
            summary = "User: keep this checkpoint\nAssistant: kept."
            base = post(f, summary)
            typed = post(f, summary, memory_type="artifact")
            tagged = post(f, summary, tags=["checkpoint"])
            other = post(f, summary + " Later note.")
            ids = {base["id"], typed["id"], tagged["id"], other["id"]}
            self.assertEqual(len(ids), 4, "distinct records were collapsed into one")
            self.assertFalse(any(r["deduplicated"] for r in (base, typed, tagged, other)))
            self.assertEqual(f["api"]("/memories/" + typed["id"])["memory_type"], "artifact")
            self.assertEqual(total(f), before + 4)


if __name__ == "__main__":
    unittest.main(verbosity=2)
