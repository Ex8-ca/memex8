//! Dreams: on a schedule the user picks, weave a handful of memories into a short
//! dream plus a "waking note" naming one real connection between them.
//!
//! Hard rule: a dream is fiction built from memories, not a memory. Dreams live in
//! their own files (`<data dir>/dreams/`), never in the `memories` collection, so
//! search, recall, timeline, MCP and prefetch cannot return them. Nothing in this
//! module calls `store_memory*`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Timelike, Utc};
use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::config::DreamConfig;
use crate::storage::qdrant::MemoryPoint;

const FORGOTTEN_AFTER_DAYS: i64 = 30;
const RECENT_WITHIN_DAYS: i64 = 7;
pub const MIN_SEEDS: usize = 4;
const SEED_TEXT_CHARS: usize = 500;

// ───────────────────────────── seed selection ─────────────────────────────

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// When the memory was last touched; falls back to when it was ingested.
fn last_touched(m: &MemoryPoint) -> Option<DateTime<Utc>> {
    parse_ts(&m.last_accessed).or_else(|| parse_ts(&m.ingested_at))
}

/// A memory may seed a dream only if it has content, isn't a machine summary or
/// cron noise, and the user hasn't marked it private.
pub fn eligible(m: &MemoryPoint) -> bool {
    !m.content.trim().is_empty()
        && m.chunk_type != "consolidated"
        && m.memory_type != "dream"
        && !m.tags.iter().any(|t| t == "cron-finding")
        && !m.content.contains("[private]")
}

/// About half forgotten (untouched for 30+ days), a quarter recent (ingested in the
/// last 7 days), a quarter linked to chosen seeds through `related_memory_ids`; any
/// shortfall is filled from the remaining eligible memories. No duplicates, at most `n`.
pub fn pick_seeds(
    all: &[MemoryPoint],
    n: usize,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) -> Vec<MemoryPoint> {
    // Work in indices into `all` so there is no borrowing gymnastics.
    let mut pool: Vec<usize> = (0..all.len()).filter(|&i| eligible(&all[i])).collect();
    pool.shuffle(rng);
    let mut chosen: Vec<usize> = Vec::new();

    fn take(from: &[usize], count: usize, n: usize, chosen: &mut Vec<usize>) {
        let mut taken = 0;
        for &i in from {
            if taken >= count || chosen.len() >= n {
                break;
            }
            if !chosen.contains(&i) {
                chosen.push(i);
                taken += 1;
            }
        }
    }

    let forgotten: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|&i| {
            last_touched(&all[i]).is_some_and(|t| (now - t).num_days() >= FORGOTTEN_AFTER_DAYS)
        })
        .collect();
    take(&forgotten, n.div_ceil(2), n, &mut chosen);

    let recent: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|&i| {
            parse_ts(&all[i].ingested_at).is_some_and(|t| (now - t).num_days() < RECENT_WITHIN_DAYS)
        })
        .collect();
    take(&recent, n / 4, n, &mut chosen);

    let wanted: HashSet<&str> = chosen
        .iter()
        .flat_map(|&i| all[i].related_memory_ids.iter().map(String::as_str))
        .collect();
    let linked: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|&i| wanted.contains(all[i].id.as_str()))
        .collect();
    take(&linked, n / 4, n, &mut chosen);

    take(&pool, n, n, &mut chosen);
    chosen.into_iter().map(|i| all[i].clone()).collect()
}

// ─────────────────────────────── scheduling ───────────────────────────────

/// True only when dreaming is enabled, the local hour matches, and enough time has
/// passed since the last dream (daily: before today; weekly: 7+ days ago).
/// Called every minute from a wall-clock timer, not from the slumber tick.
pub fn dream_due(cfg: &DreamConfig, now_local: NaiveDateTime, last: Option<NaiveDate>) -> bool {
    if !cfg.enabled || now_local.hour() != cfg.hour {
        return false;
    }
    let today = now_local.date();
    match last {
        None => true,
        Some(l) => match cfg.frequency.as_str() {
            "weekly" => (today - l).num_days() >= 7,
            _ => l < today,
        },
    }
}

// ───────────────────────────────── store ──────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DreamStatus {
    Pending,
    Kept,
}

