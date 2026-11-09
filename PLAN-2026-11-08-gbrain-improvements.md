# memex8 next implementation plan — lessons from gbrain

Date: 2026-11-08 (America/Regina)  
Status: proposed; ready to start P0  
Baseline: `dc00ff603aa5388d1bcfd70df720509194a1c5ec` (PR #32)

The previous [execution roadmap](PLAN-2026-11-08-execution-roadmap.md) is merged
and deployed on 192.168.1.3. This roadmap borrows selected gbrain patterns for
memex8's Rust/Qdrant architecture. It authorizes no new deployment, paid evaluation,
production cleanup, or change to the current duplicate-removal dry-run setting.
Each implementation package below is a separate branch and PR against updated main.

## What we want to change

1. Automatic capture stores new, useful information once, including after retries.
2. Search result slots represent distinct evidence, with good exact-name retrieval.
3. Corrections preserve history and are distinguishable from duplicates.
4. Maintenance has observable limits and can recover from interruption.
5. Optional generated answers cite accessible evidence and disclose gaps.

Observed motivation: during the deployment smoke test, ten retrieved memories
had identical bodies. Pre-upgrade slumber logs also showed individual phases
running for tens of minutes. These are observations from one deployment, not a
measured corpus-wide duplicate rate or a benchmark of the new release.

## Delivery order and ownership boundaries

| Package | Branch | Dependency | Reviewable result |
|---|---|---|---|
| P0 | `test/memory-quality-baseline` | main | Repeatable retrieval/capture quality report |
| P1 | `fix/dedup-preserve-corrections` | P0 | Semantic near matches cannot alone authorize removal |
| P2 | `feat/idempotent-session-capture` | P1 | Replay-safe session/compaction capture |
| P3 | `feat/search-result-diversity` | P0, P1 | Repeated evidence occupies one result group |
| P4 | `feat/capture-quality-policy` | P2, P3 | Observable filtering of routine automatic captures |
| P5 | `feat/hybrid-memory-search` | P0, P3 | Keyword + vector fusion and honest degradation |
| P6 | `feat/entity-alias-resolution` | P5 | Scoped, explicit entity aliases |
| P7 | `feat/slumber-budgets-progress` | P0 | Bounded phases and useful diagnostics |
| P8 | `feat/resumable-incremental-slumber` | P7 | Durable progress and safe incremental work |
| P9 | `feat/temporal-memory-corrections` | P1, P6 | Current facts, superseded history, and validity intervals |
| P10 | `feat/cited-memory-answers` | P5, P9 | Optional evidence-backed answer tool |

Default sequence: P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 → P9 → P10.
P3 can land before P2 if capture work is blocked. P7/P8 can move earlier if
maintenance is the main operational bottleneck. Dependencies do not authorize
parallel agents, stacked PRs, or concurrent production writers.

## Shared requirements for every implementation PR

- Preserve existing default response contracts. Introduce retrieval changes behind
  opt-in flags first; activate defaults only after a recorded evaluation decision.
- Keep REST, both Rust MCP dispatchers and tool schemas, and Hermes client/schema/
  output behavior aligned. Include cross-client contract checks.
- Scope candidate discovery, grouping, aliases, citations and final results by the
  caller's existing authorization, visibility, realm and tag restrictions. A hidden
  row must not suppress a visible row or leak through a count, group or citation.
- New payload fields need backward-compatible defaults plus explicit Qdrant
  serialization/deserialization and appropriate indexes. Preserve metadata through
  import/export, backup/restore, consolidation, dedup recovery and undo.
- Preserve certification and private-text redaction. Agent inference or imported
  instructions must not upgrade themselves to human certification.
- All new side indexes and journals require a recovery strategy. Derived indexes
  can be rebuilt from Qdrant; durable capture/checkpoint state must be backed up.
- Document new configuration in `config.example.toml`, behavior in README, and
  completion in `IMPLEMENTATION_STATUS.md`. Mark this plan as work lands.
- Validate implementation with formatting, build, required Rust tests, focused
  disposable-Qdrant tests and relevant Hermes tests. Never test deletes on live data.
- Retain `dedup_dry_run=true`. A full slumber is mutating even in dedup dry-run;
  use detection-only inspection or an isolated restore for evaluation.

## P0 — Establish a memory-quality baseline

Likely files: `tests/`, new `evals/memory-quality/`, `scripts/`, CI workflow.

- [ ] Create at least 50 synthetic labeled queries across exact names/identifiers,
  paraphrases, duplicate-heavy results, realm/visibility isolation, corrections,
  temporal questions, capture replay, and Hermes→MCP continuity.
- [ ] Separate development cases from at least 15 held-out cases. Expected answers
  and evidence IDs are evaluator inputs, never text supplied to the retriever.
- [ ] Add `python3 scripts/eval_memory_quality.py --suite smoke --output REPORT`
  with pinned fixtures, search settings, seed, model identity and schema version.
  This is a new command to implement, not an existing executable.
- [ ] Run against disposable Qdrant. Default CI uses deterministic provider mocks;
  genuine semantic-quality runs are a separately enabled, budgeted lane with a
  fixed embedding model. Mock scores must never be reported as real-model quality.
- [ ] Report Recall@5, MRR, distinct-evidence@10, repeated-result fraction, capture
  replay counts, correction retention, p50/p95 latency, response bytes and provider
  calls. Keep per-query results and compare against a checked-in baseline.
- [ ] Hard gates: zero scope leaks, no lost certified/corrected facts, and stable
  cross-client behavior. Establish performance budgets from the measured baseline.

Done when: two deterministic runs produce identical quality scores; CI publishes
an artifact; the duplicate-heavy fixture exposes the existing result crowding.

## P1 — Protect corrections during duplicate classification

Likely files: `src/engine/dedup.rs`, `src/engine/slumber.rs`,
`src/storage/consolidation.rs` (shared recovery journals), related tests.

- [ ] Audit exact and cosine-based candidate rules, then separate exact replays,
  possible paraphrases, and potentially changed claims in detection reports.
- [ ] Guard differences in negation, numbers, amounts, dates, named entities and
  ownership. These guards are necessary but insufficient to establish equivalence.
- [ ] Restrict automatic removal eligibility to proven exact normalized bodies
  in compatible scope/type, or explicit shared immutable evidence identity with
  unchanged claims. Keep uncertain cosine-only candidates in shadow reports.
- [ ] Apply the same decision at planning and the archive-before-delete recheck;
  preserve existing winner rules, certified protections, cooldowns and undo.
- [ ] Explain protected/uncertain candidates with stable reason codes and counters.

Done when: high-cosine pairs such as “enabled/disabled”, “$100/$1,000”, changed
addresses and “Lisbon/Porto” survive inspection AND removal-enabled isolated tests;
proven duplicates still archive and restore correctly. No live removal is enabled.

## P2 — Make session and compaction capture replay-safe

Likely files: `src/engine/session.rs`, session API, `src/storage/`, Hermes hooks.

- [ ] Inventory session-end, pre-compaction, webhook and scheduled capture paths.
  Trace whether the observed repeated bodies share a source event or are separate
  events. Session IDs alone must not collapse multiple genuine turns.
- [ ] Define redacted event identity from source, stable session/turn or segment
  identifiers, and content fingerprint. Version the identity policy. Use body hashes
  when source IDs are absent and document the bounded dedup scope.
- [ ] Persist an event/segment receipt and per-output progress before publication.
  Replays reuse committed results; partial retries finish only missing outputs.
- [ ] Use deterministic output identities or equivalent durable conflict handling
  so concurrent requests cannot both publish a second copy. An in-process lock
  alone is insufficient across REST and separately launched MCP processes.
- [ ] Distinguish replaying the same event from a new event with identical words.
  Intentional explicit remember calls retain their existing semantics.
- [ ] Return additive receipt/replayed/skipped counts. Include receipts in backups;
  support older callers without requiring a new mandatory parameter.

Done when: repeat a capture ten times, race it through two clients, and interrupt
it after its first output: the final logical outputs and extracted facts equal one
successful capture, with no missing outputs or repeated provider calls after a
completed receipt. A genuinely new turn is still captured.

## P3 — Give search results distinct evidence

Likely files: `src/engine/mod.rs`, `retrieval.rs`, `coverage.rs`, tool surfaces.

- [ ] Add an opt-in `diversify` search parameter, initially default false.
- [ ] Group proven exact normalized bodies and explicit unchanged evidence lineage
  inside accessible scope. Do not group solely by file-wide `source_hash`, common
  source file, shared topic, cosine similarity or any overlapping ancestor ID.
- [ ] Preserve complementary chunks and conflicting claims. Shared lineage by
  itself is not proof that two summaries carry identical information.
- [ ] Choose representatives deterministically, respecting certified status and
  existing relevance. Use bounded overfetch/refill BEFORE group pagination.
- [ ] Preserve compact row keys. Put additive group/member metadata outside the
  compact rows; expose IDs only for accessible group members.
- [ ] Report scan exhaustion/underfill explicitly; group before access tracking so
  suppressed copies receive no artificial reinforcement. Keep raw-cosine coverage
  independent of diversified ranking and page position.

Done when: a corpus with 20 copies plus nine distinct relevant memories returns
10 distinct groups for limit 10 when available; offset pages do not repeat groups
on an unchanged corpus; scope/filter tests pass on Qdrant and TurboVec. No stored
memories are deleted. Record baseline comparisons before choosing a default.

## P4 — Add a policy for automatic capture quality

Likely files: `src/engine/session.rs`, capture receipts, `src/config.rs`, Hermes.

- [ ] Introduce one shared capture policy with `observe` as the initial default and
  an explicit enforcing mode. Record decisions without logging private bodies.
- [ ] Identify acknowledgments, routine status chatter and repeated unchanged
  summaries; retain durable decisions, preferences, corrections and commitments.
- [ ] Apply policy to automatic lanes only. Explicit remember and user-requested
  certification bypass automatic notability filtering.
- [ ] Keep skipped-event receipts so retries do not repeatedly classify the same
  event. Policy changes need explicit versioned replay rules.
- [ ] Report captured/skipped/replayed/uncertain totals by source and reason; make
  an operator override possible without recovering a deleted original.

Done when: labeled explicit remembers and corrections are retained in every case;
the smoke corpus filters at least 90% of its routine-chatter cases with no labeled
durable-fact losses. Treat that target as a fixture gate, not a production claim.
Review observe-mode results before enabling enforcement on .3.

## P5 — Add keyword retrieval and rank fusion

Likely files: new `src/storage/lexical.rs`, `src/engine/retrieval.rs`, mutations.

- [ ] Time-box a storage spike: prefer a rebuildable Rust lexical side index
  (candidate: Tantivy) over changing the production Qdrant vector schema. Record
  dependency, disk, rebuild and migration costs before selecting the backend.
- [ ] Index redacted content, summaries and headings with stable memory IDs and
  content revisions. Register store/edit/import/recovery/delete/consolidation hooks;
  tolerate crashes through a durable dirty marker or replay/rebuild mechanism.
- [ ] Fetch bounded keyword and vector candidate pools, enforce filters on both,
  combine rankings with reciprocal-rank fusion, then diversify and paginate.
- [ ] Revalidate candidate payloads and revisions in Qdrant before returning them;
  stale lexical entries must not expose removed/private content or old facts.
- [ ] Add opt-in hybrid mode. Provide useful keyword-only results if embeddings
  are unavailable, with explicit degraded/unavailable semantic-coverage metadata.
  Never pass a fused score off as raw cosine or silently claim a semantic miss.
- [ ] Expose bounded per-arm timings, result provenance and failure reasons through
  opt-in explain output. Keep ordinary result payloads compact.

Done when: exact hostnames, commands, paths and error strings beat the vector-only
baseline on their labeled cases; paraphrase quality stays within the recorded
baseline gate; provider outages return honest lexical results; crash/rebuild and
all mutation/visibility tests pass. No external model is required for lexical mode.

## P6 — Add explicit entity aliases

Likely files: `src/engine/graph.rs`, storage, engine retrieval and tool surfaces.

- [ ] Add canonical entity IDs and explicit alias records scoped by project/realm
  and visibility. Start with user-supplied aliases and provenance; no automatic
  cross-entity merges.
- [ ] Add one alias-management operation, exposed consistently across interfaces,
  plus conflict reporting when a short alias names multiple entities.
- [ ] Inject canonical entity matches into the hybrid candidate pool and explain
  their origin; leave ambiguous names unresolved or return disambiguation choices.
- [ ] Reuse existing graph traversal and recorded edges. Alias equivalence must
  not invent typed relationships or certify a factual claim.

Done when: a fixture links `.3`, `192.168.1.3` and `ai5080` to one server; another
realm's `.3` stays distinct; private aliases remain private; ambiguous names do not
silently choose a person/server.

## P7 — Bound slumber work and expose progress

Likely files: `src/engine/slumber.rs`, `scheduler.rs`, `doctor.rs`, slumber API.

- [ ] Measure phase timings, rows scanned/changed, provider calls/tokens and errors.
  Extend the existing doctor/status interfaces with current phase, last success,
  index health, embedding identity and backup age when a backup location is known.
- [ ] Add validated run/phase work, wall-time and provider-call/token budgets.
  Dollar estimates are advisory unless backed by known pricing; local providers
  still obey time/work limits.
- [ ] Thread cancellation and provider deadlines into phases. Stop between atomic
  work units; report a non-interruptible operation honestly instead of claiming a
  hard deadline. Preserve in-flight recovery journals.
- [ ] Prevent scheduler overlap across processes with durable ownership/fencing;
  stale workers must not publish after their lease is lost.
- [ ] Report completed, budget-exhausted, cancelled and failed as distinct outcomes.

Done when: isolated runs stop scheduling new work at their cap, identify the last
completed unit, retain recoverable state, and remain observable via status/doctor.
Two schedulers cannot concurrently mutate the same maintenance work.

## P8 — Resume maintenance and process changed work

Likely files: maintenance journal/storage, `slumber.rs`, mutation paths, backup.

- [ ] Persist versioned run/phase checkpoints and per-memory input fingerprints.
  Make committed work units idempotent and recoverable before resuming a phase.
- [ ] Maintain durable dirty/version state from all mutations. For pairwise or
  realm-wide phases, define dependency invalidation; a changed member can dirty
  the realm or associated neighbors. Not every phase is safely per-memory.
- [ ] Process changed work incrementally and retain an explicit full-rebuild mode.
  Some passes require bounded periodic full scans; document which and why.
- [ ] Extend backup/restore and doctor for checkpoints and dirty state. Older
  backups trigger a safe rebuild, never a claim that everything is up to date.

Done when: interrupted/resumed and uninterrupted runs converge to equivalent
logical state; a second unchanged run avoids eligible expensive work; edit/delete/
restore invalidations are covered. Measure improvements on an isolated ~20k-memory
fixture rather than promising an unmeasured production speedup.

## P9 — Model corrections and fact validity

Likely files: memory payload, engine/storage correction operation, search/timeline.

- [ ] Add optional `event_at`, `valid_from`, `valid_until` and explicit supersession
  links with evidence and attribution. Preserve `ingested_at` for ingestion history.
- [ ] Start with an explicit correction operation. Authorize it using existing
  caller scope; an automatic inference cannot supersede a certified user fact.
- [ ] Validate intervals/cycles and journal multi-point updates so partial failure
  is recoverable. Retain old facts and distinguish disputed from superseded facts.
- [ ] Add opt-in current/as-of retrieval. Unknown legacy validity remains unknown;
  do not infer event dates from ingestion dates or claim complete historical coverage.
- [ ] Preserve the current timeline contract. Add event/valid-time behavior via
  explicit parameters, with separate recorded-time semantics and clear unknowns.

Done when: “use server A” → “use server B from November 1” retrieves B as current,
A for a known earlier interval, and both with correction provenance in history.
A newly imported old note never silently overrides a certified current fact.

## P10 — Produce optional cited answers

Likely files: new `src/engine/answer.rs`, REST/MCP/Hermes, provider configuration.

- [ ] Add an optional `memex8_answer` capability using the shared scoped hybrid
  retriever, diversified compact candidates, then selected full bodies.
- [ ] Bound context, time and provider usage. Require explicit provider setup;
  ordinary search retains its retrieval-only behavior and costs.
- [ ] Return answer text, citations tied to accessible memory IDs/revisions,
  conflicting evidence, freshness/validity limitations, and an insufficient-evidence
  outcome. A retrieval-strength score is not an answer-confidence score.
- [ ] Validate citation IDs/revisions and access before response; reject unknown
  citations. Evaluate claim support separately—valid IDs do not prove entailment.
- [ ] Treat memory text as quoted evidence, never tool instructions. Test imported
  instruction-like content and preserve source attribution. Do not store generated
  answers automatically or feed them back as fresh independent evidence.

Done when: a held-out multi-memory question cites its supporting facts; a changed
fact selects the valid evidence; contradictory evidence is disclosed; an unsupported
question produces an insufficient-evidence result with no invented citations.

## Reranking experiment — conditional follow-up after P5

Use the evaluation harness to compare hybrid alone with a bounded cross-encoder
reranker. Start with a small candidate pool and configurable timeout, provider and
fallback. Measure held-out quality, p95 latency, calls/cost, and provider failures.
Only propose a separate implementation PR if the measured gain justifies those
costs. No default paid provider and no transfer of gbrain benchmark claims to memex8.

## First work session: P0 only

1. Branch from updated main as `test/memory-quality-baseline`.
2. Add duplicate-heavy, corrected-fact, identifier, scope and capture-replay
   fixtures first; expand to the minimum corpus and held-out split above.
3. Implement the disposable evaluation runner, metrics schema and CI artifact.
4. Record the current baseline, including known failures; do not fix product code
   or relax expected outcomes to hide those failures. Baseline comparisons may
   permit existing failures; scope/privacy invariants remain strict gates.
5. Open the focused draft PR with the report and next package P1 identified.

## Rollout and completion checklist

- [ ] P0 baseline merged; P1 correction safety merged.
- [ ] P2–P4 capture and retrieval improvements merged and evaluated.
- [ ] P5/P6 hybrid and aliases merged with an explicit default-mode decision.
- [ ] P7/P8 maintenance limits and recovery drills passed on an isolated restore.
- [ ] P9 correction history and P10 evidence-backed answers evaluated.
- [ ] For each requested deployment: verify a fresh backup, preserve embedding
  identity/runtime settings, run scoped cross-client smoke tests, and document
  image/config/rollback paths. Existing backup exclusions still apply.
- [ ] Restart/reload clients where required, then verify actual tool availability;
  updating plugin files alone does not update an already running Hermes process.

Merge readiness and production rollout are separate gates. Default changes,
quality-policy enforcement and any duplicate removal require explicit rollout
choices supported by the recorded evaluation; production actions still need the
user's deployment/maintenance authorization.

## Source notes

Reviewed gbrain at `555b73a8079de8d7061884358652e19d37f7acdf` (MIT). Borrow patterns;
retain attribution/license if implementation copies source. Referenced behavior
is not a claim that gbrain solves every race or that its scores predict ours.

- [Capture cross-lane dedup and its documented limits](https://github.com/garrytan/gbrain/blob/555b73a8079de8d7061884358652e19d37f7acdf/docs/guides/ambient-writeback.md#duplicates-across-lanes)
- [Claim-divergence guards and shadow matching](https://github.com/garrytan/gbrain/blob/555b73a8079de8d7061884358652e19d37f7acdf/src/core/facts/capture-dedup.ts)
- [Hybrid retrieval, page pooling, aliases and benchmark limitations](https://github.com/garrytan/gbrain/blob/555b73a8079de8d7061884358652e19d37f7acdf/docs/architecture/RETRIEVAL.md)
- [Cross-harness memory conformance](https://github.com/garrytan/gbrain/blob/555b73a8079de8d7061884358652e19d37f7acdf/evals/brainbench/README.md)
- [Valid-time read contracts](https://github.com/garrytan/gbrain/blob/555b73a8079de8d7061884358652e19d37f7acdf/src/core/valid-time.ts)
- [Maintenance budget accounting](https://github.com/garrytan/gbrain/blob/555b73a8079de8d7061884358652e19d37f7acdf/src/core/cycle/budget-meter.ts)
