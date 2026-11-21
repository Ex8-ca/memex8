//! Memory writes: the one entry point for storing a new memory.
//!
//! Callers (MCP, REST, webhooks, dream keeping) describe what to store as a
//! [`NewMemory`] and call [`Engine::write_memory`]. Validation, defaults,
//! private-span redaction, the optional exact-duplicate check and the
//! certified flag all live here, so a rule changes in one place.
use super::{redaction, Engine};
use crate::engine::decay;

/// Who may see a memory. Anything that is not exactly `public` (any case,
/// surrounding whitespace ignored) is private: a malformed caller can never
/// mark a memory shareable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

impl Visibility {
    /// Fail-closed parse: unknown values become [`Visibility::Private`].
    pub fn parse(raw: &str) -> Self {
        match crate::storage::qdrant::normalize_visibility(raw) {
            "public" => Self::Public,
            _ => Self::Private,
        }
    }

    /// `None` is private, like an unrecognised value.
    pub fn from_option(raw: Option<&str>) -> Self {
        raw.map(Self::parse).unwrap_or_default()
    }

    /// Exactly `private` or `public`; anything else is `None`.
    pub fn parse_strict(raw: &str) -> Option<Self> {
        match raw {
            "private" => Some(Self::Private),
            "public" => Some(Self::Public),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}

/// Whether a write may be answered by an existing memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dedup {
    /// Always write a new memory.
    #[default]
    Off,
    /// Return the existing memory when one has the same content, memory type,
    /// tags, source and visibility. Concurrent identical writes cannot both miss.
    Identical,
}

/// Everything a caller says about a memory to store.
#[derive(Debug, Clone)]
pub struct NewMemory<'a> {
    pub content: &'a str,
    pub tags: Vec<String>,
    pub realm_hint: Option<&'a str>,
    /// Defaults to `manual` when stored.
    pub source: Option<&'a str>,
    pub visibility: Visibility,
    /// Defaults to `general`. Unknown types are stored as given and decay like
    /// `general`; a warning is logged so a typo is visible.
    pub memory_type: Option<&'a str>,
    pub certified: bool,
    pub dedup: Dedup,
}

impl<'a> NewMemory<'a> {
    /// A private, uncertified `general` memory with no tags and no duplicate check.
    pub fn new(content: &'a str) -> Self {
        Self {
            content,
            tags: Vec::new(),
            realm_hint: None,
            source: None,
            visibility: Visibility::Private,
            memory_type: None,
            certified: false,
            dedup: Dedup::Off,
        }
    }
}

/// Result of a write. `created` is false only when [`Dedup::Identical`]
/// matched an existing memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: String,
    pub created: bool,
}

impl Engine {
    pub async fn write_memory(&self, new: NewMemory<'_>) -> anyhow::Result<Stored> {
        // Redact once, up front: the hash, the duplicate check and the stored
        // payload all see the same clean text.
        let content = redaction::required(new.content)?;
        let tags: Vec<String> = new.tags.iter().map(|t| redaction::text(t)).collect();
        let realm_hint = new.realm_hint.map(redaction::text);
        let source = new.source.map(redaction::text);
        let memory_type = redaction::text(new.memory_type.unwrap_or("general"));
        if !decay::is_known_memory_type(&memory_type) {
            tracing::warn!(
                memory_type = %memory_type,
                "unknown memory_type; it will decay like \"general\""
            );
        }
        let clean = CleanMemory {
            content: &content,
            tags: &tags,
            realm_hint: realm_hint.as_deref(),
            source: source.as_deref(),
            visibility: new.visibility,
            memory_type: &memory_type,
            certified: new.certified,
        };

        match new.dedup {
            Dedup::Off => Ok(Stored {
                id: self.insert_memory(&clean, "").await?,
                created: true,
            }),
            Dedup::Identical => {
                // The stored `source_hash` is the content hash, the same
                // meaning import gives it, so slumber's exact-duplicate
                // detection also recognises these memories.
                let hash = crate::storage::qdrant::content_hash(&content);
                let lock = self.idempotency_locks.get(&hash);
                let _held = lock.lock().await;

                let wanted_source = clean.source.unwrap_or("manual");
                let mut wanted_tags = tags.clone();
                wanted_tags.sort();
                wanted_tags.dedup();
                let erased = self.erased_ids()?;
                for existing in self.store.memories_with_source_hash(&hash, 16).await? {
                    let mut existing_tags = existing.tags.clone();
                    existing_tags.sort();
                    existing_tags.dedup();
                    if !erased.contains(&existing.id)
                        && existing.content == content
                        && existing.memory_type == memory_type
                        && existing.visibility == new.visibility.as_str()
                        && existing.source_file.as_deref() == Some(wanted_source)
                        && existing_tags == wanted_tags
                    {
                        return Ok(Stored {
                            id: existing.id,
                            created: false,
                        });
                    }
                }
                Ok(Stored {
                    id: self.insert_memory(&clean, &hash).await?,
                    created: true,
                })
            }
        }
    }