/// Files written before the status field existed load as kept.
fn legacy_kept() -> DreamStatus {
    DreamStatus::Kept
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dream {
    pub id: String,
    pub created_at: String,
    pub title: String,
    pub dream: String,
    pub waking_note: String,
    #[serde(default)]
    pub waking_note_ids: Vec<String>,
    #[serde(default)]
    pub seed_ids: Vec<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default = "legacy_kept")]
    pub status: DreamStatus,
}

impl Dream {
    pub fn new(parsed: ParsedDream, seed_ids: Vec<String>, model: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: Utc::now().to_rfc3339(),
            title: parsed.title,
            dream: parsed.dream,
            waking_note: parsed.waking_note,
            waking_note_ids: parsed.waking_note_ids,
            seed_ids,
            model: model.to_string(),
            status: DreamStatus::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DreamStore {
    dir: PathBuf,
}

impl DreamStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_for(&self, d: &Dream) -> PathBuf {
        let stamp = parse_ts(&d.created_at)
            .unwrap_or_else(Utc::now)
            .format("%Y%m%d-%H%M%S");
        self.dir
            .join(format!("{stamp}-{}.json", &d.id[..d.id.len().min(8)]))
    }

    /// Write via a temp file then rename, so a crash never leaves a half-written dream.
    pub fn save(&self, d: &Dream) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.file_for(d);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(d)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        v.sort();
        v.reverse(); // names start with a timestamp: newest first
        v
    }

    fn read(path: &Path) -> Option<Dream> {
        match std::fs::read(path).map(|b| serde_json::from_slice::<Dream>(&b)) {
            Ok(Ok(d)) => Some(d),
            _ => {
                tracing::warn!("Skipping unreadable dream file {}", path.display());
                None
            }
        }
    }

    /// Newest first; corrupt files are skipped with a warning.
    pub fn list(&self, limit: usize, status: Option<DreamStatus>) -> Vec<Dream> {
        self.files()
            .iter()
            .filter_map(|p| Self::read(p))
            .filter(|d| status.is_none_or(|s| d.status == s))
            .take(limit)
            .collect()
    }

    fn find(&self, id: &str) -> Option<(PathBuf, Dream)> {
        self.files()
            .into_iter()
            .filter_map(|p| Self::read(&p).map(|d| (p, d)))
            .find(|(_, d)| d.id == id)
    }

