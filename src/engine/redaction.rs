//! One private-span parser for all ingestion paths. Never includes input in errors.
use serde::{de::DeserializeOwned, Serialize};

#[derive(Debug)]
pub enum RedactionError {
    Empty,
    Unsanitized,
}
impl std::fmt::Display for RedactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "content empty after redaction",
            Self::Unsanitized => "private spans require sanitization before persistence",
        })
    }
}
impl std::error::Error for RedactionError {}

/// Keep state across conversation messages so truncation cannot expose a span's tail.
#[derive(Default)]
pub struct Redactor {
    depth: usize,
}
impl Redactor {
    pub fn redact(&mut self, input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let tail = &input.as_bytes()[i..];
            if tail
                .get(..9)
                .is_some_and(|s| s.eq_ignore_ascii_case(b"<private>"))
            {
                if self.depth == 0 {
                    out.push_str("[private]");
                }
                self.depth = self.depth.saturating_add(1);
                i += 9;
            } else if self.depth > 0
                && tail
                    .get(..10)
                    .is_some_and(|s| s.eq_ignore_ascii_case(b"</private>"))
            {
                self.depth -= 1;
                i += 10;
            } else {
                let ch = input[i..].chars().next().unwrap();
                if self.depth == 0 {
                    out.push(ch);
                }
                i += ch.len_utf8();
            }
        }
        out
    }
}

pub fn text(input: &str) -> String {
    Redactor::default().redact(input)
}
pub fn meaningful(input: &str) -> bool {
    !input.replace("[private]", "").trim().is_empty()
}
pub fn required(input: &str) -> Result<String, RedactionError> {
    let clean = text(input);
    if meaningful(&clean) {
        Ok(clean)
    } else {
        Err(RedactionError::Empty)
    }
}

fn sanitize_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = text(s),
        serde_json::Value::Array(values) => values.iter_mut().for_each(sanitize_value),
        serde_json::Value::Object(values) => values.values_mut().for_each(sanitize_value),
        _ => {}
    }
}

/// Sanitize textual metadata as well as content; structured IDs/numbers remain intact.
pub fn sanitize<T: Serialize + DeserializeOwned>(input: &T) -> anyhow::Result<T> {
    let mut value = serde_json::to_value(input)?;
    sanitize_value(&mut value);
    Ok(serde_json::from_value(value)?)
}

/// Storage is a guard, not a second sanitizer: don't pair a changed body with an old vector.
pub fn ensure_clean<T: Serialize>(input: &T) -> anyhow::Result<()> {
    let original = serde_json::to_value(input)?;
    let mut clean = original.clone();
    sanitize_value(&mut clean);
    if original != clean {
        return Err(RedactionError::Unsanitized.into());
    }
    Ok(())
}

pub fn ensure_memory(memory: &crate::storage::qdrant::MemoryPoint) -> anyhow::Result<()> {
    required(&memory.content)?;
    ensure_clean(memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans_are_case_insensitive_multiline_nested_and_fail_closed() {
        for (raw, expected) in [
            (
                "hello <private>secret</private> world",
                "hello [private] world",
            ),
            (
                "a<PRIVATE>one\ntwo</PrIvAtE>b<private>three</private>c",
                "a[private]b[private]c",
            ),
            (
                "é🦀<private>inner<private>nested</private>tail</private>界",
                "é🦀[private]界",
            ),
            ("public<private>unclosed", "public[private]"),
            (
                "public<private>a<private>b</private>tail",
                "public[private]",
            ),
            (
                "literal <privacy>ok</privacy>",
                "literal <privacy>ok</privacy>",
            ),
        ] {
            assert_eq!(text(raw), expected);
            assert_eq!(text(expected), expected);
        }
    }
    #[test]
    fn all_private_and_marker_only_inputs_are_empty() {
        for input in [
            "",
            " \n",
            "<private>secret</private>",
            "<private>unclosed",
            "[private] \n[private]",
        ] {
            assert!(matches!(required(input), Err(RedactionError::Empty)));
        }
        assert!(required("public <private>secret</private>").is_ok());
    }
    #[test]
    fn conversation_state_survives_message_boundaries() {
        let mut parser = Redactor::default();
        assert_eq!(parser.redact("public <private>secret"), "public [private]");
        assert_eq!(parser.redact("secret tail</private> safe"), " safe");
    }
}
