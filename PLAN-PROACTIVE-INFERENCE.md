# Plan: Proactive Memory Inference & Gap Detection

> Version: 1.0 | Date: 2026-05-02 | Feature Branch: `feature/memory-evolution`

## Problem Statement

Currently memex8 passively stores and retrieves memories. The system knows:
- **Frequency**: How often a topic is touched
- **Recency**: When it was last accessed
- **Importance**: Via upvotes and decay curves

But it does NOT know:
- **What else SHOULD be connected** (gap detection)
- **Patterns across sessions** that suggest implied needs
- **Proactive suggestions** surfaced before the user asks

## Vision

memex8 should function like a thoughtful collaborator who, after seeing you work on Server A, might say:

> "I notice you connected to Server A and mentioned Server B. Should Server B get the same treatment?"

This mimics how human memory works — not just storing what happened, but building understanding of *what matters and what's missing*.

---

## Architecture

### New Components

```
src/
├── engine/
│   ├── inference.rs        # NEW: Gap detection & proactive suggestion engine
│   ├── reactions.rs       # NEW: Reaction/engagement tracking from text
│   └── associations.rs    # ENHANCE: Richer semantic association building
```

### Data Model Extensions

**MemoryPoint gains:**
```rust
// In storage/qdrant.rs - MemoryPoint
/// Reaction score inferred from engagement patterns (-1.0 to 1.0)
pub reaction_score: f32,
/// Topic clusters this memory belongs to
pub topic_clusters: Vec<String>,
/// Memories this is inferentially linked to (not just semantically similar)
pub inferred_links: Vec<InferredLink>,
```

**New struct:**
```rust
pub struct InferredLink {
    pub target_memory_id: String,
    pub link_type: LinkType,  // FillGap, TemporalNext, Prerequisite, Companion
    pub confidence: f32,
    pub suggestion_text: String,
}

pub enum LinkType {
    FillGap,      // "You mentioned X but not Y — Y is usually related"
    TemporalNext, // "After doing X, people typically do Y"
    Prerequisite, // "To do X, you might need Y first"
    Companion,    // "X and Y are often configured together"
}
```

---

## Phase 1: Reaction Tracking

### 1.1 Reaction Score Inference

**File: `src/engine/reactions.rs`** (new)

```rust
/// Analyze text to infer emotional/engagement reaction score.
/// Returns -1.0 (negative) to 1.0 (positive).
pub fn infer_reaction(text: &str) -> f32 {
    // Positive signals: "great", "awesome", "perfect", "thanks", "love it"
    // Negative signals: "frustrated", "annoying", "broken", "hate", "terrible"
    // Engagement signals: long detailed responses, follow-up questions
    // Passive signals: one-word answers, "ok", "sure"
}
```

**Integration:**
- When `memex8_store` is called, infer reaction from content
- When memories are accessed via `memex8_search` or `memex8_recall`, track engagement type
- Store reaction as `reaction_score` in MemoryPoint payload

### 1.2 Reaction-Aware Importance

**In `src/engine/slumber.rs` — decay_memories():**

```rust
// Currently:
score = importance * recency * (1 + access_count * 0.05)

// New:
score = importance * recency * (1 + access_count * 0.05) * reaction_boost

// Where:
reaction_boost = 1.0 + (reaction_score * 0.3)  // -0.3 to +0.3 range
```

---

## Phase 2: Semantic Association Engine

### 2.1 Enhanced Association Building

**File: `src/engine/associations.rs`** (new, replacing stub in graph.rs)

```rust
/// Build rich semantic associations between memories.
/// Returns for each memory a list of inferred links with types and confidence.
pub async fn build_inferred_associations(
    memories: &[MemoryPoint],
) -> HashMap<String, Vec<InferredLink>> {
    // For each pair of memories, classify the relationship type:
    // 1. Semantic similarity (cosine > 0.8) → Companion
    // 2. Temporal co-occurrence (same session, different topics) → TemporalNext
    // 3. Prerequisite pattern ("setup X" then "configure Y") → Prerequisite
    // 4. Gap pattern (Topic A mentioned frequently, Topic B never) → FillGap
    
    // Uses word co-occurrence matrices and temporal session tracking
}
```

### 2.2 Topic Cluster Detection

**New in `src/engine/associations.rs`:**

