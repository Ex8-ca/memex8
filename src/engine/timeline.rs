//! Chronological neighbors with bounded, fully paginated tie handling.
use super::{retrieval::CompactMemoryResult, Engine};
use crate::storage::qdrant::MemoryPoint;
use chrono::{DateTime, SecondsFormat, Utc};
use qdrant_client::qdrant::{DatetimeRange, Direction, Timestamp};
use serde::{Deserialize, Serialize};

const WORK_LIMIT: usize = 10_000;
const PAGE_SIZE: usize = 100;

#[derive(Debug)]
pub enum TimelineError {
    Invalid(&'static str),
    NotFound,
    InvalidAnchorTime,
}
impl std::fmt::Display for TimelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid(message) => message,
            Self::NotFound => "Timeline anchor not found in requested scope",
            Self::InvalidAnchorTime => "Timeline anchor has an invalid ingested_at timestamp",
        })
    }
}
impl std::error::Error for TimelineError {}

pub enum Anchor {
    MemoryId(String),
    Query(String),
    Time(DateTime<Utc>),
}

#[derive(Default, Deserialize)]
pub struct TimelineRequest {
    pub id: Option<String>,
    pub query: Option<String>,
    pub at: Option<String>,
    pub before: Option<usize>,
    pub after: Option<usize>,
    pub realm: Option<String>,
    pub visibility: Option<String>,
}
impl TimelineRequest {
    pub fn validate(&self) -> Result<(Anchor, usize, usize), TimelineError> {
        let (before, after) = (self.before.unwrap_or(5), self.after.unwrap_or(5));
        if before > 50 || after > 50 {
            return Err(TimelineError::Invalid(
                "before and after must be between 0 and 50",
            ));
        }
        if [self.id.is_some(), self.query.is_some(), self.at.is_some()]
            .into_iter()
            .filter(|v| *v)
            .count()
            != 1
        {
            return Err(TimelineError::Invalid(
                "Pass exactly one of id, query, or at",
            ));
        }
        let anchor = if let Some(id) = &self.id {
            let id = uuid::Uuid::parse_str(id)
                .map_err(|_| TimelineError::Invalid("id must be a memory UUID"))?;
            Anchor::MemoryId(id.to_string())
        } else if let Some(query) = &self.query {
            if query.trim().is_empty() {
                return Err(TimelineError::Invalid("query must not be empty"));
            }
            Anchor::Query(query.clone())
        } else {
            Anchor::Time(
                DateTime::parse_from_rfc3339(self.at.as_deref().unwrap())
                    .map_err(|_| TimelineError::Invalid("at must be an RFC3339 timestamp"))?
                    .with_timezone(&Utc),
            )
        };
        if self
            .visibility
            .as_deref()
            .is_some_and(|v| !matches!(v, "public" | "private"))
        {
            return Err(TimelineError::Invalid(
                "visibility must be private or public",
            ));
        }
        Ok((anchor, before, after))
    }
}

#[derive(Serialize)]
pub struct TimelineResponse {
    pub anchor_id: Option<String>,
    pub anchor_at: Option<String>,
    pub results: Vec<CompactMemoryResult>,
    pub no_match: bool,
    pub truncated: bool,
}

#[derive(Default)]
struct Budget {
    scanned: usize,
    truncated: bool,
}
impl Budget {
    fn remaining(&self) -> usize {
        WORK_LIMIT.saturating_sub(self.scanned)
    }
}
fn timestamp(time: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: time.timestamp(),
        nanos: time.timestamp_subsec_nanos() as i32,
    }
}
fn equal_range(time: DateTime<Utc>) -> DatetimeRange {
    DatetimeRange {
        gte: Some(timestamp(time)),
        lte: Some(timestamp(time)),
        ..Default::default()
    }
}
fn key(memory: &MemoryPoint) -> Option<(DateTime<Utc>, String)> {
    DateTime::parse_from_rfc3339(&memory.ingested_at)
        .ok()
        .map(|time| (time.with_timezone(&Utc), memory.id.clone()))
}
fn ordered(mut rows: Vec<MemoryPoint>) -> Vec<MemoryPoint> {
    rows.retain(|m| key(m).is_some());
    rows.sort_by_cached_key(|m| key(m).unwrap());
    rows.dedup_by(|a, b| a.id == b.id);
    rows
}

