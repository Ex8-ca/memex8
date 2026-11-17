//! Person lookup: "who is X" questions should answer from long-lived facts.
//!
//! Dense similarity ranks a short profile fact ("X is the user's spouse") below
//! long transcripts and chat logs that merely repeat the name, so the fact the
//! question is asking for can fall outside the candidate set entirely. When a
//! query is explicitly a question about someone, long-lived memories that name
//! them are fetched separately and placed ahead of other results. Every other
//! query is left untouched.

use regex::Regex;
use std::sync::OnceLock;

/// Memory types that describe durable facts about people and preferences.
pub const LONG_LIVED_TYPES: &[&str] = &["profile", "preference", "entity", "relationship"];

/// How many long-lived memories to score per person lookup before name filtering.
pub const CANDIDATES: usize = 200;

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "my", "our", "your", "his", "her", "their", "is", "was", "are", "were", "of",
    "to", "me", "us", "about", "again", "exactly", "really", "mr", "mrs", "ms", "dr",
];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:who\s*(?:'s|’s|\s+is|\s+was|\s+are|\s+were)|tell\s+me\s+(?:more\s+)?about|what\s+do\s+(?:you|we)\s+know\s+about)\s+(.+?)[\s?.!]*$",
        )
        .expect("person lookup regex")
    })
}

/// Name terms when `query` is a question about a person, else empty.
pub fn person_terms(query: &str) -> Vec<String> {
    let Some(caps) = pattern().captures(query) else {
        return Vec::new();
    };
    caps[1]
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '’' || c == '-'))
        .map(|t| {
            let t = t.trim_matches(|c| c == '\'' || c == '’' || c == '-');
            let t = t
                .strip_suffix("'s")
                .or_else(|| t.strip_suffix("’s"))
                .unwrap_or(t);
            t.to_lowercase()
        })
        .filter(|t| t.chars().count() >= 2 && !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// True when `content` contains every term as a whole word (case-insensitive).
pub fn names_all(content: &str, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let words: std::collections::HashSet<String> = content
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    terms.iter().all(|t| {
        t.split(|c: char| !c.is_alphanumeric())
            .filter(|p| !p.is_empty())
            .all(|p| words.contains(p))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_person_questions() {
        assert_eq!(person_terms("Who is Avery"), vec!["avery"]);
        assert_eq!(person_terms("who's avery quinn?"), vec!["avery", "quinn"]);
        assert_eq!(person_terms("Tell me about Jordan."), vec!["jordan"]);
        assert_eq!(person_terms("what do you know about my wife"), vec!["wife"]);
        assert_eq!(person_terms("Who was Dr. Smith's"), vec!["smith"]);
    }

    #[test]
    fn ignores_other_queries() {
        for q in [
            "Tesla Model 3 audio upgrade",
            "Avery retirement countdown",
            "where is the backup",
            "notes about who is on call",
            "who is",
            "",
        ] {
            assert!(person_terms(q).is_empty(), "{q:?} -> {:?}", person_terms(q));
        }
    }

    #[test]
    fn matches_whole_words_only() {
        let t = vec!["avery".to_string(), "quinn".to_string()];
        assert!(names_all("**AVERY QUINN** is the user's wife", &t));
        assert!(!names_all("Avery Quinnell", &t));
        assert!(!names_all("Averys quinn", &t));
        assert!(!names_all("anything", &[]));
    }
}