```rust
/// Detect topic clusters using k-means on memory vectors.
/// Returns cluster IDs and top keywords per cluster.
pub async fn detect_topic_clusters(
    memories: &[MemoryPointWithVector],
    k: usize,
) -> Vec<TopicCluster> {
    // Run k-means clustering on memory vectors
    // Extract top TF-IDF keywords per cluster
    // Assign cluster IDs to memories
}
```

### 2.3 Gap Detection Logic

**In `build_inferred_associations()`:**

```rust
// For each topic cluster:
// 1. Find memories with high importance + access_count
// 2. Check if related companion topics are absent or low-importance
// 3. If gap detected with high confidence → create FillGap inferred_link

// Example:
// Cluster "server-deployment" has: Server A (importance 0.9), Config (importance 0.7)
// Gap: Server B exists in memories but is not connected to deployment setup
// → Suggest: "Server B might need the same deployment configuration"
```

---

## Phase 3: Inference & Suggestion Engine

### 3.1 Proactive Suggestion API

**File: `src/api/routes/inference.rs`** (new)

**Endpoint: `POST /api/v1/inference/suggest`**

```rust
/// Given a new memory or topic context, return proactive suggestions.
/// 
/// Request:
/// {
///   "topic": "Server A",  // or "memory_id": "..."
///   "limit": 5
/// }
///
/// Response:
/// {
///   "suggestions": [
///     {
///       "type": "FillGap",
///       "target": "Server B",
///       "confidence": 0.78,
///       "reasoning": "You configured Server A with Twilio but Server B has no Twilio setup",
///       "action": "Would you like me to apply the same Twilio configuration to Server B?"
///     }
///   ]
/// }
```

### 3.2 MCP Tool: `memex8_infer`

**File: `src/mcp/tools.rs`** — add to tool list:

```json
{
  "name": "memex8_infer",
  "description": "Given context about a topic, infer related gaps and suggest follow-up actions",
  "inputSchema": {
    "type": "object",
    "properties": {
      "topic": {"type": "string", "description": "Topic to analyze for gaps"},
      "memory_id": {"type": "string", "description": "Or a specific memory ID to analyze"},
      "limit": {"type": "number", "description": "Max suggestions (default 5)"}
    }
  }
}
```

### 3.3 CLI Command: `memex8 infer`

**File: `src/main.rs`** — add subcommand:

```bash
memex8 infer --topic "Twilio"        # Suggest related gaps
memex8 infer --memory-id "abc-123"   # From a specific memory
memex8 infer --watch                 # Enable proactive suggestions during session
```

---

## Phase 4: Integration with Slumber

### 4.1 New Slumber Phase

**In `src/engine/slumber.rs` — `run_full_pipeline()`:**

```rust
// Phase 10: Build inferred associations (runs after Phase 9: associations)
// Replaces simple association_strengths with richer InferredLink structures
tracing::info!("💤 Slumber phase 10: Build inferred associations");
report.inferred_links_built = self.build_inferred_associations_phase().await?;
```

### 4.2 Gap Analysis Report

After association building, generate a gap report stored in Qdrant:

```rust
pub struct GapAnalysisReport {
    pub id: String,
    pub topic_cluster: String,
    pub high_importance_topic: String,
    pub missing_related_topic: String,
    pub confidence: f32,
    pub created_at: String,
    pub resolved: bool,  // User acted on it or dismissed it
}
```

Store in a new `gaps` Qdrant collection.

### 4.3 Suggestion Resolution Tracking

When user acts on a suggestion:
```bash
memex8 infer --resolve "gap-id"  # Mark as resolved
memex8 infer --dismiss "gap-id" # Dismiss and suppress similar
```

---

## Phase 5: Session-End Extraction (Automation)

### 5.1 Session Summary Tool

**File: `src/engine/session.rs`** (new)

```rust
/// Called at end of Hermes Agent session (or via cron).
/// Analyzes recent memories and:
/// 1. Extracts key decisions made
/// 2. Identifies follow-up items
/// 3. Updates importance based on session engagement
/// 4. Triggers inference for gap detection
pub async fn session_end_extract() -> anyhow::Result<SessionSummary> {
    // Get memories from last N hours
    // Group by topic
    // LLM-assisted extraction of decisions + follow-ups
    // Store as special "session-summary" memories with high importance
}
```

### 5.2 Cron-Triggered Inference

