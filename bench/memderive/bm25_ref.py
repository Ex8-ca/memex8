"""Event-level BM25 over a chronicle: the paper's strongest baseline, as a sanity anchor.

Tokenizer rules match src/engine/lexical.rs for ASCII text. (Rust's
`is_alphanumeric` also keeps non-ASCII letters; the bench data is English.)
"""
import math
import re
from collections import Counter

STOP = set(
    "a an and are as at be by for from has have in is it its of on or that the this to was were will with".split()
)


def tokenize(text: str) -> list[str]:
    toks = re.split(r"[^0-9a-z]+", text.lower())
    return [t for t in toks if t and t not in STOP and (len(t) > 1 or t.isdigit())]


class BM25:
    def __init__(self, docs: dict[str, str], k1: float = 1.2, b: float = 0.75):
        self.k1, self.b = k1, b
        self.tf = {i: Counter(tokenize(t)) for i, t in docs.items()}
        self.len = {i: sum(c.values()) for i, c in self.tf.items()}
        self.avg = (sum(self.len.values()) / len(self.len)) if self.len else 0.0
        self.df = Counter(t for c in self.tf.values() for t in c)
        self.n = len(self.tf)

    def search(self, query: str, k: int) -> list[str]:
        q = set(tokenize(query))
        scores = {}
        for i, c in self.tf.items():
            s = 0.0
            for t in q:
                f = c.get(t)
                if not f:
                    continue
                idf = math.log(1 + (self.n - self.df[t] + 0.5) / (self.df[t] + 0.5))
                s += idf * f * (self.k1 + 1) / (f + self.k1 * (1 - self.b + self.b * self.len[i] / self.avg))
            if s > 0:
                scores[i] = s
        return [i for i, _ in sorted(scores.items(), key=lambda x: (-x[1], x[0]))[:k]]
