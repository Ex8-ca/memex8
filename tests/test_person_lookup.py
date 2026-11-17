"""Person lookup: "who is X" answers from long-lived facts naming X.

Dense similarity alone ranks a short profile fact below long transcripts that
repeat the name, so the fact can miss the candidate set entirely. These tests
pin the vectors so the noise outranks the fact by similarity, then prove that a
person question surfaces the fact first while other query shapes are unchanged.
"""
import math
import unittest

from test_consolidation import fixture


def vector(*values):
    return list(values) + [0.] * (64 - len(values))


def unit(sim):
    """A unit vector with cosine `sim` to the query axis."""
    return vector(sim, math.sqrt(1 - sim * sim))


class PersonLookupTests(unittest.TestCase):
    def test_person_question_ranks_naming_profile_fact_first(self):
        with fixture() as f:
            def put(content, sim, memory_type="general", visibility="private"):
                id_ = f["api"]("/memories", {"content": content, "memory_type": memory_type}, "POST")["id"]
                payload = {**f["api"]("/memories/" + id_), "memory_type": memory_type,
                           "visibility": visibility}
                f["q"]("/collections/memories/points?wait=true",
                       {"points": [{"id": id_, "vector": unit(sim), "payload": payload}]}, "PUT")
                return id_

            noise = [put(f"Transcript {i}: Avery mentioned the groceries and Avery laughed", .9 - i * .01)
                     for i in range(12)]
            fact = put("Avery Quinn is the user's sister", .2, "profile")
            other = put("Jordan Lee is the user's neighbour", .25, "profile")
            public_fact = put("Avery Quinn works as a nurse", .15, "profile", "public")

            for q in ["Who is Avery", "who's avery?", "Tell me about Avery", "Ambient Avery notes"]:
                f["state"]["embedding_vectors"][q] = vector(1)

            def search(query, **options):
                return f["api"]("/memories/search", {"query": query, "limit": 5, **options}, "POST")["results"]

            hits = search("Who is Avery")
            ids = [r["id"] for r in hits]
            self.assertEqual(ids[:2], [fact, public_fact], ids)
            self.assertNotIn(other, ids)  # long-lived, but does not name Avery
            self.assertEqual(ids[2:], noise[:3])  # the rest keep their similarity order
            for q in ["who's avery?", "Tell me about Avery"]:
                self.assertEqual([r["id"] for r in search(q)][:2], [fact, public_fact], q)

            # Visibility still applies to person-lookup candidates.
            public = [r["id"] for r in search("Who is Avery", visibility="public")]
            self.assertIn(public_fact, public)
            self.assertNotIn(fact, public)

            # Same embedding, but not a person question: ranking is untouched and the
            # sub-threshold fact stays out.
            plain = [r["id"] for r in search("Ambient Avery notes")]
            self.assertEqual(plain, noise[:5])

            # Tag-filtered searches never take the person-lookup path.
            tagged = search("Who is Avery", tags=["absent"])
            self.assertEqual(tagged, [])


if __name__ == "__main__":
    unittest.main()