**In `src/engine/scheduler.rs`:**

```rust
// After slumber consolidation completes:
// Run gap detection on top-10 high-importance clusters
// Store any new gaps in the gaps collection
// If API server is running, optionally push to notification channel
```

---

## Implementation Order

### Step 1: Reaction Tracking (~2 hours)
- [ ] Create `src/engine/reactions.rs` with `infer_reaction()` function
- [ ] Add `reaction_score` field to `MemoryPoint` in `storage/qdrant.rs`
- [ ] Add migration to add `reaction_score` to existing memories
- [ ] Update `memex8_store` to infer and store reaction score
- [ ] Update `decay_memories()` to use reaction_boost
- [ ] Test: Store memories with varying emotional content, verify scoring

### Step 2: Semantic Associations (~3 hours)
- [ ] Create `src/engine/associations.rs`
- [ ] Implement `build_semantic_associations()` 
- [ ] Implement `detect_topic_clusters()` using k-means
- [ ] Update `MemoryPoint` to include `topic_clusters: Vec<String>`
- [ ] Integrate into slumber Phase 9 → Phase 10
- [ ] Test: Verify related memories get linked after slumber

### Step 3: Gap Detection (~3 hours)
- [ ] Implement `detect_gaps()` in associations.rs
- [ ] Create `gaps` Qdrant collection
- [ ] Implement `GapAnalysisReport` storage/retrieval
- [ ] Test: Add memories about Server A setup, verify gap suggested for Server B

### Step 4: Inference API & MCP Tool (~2 hours)
- [ ] Create `src/api/routes/inference.rs`
- [ ] Implement `POST /api/v1/inference/suggest`
- [ ] Add `memex8_infer` to MCP tools
- [ ] Add `memex8 infer` CLI command
- [ ] Test: API returns sensible gap suggestions

### Step 5: Suggestion Resolution (~1 hour)
- [ ] Add `resolve` and `dismiss` endpoints
- [ ] Add `memex8 infer --resolve` CLI
- [ ] Track resolution in gaps collection
- [ ] Test: Resolve a gap, verify it stops appearing

### Step 6: Session-End Extraction (~2 hours)
- [ ] Create `src/engine/session.rs`
- [ ] Implement `session_end_extract()`
- [ ] Add cron-triggered extraction option
- [ ] Integrate with Hermes Agent session hooks
- [ ] Test: Run extraction, verify decisions/follow-ups extracted

---

## Configuration

**In `config.toml`:**

```toml
[inference]
enabled = true
gap_confidence_threshold = 0.6
max_suggestions_per_topic = 5
reaction_boost_weight = 0.3

[inference.notifications]
enabled = false
# webhook_url = "https://example.com/webhook"
```

---

## Backward Compatibility

- All new fields have defaults (empty vectors, 0.0 scores)
- Existing memories are migrated with null/zero values
- Gap collection is created on first use if missing
- Feature can be disabled entirely via config

---

## Testing Strategy

1. **Unit tests**: reaction scoring, association building, gap detection
2. **Integration tests**: Store memories → run slumber → verify associations
3. **Manual testing**: 
   - Store memories about Twilio on Server A
   - Ask for suggestions on Server A
   - Verify Server B suggestion appears
   - Resolve the gap
   - Verify it stops appearing

---

## Effort Estimate

| Phase | Hours |
|-------|-------|
| 1. Reaction Tracking | 2 |
| 2. Semantic Associations | 3 |
| 3. Gap Detection | 3 |
| 4. Inference API + MCP | 2 |
| 5. Suggestion Resolution | 1 |
| 6. Session-End Extraction | 2 |
| **Total** | **13** |

---

## Files to Modify/Create

**New files:**
- `src/engine/reactions.rs` (~100 lines)
- `src/engine/associations.rs` (~400 lines)
- `src/api/routes/inference.rs` (~200 lines)
- `src/engine/session.rs` (~150 lines)

**Modified files:**
- `src/storage/qdrant.rs` — add new fields to MemoryPoint
- `src/engine/slumber.rs` — add Phase 10, update decay
- `src/engine/mod.rs` — export new modules
- `src/mcp/tools.rs` — add memex8_infer
- `src/main.rs` — add `infer` subcommand
- `src/config.rs` — add InferenceConfig
- `config.example.toml` — add inference section
- `PLAN.md` — reference this plan