    /// Embed, place in a realm, write to Qdrant and the lexical index.
    /// Inputs are already redacted.
    async fn insert_memory(
        &self,
        memory: &CleanMemory<'_>,
        source_hash: &str,
    ) -> anyhow::Result<String> {
        let embedder = self.make_embedder()?;
        let vector = embedder.embed(memory.content).await?;

        let id = uuid::Uuid::new_v4().to_string();

        // Assign to realm
        let realm_id = if let Some(hint) = memory.realm_hint {
            self.store.find_realm_by_name(hint).await?.map(|r| r.id)
        } else {
            None
        };

        let realm_id = match realm_id {
            Some(rid) => rid,
            None => self.auto_assign_realm(&vector).await?,
        };

        let realm = self.store.get_realm(&realm_id).await?;
        let realm_name = realm.map(|r| r.name.clone()).unwrap_or_default();

        let source_str = memory.source.unwrap_or("manual");

        // Infer reaction score from content
        let reaction_score = crate::engine::reactions::infer_reaction(memory.content);

        // A new memory is not in any compressed index built earlier, and search
        // prefers that index. Drop it before the write so the new memory is
        // visible immediately instead of only after the next restart.
        self.invalidate_compressed_index().await;
        self.store
            .store_memory_with_tags(
                &id,
                &vector,
                memory.content,
                None,
                Some(source_str),
                &realm_id,
                &realm_name,
                source_hash,
                "manual",
                memory.memory_type,
                reaction_score,
                memory.visibility.as_str(),
                memory.tags,
                memory.certified,
            )
            .await?;
        // Invalidate again now the write is durable: a reload that verified the
        // corpus before this write landed must not keep its map (see
        // `reload_compressed_index`).
        self.invalidate_compressed_index().await;

        {
            let mut text = memory.content.to_string();
            for t in memory.tags {
                text.push(' ');
                text.push_str(t);
            }
            self.lexical.write().await.upsert(&id, &text);
        }

        Ok(id)
    }
}

/// A [`NewMemory`] after redaction and defaulting.
struct CleanMemory<'a> {
    content: &'a str,
    tags: &'a [String],
    realm_hint: Option<&'a str>,
    source: Option<&'a str>,
    visibility: Visibility,
    memory_type: &'a str,
    certified: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_fails_closed() {
        assert_eq!(Visibility::parse("public"), Visibility::Public);
        assert_eq!(Visibility::parse("  PUBLIC "), Visibility::Public);
        for raw in ["private", "", "public-all", "wat", "🦀"] {
            assert_eq!(Visibility::parse(raw), Visibility::Private, "{raw:?}");
        }
        assert_eq!(Visibility::from_option(None), Visibility::Private);
        assert_eq!(Visibility::from_option(Some("public")), Visibility::Public);
    }

    #[test]
    fn strict_visibility_accepts_only_exact_values() {
        assert_eq!(
            Visibility::parse_strict("private"),
            Some(Visibility::Private)
        );
        assert_eq!(Visibility::parse_strict("public"), Some(Visibility::Public));
        for raw in ["PUBLIC", " public", "", "nonsense"] {
            assert_eq!(Visibility::parse_strict(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn visibility_round_trips_through_its_stored_string() {
        for v in [Visibility::Private, Visibility::Public] {
            assert_eq!(Visibility::parse(v.as_str()), v);
        }
    }

    #[test]
    fn new_memory_defaults_to_private_uncertified_and_no_dedup() {
        let m = NewMemory::new("hello");
        assert_eq!(m.visibility, Visibility::Private);
        assert!(!m.certified);
        assert_eq!(m.dedup, Dedup::Off);
        assert!(m.tags.is_empty() && m.memory_type.is_none() && m.source.is_none());
    }
}
