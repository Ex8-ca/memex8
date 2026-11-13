"""Retrieval metrics against DeriveMem gold. Pure functions, no I/O.

All metrics ask whether the retrieved event IDs contain what a correct answer
needs, so they measure gathering (the paper's main bottleneck) without an LLM.
"""


def support_recall(retrieved: list[str], gold: dict) -> float:
    """Share of the valid support events that were retrieved."""
    support = set(gold.get("valid_support") or gold.get("gold_evidence") or [])
    return len(support & set(retrieved)) / len(support) if support else 1.0


def witnesses(gold: dict) -> list[list[str]]:
    """The primary witness plus alternatives; each is a minimal sufficient event set."""
    ws = [gold.get("witness") or []] + list(gold.get("alternative_witnesses") or [])
    return [w for w in ws if w]


def witness_recall(retrieved: list[str], gold: dict) -> float:
    """Best coverage of any single witness."""
    got = set(retrieved)
    ws = witnesses(gold)
    return max(len(got & set(w)) / len(w) for w in ws) if ws else 1.0


def full_witness(retrieved: list[str], gold: dict) -> bool:
    """True when some complete witness was retrieved (answerable from context alone)."""
    got = set(retrieved)
    ws = witnesses(gold)
    return any(set(w) <= got for w in ws) if ws else True


def obsolete_hit(retrieved: list[str], gold: dict) -> bool:
    """True when a superseded or invalid event was retrieved."""
    return bool(set(gold.get("invalid_evidence") or []) & set(retrieved))


def score(retrieved: list[str], gold: dict) -> dict:
    return {
        "family": gold.get("family", "unknown"),
        "support": support_recall(retrieved, gold),
        "witness": witness_recall(retrieved, gold),
        "full": full_witness(retrieved, gold),
        "obsolete": obsolete_hit(retrieved, gold),
    }


def summarize(rows: list[dict]) -> dict:
    """Aggregate per-query rows overall and per family, as percentages."""

    def agg(rs: list[dict]) -> dict:
        n = len(rs) or 1
        return {
            "n": len(rs),
            "support_recall": round(100 * sum(r["support"] for r in rs) / n, 2),
            "witness_recall": round(100 * sum(r["witness"] for r in rs) / n, 2),
            "full_witness": round(100 * sum(r["full"] for r in rs) / n, 2),
            "obsolete_hit": round(100 * sum(r["obsolete"] for r in rs) / n, 2),
        }

    out = {"all": agg(rows)}
    for fam in sorted({r["family"] for r in rows}):
        out[fam] = agg([r for r in rows if r["family"] == fam])
    return out