impl Engine {
    pub async fn timeline_request(
        &self,
        request: TimelineRequest,
    ) -> anyhow::Result<TimelineResponse> {
        let (anchor, before, after) = request.validate()?;
        self.timeline(
            anchor,
            before,
            after,
            request.realm.as_deref(),
            request.visibility.as_deref(),
        )
        .await
    }

    pub async fn timeline(
        &self,
        anchor: Anchor,
        before: usize,
        after: usize,
        realm: Option<&str>,
        visibility: Option<&str>,
    ) -> anyhow::Result<TimelineResponse> {
        anyhow::ensure!(
            before <= 50 && after <= 50,
            TimelineError::Invalid("before and after must be between 0 and 50")
        );
        anyhow::ensure!(
            visibility.is_none_or(|v| matches!(v, "private" | "public")),
            TimelineError::Invalid("visibility must be private or public")
        );
        let (anchor_memory, time) = match anchor {
            Anchor::Time(time) => (None, time),
            Anchor::MemoryId(id) => {
                let memory = self
                    .store
                    .get_memory(&id)
                    .await?
                    .filter(|m| {
                        realm.is_none_or(|r| m.realm_name == r)
                            && visibility.is_none_or(|v| m.visibility == v)
                    })
                    .ok_or(TimelineError::NotFound)?;
                let time = key(&memory).ok_or(TimelineError::InvalidAnchorTime)?.0;
                (Some(memory), time)
            }
            Anchor::Query(query) => {
                let vector = self.make_embedder()?.embed(&query).await?;
                let hits = self
                    .search_embedded(&query, realm, None, 1, 0, 0.3, visibility, &vector)
                    .await?;
                let Some(hit) = hits.first() else {
                    return Ok(TimelineResponse {
                        anchor_id: None,
                        anchor_at: None,
                        results: vec![],
                        no_match: true,
                        truncated: false,
                    });
                };
                let memory = self
                    .store
                    .get_memory(&hit.id)
                    .await?
                    .filter(|m| {
                        realm.is_none_or(|r| m.realm_name == r)
                            && visibility.is_none_or(|v| m.visibility == v)
                    })
                    .ok_or(TimelineError::NotFound)?;
                let time = key(&memory).ok_or(TimelineError::InvalidAnchorTime)?.0;
                (Some(memory), time)
            }
        };
        let anchor_id = anchor_memory.as_ref().map(|m| m.id.clone());
        let mut budget = Budget::default();
        let bucket = if before + after > 0 {
            self.timeline_equal(time, realm, visibility, &mut budget)
                .await?
        } else {
            vec![]
        };
        let is_before = |m: &MemoryPoint| {
            key(m).is_some_and(|(t, id)| {
                anchor_id
                    .as_ref()
                    .map_or(t < time, |anchor| (t, &id) < (time, anchor))
            })
        };
        let is_after = |m: &MemoryPoint| {
            key(m).is_some_and(|(t, id)| {
                anchor_id
                    .as_ref()
                    .map_or(t >= time, |anchor| (t, &id) > (time, anchor))
            })
        };
        let mut older: Vec<_> = bucket.iter().filter(|m| is_before(m)).cloned().collect();
        let mut newer: Vec<_> = bucket.into_iter().filter(|m| is_after(m)).collect();
        if older.len() < before {
            older.extend(
                self.timeline_side(
                    time,
                    true,
                    before - older.len(),
                    realm,
                    visibility,
                    &mut budget,
                )
                .await?,
            );
        }
        if newer.len() < after {
            newer.extend(
                self.timeline_side(
                    time,
                    false,
                    after - newer.len(),
                    realm,
                    visibility,
                    &mut budget,
                )
                .await?,
            );
        }
        older.retain(is_before);
        newer.retain(is_after);
        let older = ordered(older);
        let mut selected: Vec<_> = older.into_iter().rev().take(before).collect();
        selected.extend(anchor_memory);
        selected.extend(ordered(newer).into_iter().take(after));
        let results = ordered(selected)
            .into_iter()
            .map(|mut m| {
                m.ingested_at = key(&m)
                    .unwrap()
                    .0
                    .to_rfc3339_opts(SecondsFormat::AutoSi, true);
                CompactMemoryResult::from_memory(m, 0.0)
            })
            .collect();
        Ok(TimelineResponse {
            anchor_id,
            anchor_at: Some(time.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
            results,
            no_match: false,
            truncated: budget.truncated,
        })
    }

    async fn timeline_equal(
        &self,
        time: DateTime<Utc>,
        realm: Option<&str>,
        visibility: Option<&str>,
        budget: &mut Budget,
    ) -> anyhow::Result<Vec<MemoryPoint>> {
        let mut rows = Vec::new();
        let mut offset = None;
        loop {
            if budget.remaining() == 0 {
                budget.truncated = true;
                break;
            }
            let (page, next) = self
                .store
                .timeline_page(
                    equal_range(time),
                    None,
                    PAGE_SIZE.min(budget.remaining()),
                    offset,
                    realm,
                    visibility,
                )
                .await?;
            budget.scanned += page.len();
            rows.extend(page);
            offset = next;
            if offset.is_none() {
                break;
            }
        }
        Ok(rows)
    }

    async fn timeline_side(
        &self,
        time: DateTime<Utc>,
        before: bool,
        count: usize,
        realm: Option<&str>,
        visibility: Option<&str>,
        budget: &mut Budget,
    ) -> anyhow::Result<Vec<MemoryPoint>> {
        if count == 0 {
            return Ok(vec![]);
        }
        if budget.remaining() == 0 {
            budget.truncated = true;
            return Ok(vec![]);
        }
        let range = if before {
            DatetimeRange {
                lt: Some(timestamp(time)),
                ..Default::default()
            }
        } else {
            DatetimeRange {
                gt: Some(timestamp(time)),
                ..Default::default()
            }
        };
        let (mut rows, _) = self
            .store
            .timeline_page(
                range,
                Some(if before {
                    Direction::Desc
                } else {
                    Direction::Asc
                }),
                count.min(budget.remaining()),
                None,
                realm,
                visibility,
            )
            .await?;
        budget.scanned += rows.len();
        if let Some(boundary) = rows.iter().filter_map(key).map(|k| k.0).reduce(|a, b| {
            if before {
                a.min(b)
            } else {
                a.max(b)
            }
        }) {
            // Ordered scroll can cut a timestamp tie arbitrarily. Read its complete
            // bucket with ID pagination before choosing the deterministic cutoff.
            rows.extend(
                self.timeline_equal(boundary, realm, visibility, budget)
                    .await?,
            );
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_one_anchor_limits_and_utc() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"query":""}),
            serde_json::json!({"query":"a", "at":"bad"}),
            serde_json::json!({"at":"bad"}),
            serde_json::json!({"at":"2026-11-08T00:00:00Z", "before":51}),
        ] {
            assert!(serde_json::from_value::<TimelineRequest>(value)
                .unwrap()
                .validate()
                .is_err());
        }
        let request: TimelineRequest =
            serde_json::from_value(serde_json::json!({"at":"2026-11-08T02:00:00+02:00"})).unwrap();
        let (Anchor::Time(time), before, after) = request.validate().unwrap() else {
            panic!()
        };
        assert_eq!((before, after), (5, 5));
        assert_eq!(
            time.to_rfc3339_opts(SecondsFormat::AutoSi, true),
            "2026-11-08T00:00:00Z"
        );
    }
}