    pub fn get(&self, id: &str) -> Option<Dream> {
        self.find(id).map(|(_, d)| d)
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        match self.find(id) {
            Some((p, _)) => {
                std::fs::remove_file(p)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Mark a dream kept. Returns the updated dream, or None if it doesn't exist.
    pub fn keep(&self, id: &str) -> anyhow::Result<Option<Dream>> {
        let Some((path, mut d)) = self.find(id) else {
            return Ok(None);
        };
        d.status = DreamStatus::Kept;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&d)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(Some(d))
    }

    pub fn pending_count(&self) -> usize {
        self.list(usize::MAX, Some(DreamStatus::Pending)).len()
    }

    /// Local date of the newest dream, whatever its status.
    pub fn last_date(&self) -> Option<NaiveDate> {
        self.list(1, None)
            .first()
            .and_then(|d| parse_ts(&d.created_at))
            .map(|t| t.with_timezone(&Local).date_naive())
    }

    /// Delete pending dreams older than `days`. Returns how many were removed.
    pub fn expire_pending(&self, days: i64, now: DateTime<Utc>) -> usize {
        let mut n = 0;
        for p in self.files() {
            if let Some(d) = Self::read(&p) {
                let old = parse_ts(&d.created_at).is_some_and(|t| (now - t).num_days() >= days);
                if d.status == DreamStatus::Pending && old && std::fs::remove_file(&p).is_ok() {
                    n += 1;
                }
            }
        }
        n
    }
}

// ──────────────────────────── prompt and parsing ──────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDream {
    pub title: String,
    pub dream: String,
    pub waking_note: String,
    pub waking_note_ids: Vec<String>,
}

pub fn system_prompt(max_words: usize) -> String {
    format!(
        "You are memex8, dreaming. Weave the numbered memories below into one short, surreal dream \
         (at most {max_words} words). Recombine them freely; it's a dream, not a summary. Then write a \
         waking note: one real connection between two or more of the memories, stated plainly, without \
         inventing facts. Answer as JSON only: \
         {{\"title\": \"...\", \"dream\": \"...\", \"waking_note\": \"...\", \"waking_note_ids\": [\"<memory id>\", ...]}}"
    )
}

/// Seeds as `[id] text` lines, redacted (API keys and the like) and trimmed.
pub fn seed_lines(seeds: &[MemoryPoint]) -> String {
    seeds
        .iter()
        .map(|m| {
            let text: String = crate::engine::redaction::text(&m.content)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(SEED_TEXT_CHARS)
                .collect();
            format!("[{}] {}", m.id, text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse the model's reply: tolerate code fences and chatter, drop ids that aren't
/// seeds, cut an over-long dream to `max_words`, reject an empty dream.
pub fn parse_dream(
    raw: &str,
    seeds: &[MemoryPoint],
    max_words: usize,
) -> anyhow::Result<ParsedDream> {
    let start = raw.find('{');
    let end = raw.rfind('}');
    let (Some(s), Some(e)) = (start, end) else {
        return Err(DreamError::BadReply("no JSON object".into()).into());
    };
    let v: serde_json::Value = serde_json::from_str(&raw[s..=e])
        .map_err(|e| DreamError::BadReply(format!("invalid JSON: {e}")))?;
    let text = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };

    let dream_full = text("dream");
    if dream_full.is_empty() {
        return Err(DreamError::BadReply("the dream was empty".into()).into());
    }
    let words: Vec<&str> = dream_full.split_whitespace().collect();
    let dream = if words.len() > max_words {
        format!("{}…", words[..max_words].join(" "))
    } else {
        dream_full
    };

    let seed_ids: HashSet<&str> = seeds.iter().map(|m| m.id.as_str()).collect();
    let waking_note_ids: Vec<String> = v
        .get("waking_note_ids")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|x| x.trim_matches(|c| c == '[' || c == ']').to_string())
                .filter(|x| seed_ids.contains(x.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let title = {
        let t = text("title");
        if t.is_empty() {
            "Untitled dream".to_string()
        } else {
            t
        }
    };
    Ok(ParsedDream {
        title,
        dream,
        waking_note: text("waking_note"),
        waking_note_ids,
    })
}

// ───────────────────────────────── errors ─────────────────────────────────

#[derive(Debug)]
pub enum DreamError {
    /// A dream is already being generated.
    InProgress,
    /// Fewer than `MIN_SEEDS` eligible memories.
    NotEnoughMemories(usize),
    /// The configured LLM backend can't be used (missing key or URL, unknown backend, HTTP error).
    Backend(String),
    /// The model answered, but not with a usable dream.
    BadReply(String),
}

impl std::fmt::Display for DreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProgress => write!(f, "a dream is already in progress"),
            Self::NotEnoughMemories(n) => write!(
                f,
                "only {n} eligible memories; need at least {MIN_SEEDS} to dream"
            ),
            Self::Backend(m) => write!(f, "dream backend unavailable: {m}"),
            Self::BadReply(m) => write!(f, "the model's reply wasn't a usable dream: {m}"),
        }
    }
}

impl std::error::Error for DreamError {}

// ─────────────────────────────── LLM backend ──────────────────────────────

/// One chat completion against the backend chosen for consolidation
/// (`slumber.consolidation.backend`: "openai" or "local"), with `dream.model`
/// overriding the model. Returns (reply, model used).
pub async fn call_llm(
    cfg: &crate::config::AppConfig,
    system: &str,
    user: &str,
) -> anyhow::Result<(String, String)> {
    let backend = cfg.slumber.consolidation.backend.as_str();
    let model = cfg
        .dream
        .model
        .clone()
        .or_else(|| cfg.slumber.consolidation.model.clone());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(240))
        .build()?;
    let mut body = serde_json::json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": 2500,
        "temperature": 1.0,
    });
    let (url, key, model) = match backend {
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| DreamError::Backend("OPENAI_API_KEY is not set".into()))?;
            let base = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            (
                format!("{}/chat/completions", base.trim_end_matches('/')),
                Some(key),
                model.unwrap_or_else(|| "gpt-4o-mini".into()),
            )
        }
        "local" => {
            let base = std::env::var("LOCAL_LLM_URL")
                .map_err(|_| DreamError::Backend("LOCAL_LLM_URL is not set".into()))?;
            // Reasoning models burn minutes on hidden thinking; a dream doesn't need it.
            body["chat_template_kwargs"] = serde_json::json!({"enable_thinking": false});
            (
                format!("{}/v1/chat/completions", base.trim_end_matches('/')),
                std::env::var("LOCAL_LLM_API_KEY").ok(),
                model.unwrap_or_default(),
            )
        }
        other => {
            return Err(DreamError::Backend(format!("unknown backend '{other}'")).into());
        }
    };
    if !model.is_empty() {
        body["model"] = serde_json::json!(model);
    }
    let mut req = client.post(url).json(&body);
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(DreamError::Backend(format!("LLM returned HTTP {}", resp.status())).into());
    }
    let v: serde_json::Value = resp.json().await?;
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    Ok((text, model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn mem(id: &str, content: &str, ingested_days_ago: i64, touched_days_ago: i64) -> MemoryPoint {
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        serde_json::from_value(serde_json::json!({
            "id": id, "content": content, "summary": null,
            "source_file": null, "realm_id": "r", "realm_name": "R",
            "importance": 1.0, "upvotes": 0, "tags": [],
            "ingested_at": (now - Duration::days(ingested_days_ago)).to_rfc3339(),
            "last_accessed": (now - Duration::days(touched_days_ago)).to_rfc3339(),
            "access_count": 0, "chunk_type": "manual", "heading": null,
            "source_hash": "h", "memory_type": "general", "visibility": "private"
        }))
        .unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap()
    }

    fn corpus() -> Vec<MemoryPoint> {
        let mut v = Vec::new();
        for i in 0..30 {
            v.push(mem(&format!("old{i}"), &format!("old fact {i}"), 200, 90));
        }
        for i in 0..20 {
            v.push(mem(&format!("new{i}"), &format!("new fact {i}"), 2, 1));
        }
        for i in 0..20 {
            v.push(mem(&format!("mid{i}"), &format!("mid fact {i}"), 60, 14));
        }
        v
    }

    // ---- eligibility and seeds ----

    #[test]
    fn ineligible_memories_are_never_picked() {
        let mut all = corpus();
        let mut s = mem("sum", "a summary", 100, 100);
        s.chunk_type = "consolidated".into();
        let mut cron = mem("cron", "cron output", 1, 1);
        cron.tags = vec!["cron-finding".into()];
        let private = mem("priv", "secret [private] thing", 100, 100);
        let empty = mem("empty", "   ", 100, 100);
        let mut dream = mem("dm", "an old dream", 100, 100);
        dream.memory_type = "dream".into();
        all.extend([s, cron, private, empty, dream]);
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..20 {
            let seeds = pick_seeds(&all, 12, now(), &mut rng);
            for bad in ["sum", "cron", "priv", "empty", "dm"] {
                assert!(
                    seeds.iter().all(|m| m.id != bad),
                    "{bad} must not seed a dream"
                );
            }
        }
    }

    #[test]
    fn seeds_have_no_duplicates_and_respect_the_limit() {
        let all = corpus();
        let mut rng = StdRng::seed_from_u64(7);
        let seeds = pick_seeds(&all, 12, now(), &mut rng);
        assert_eq!(seeds.len(), 12);
        let ids: HashSet<&str> = seeds.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), seeds.len());
    }

    #[test]
    fn mix_contains_forgotten_and_recent() {
        let all = corpus();
        let mut rng = StdRng::seed_from_u64(7);
        let seeds = pick_seeds(&all, 12, now(), &mut rng);
        assert!(seeds.iter().any(|m| m.id.starts_with("old")));
        assert!(seeds.iter().any(|m| m.id.starts_with("new")));
    }

    #[test]
    fn linked_memories_are_followed() {
        let mut all = vec![mem("a", "a", 100, 100)];
        all[0].related_memory_ids = vec!["b".into()];
        all.push(mem("b", "b", 50, 10));
        all.push(mem("c", "c", 50, 10));
        let mut rng = StdRng::seed_from_u64(7);
        let seeds = pick_seeds(&all, 8, now(), &mut rng);
        assert_eq!(seeds.len(), 3); // fewer eligible than n: all returned
    }

    #[test]
    fn fewer_eligible_than_n_returns_them_all() {
        let all: Vec<_> = (0..5).map(|i| mem(&format!("m{i}"), "x", 10, 10)).collect();
        let mut rng = StdRng::seed_from_u64(7);
        assert_eq!(pick_seeds(&all, 12, now(), &mut rng).len(), 5);
    }

    #[test]
    fn same_rng_seed_gives_same_seeds() {
        let all = corpus();
        let a = pick_seeds(&all, 12, now(), &mut StdRng::seed_from_u64(1));
        let b = pick_seeds(&all, 12, now(), &mut StdRng::seed_from_u64(1));
        assert_eq!(
            a.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            b.iter().map(|m| m.id.clone()).collect::<Vec<_>>()
        );
    }

    // ---- schedule ----

    fn cfg(freq: &str, hour: u32, enabled: bool) -> DreamConfig {
        DreamConfig {
            enabled,
            frequency: freq.into(),
            hour,
            ..DreamConfig::default()
        }
    }

    fn at(d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d)
            .unwrap()
            .and_hms_opt(h, 30, 0)
            .unwrap()
    }

    #[test]
    fn disabled_or_wrong_hour_is_never_due() {
        assert!(!dream_due(&cfg("daily", 4, false), at(13, 4), None));
        assert!(!dream_due(&cfg("daily", 4, true), at(13, 5), None));
    }

    #[test]
    fn first_dream_is_due_in_the_hour() {
        assert!(dream_due(&cfg("daily", 4, true), at(13, 4), None));
        assert!(dream_due(&cfg("weekly", 4, true), at(13, 4), None));
    }

    #[test]
    fn daily_fires_once_per_day() {
        let c = cfg("daily", 4, true);
        let today = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        assert!(!dream_due(&c, at(13, 4), Some(today)));
        assert!(dream_due(&c, at(13, 4), Some(today - Duration::days(1))));
    }

    #[test]
    fn weekly_waits_seven_days() {
        let c = cfg("weekly", 4, true);
        let today = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        assert!(!dream_due(&c, at(13, 4), Some(today - Duration::days(6))));
        assert!(dream_due(&c, at(13, 4), Some(today - Duration::days(7))));
    }

    // ---- store ----

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("memex8-dream-{tag}-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn dream(id: &str, created: DateTime<Utc>, status: DreamStatus) -> Dream {
        Dream {
            id: id.into(),
            created_at: created.to_rfc3339(),
            title: format!("t-{id}"),
            dream: "d".into(),
            waking_note: "w".into(),
            waking_note_ids: vec![],
            seed_ids: vec![],
            model: "m".into(),
            status,
        }
    }

    #[test]
    fn save_list_get_delete_roundtrip() {
        let s = DreamStore::new(tmpdir("rt"));
        let a = dream("aaaaaaaa-1", now() - Duration::days(2), DreamStatus::Kept);
        let b = dream(
            "bbbbbbbb-2",
            now() - Duration::days(1),
            DreamStatus::Pending,
        );
        s.save(&a).unwrap();
        s.save(&b).unwrap();
        let l = s.list(10, None);
        assert_eq!(
            l.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["bbbbbbbb-2", "aaaaaaaa-1"]
        );
        assert_eq!(s.list(1, None).len(), 1);
        assert_eq!(s.get("aaaaaaaa-1").unwrap().title, "t-aaaaaaaa-1");
        assert!(s.get("nope").is_none());
        assert!(s.delete("aaaaaaaa-1").unwrap());
        assert!(!s.delete("aaaaaaaa-1").unwrap());
        assert_eq!(s.list(10, None).len(), 1);
        let _ = std::fs::remove_dir_all(s.dir());
    }

    #[test]
    fn status_filter_keep_and_pending_count() {
        let s = DreamStore::new(tmpdir("st"));
        s.save(&dream(
            "p1111111",
            now() - Duration::days(1),
            DreamStatus::Pending,
        ))
        .unwrap();
        s.save(&dream(
            "k2222222",
            now() - Duration::days(2),
            DreamStatus::Kept,
        ))
        .unwrap();
        assert_eq!(s.list(10, Some(DreamStatus::Pending)).len(), 1);
        assert_eq!(s.pending_count(), 1);
        let kept = s.keep("p1111111").unwrap().unwrap();
        assert_eq!(kept.status, DreamStatus::Kept);
        assert_eq!(s.pending_count(), 0);
        assert!(s.keep("missing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(s.dir());
    }

    #[test]
    fn legacy_file_without_status_loads_as_kept() {
        let dir = tmpdir("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("20261101-000000-legacy00.json"),
            r#"{"id":"legacy00-x","created_at":"2026-11-01T00:00:00Z","title":"t","dream":"d","waking_note":"w"}"#,
        )
        .unwrap();
        let s = DreamStore::new(&dir);
        assert_eq!(s.get("legacy00-x").unwrap().status, DreamStatus::Kept);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_skipped_not_fatal() {
        let s = DreamStore::new(tmpdir("bad"));
        s.save(&dream("good0000", now(), DreamStatus::Kept))
            .unwrap();
        std::fs::write(s.dir().join("20990101-000000-broken00.json"), "{not json").unwrap();
        let l = s.list(10, None);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].id, "good0000");
        let _ = std::fs::remove_dir_all(s.dir());
    }

    #[test]
    fn pending_dreams_expire_but_kept_ones_do_not() {
        let s = DreamStore::new(tmpdir("exp"));
        s.save(&dream(
            "oldpend0",
            now() - Duration::days(20),
            DreamStatus::Pending,
        ))
        .unwrap();
        s.save(&dream(
            "oldkept0",
            now() - Duration::days(20),
            DreamStatus::Kept,
        ))
        .unwrap();
        s.save(&dream(
            "newpend0",
            now() - Duration::days(2),
            DreamStatus::Pending,
        ))
        .unwrap();
        assert_eq!(s.expire_pending(14, now()), 1);
        let ids: Vec<String> = s.list(10, None).into_iter().map(|d| d.id).collect();
        assert!(ids.contains(&"oldkept0".to_string()) && ids.contains(&"newpend0".to_string()));
        assert!(!ids.contains(&"oldpend0".to_string()));
        let _ = std::fs::remove_dir_all(s.dir());
    }

    #[test]
    fn last_date_is_the_newest_dream() {
        let s = DreamStore::new(tmpdir("ld"));
        assert!(s.last_date().is_none());
        s.save(&dream(
            "d1111111",
            now() - Duration::days(9),
            DreamStatus::Kept,
        ))
        .unwrap();
        s.save(&dream(
            "d2222222",
            now() - Duration::days(3),
            DreamStatus::Pending,
        ))
        .unwrap();
        let want = (now() - Duration::days(3))
            .with_timezone(&Local)
            .date_naive();
        assert_eq!(s.last_date(), Some(want));
        let _ = std::fs::remove_dir_all(s.dir());
    }

    // ---- parsing ----

    fn seeds2() -> Vec<MemoryPoint> {
        vec![mem("id-1", "one", 1, 1), mem("id-2", "two", 1, 1)]
    }

    #[test]
    fn fenced_json_is_accepted() {
        let raw = "Sure!\n```json\n{\"title\":\"T\",\"dream\":\"a b c\",\"waking_note\":\"n\",\"waking_note_ids\":[\"id-1\"]}\n```";
        let p = parse_dream(raw, &seeds2(), 100).unwrap();
        assert_eq!(p.title, "T");
        assert_eq!(p.waking_note_ids, vec!["id-1"]);
    }

    #[test]
    fn non_seed_ids_are_dropped_and_brackets_tolerated() {
        let raw = r#"{"title":"T","dream":"x","waking_note":"n","waking_note_ids":["[id-2]","invented","id-1"]}"#;
        let p = parse_dream(raw, &seeds2(), 100).unwrap();
        assert_eq!(p.waking_note_ids, vec!["id-2", "id-1"]);
    }

    #[test]
    fn long_dream_is_truncated_to_max_words() {
        let words = vec!["w"; 50].join(" ");
        let raw = format!(r#"{{"title":"T","dream":"{words}","waking_note":"n"}}"#);
        let p = parse_dream(&raw, &seeds2(), 10).unwrap();
        assert_eq!(p.dream.split_whitespace().count(), 10);
        assert!(p.dream.ends_with('…'));
    }

    #[test]
    fn garbage_and_empty_dreams_are_errors() {
        assert!(parse_dream("no json at all", &seeds2(), 100).is_err());
        assert!(parse_dream("{not json}", &seeds2(), 100).is_err());
        assert!(parse_dream(
            r#"{"title":"T","dream":"  ","waking_note":"n"}"#,
            &seeds2(),
            100
        )
        .is_err());
    }

    #[test]
    fn missing_title_gets_a_default() {
        let p = parse_dream(r#"{"dream":"x","waking_note":"n"}"#, &seeds2(), 100).unwrap();
        assert_eq!(p.title, "Untitled dream");
    }

    #[test]
    fn seed_lines_are_trimmed_and_flattened() {
        let mut m = mem("id-9", &"word ".repeat(400), 1, 1);
        m.content.push_str("\n\nnewline");
        let l = seed_lines(&[m]);
        assert!(l.starts_with("[id-9] word word"));
        assert!(!l.contains('\n'));
        assert!(l.chars().count() < 520);
    }
}
