use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    pub heading: Option<String>,
    pub parent_heading: Option<String>,
    pub parent_chain: Vec<String>,
    pub chunk_type: String,
    pub token_count: usize,
}

/// Chunk markdown content using AST-based parsing.
///
/// Strategy determines the heading level at which to split:
/// - "section" → split at H2 (default)
/// - "h1" → split at H1 only (larger chunks)
/// - "h3" → split at H3 (smaller chunks)
/// - "paragraph" → split at paragraph boundaries
/// - "file" → one chunk per file
pub fn chunk(content: &str, strategy: &str, max_tokens: u32) -> anyhow::Result<Vec<Chunk>> {
    match strategy {
        "section" | "h2" => chunk_by_heading(content, HeadingLevel::H2, max_tokens),
        "h1" => chunk_by_heading(content, HeadingLevel::H1, max_tokens),
        "h3" => chunk_by_heading(content, HeadingLevel::H3, max_tokens),
        "paragraph" => chunk_by_paragraph(content, max_tokens),
        "file" => chunk_by_file(content),
        _ => chunk_by_heading(content, HeadingLevel::H2, max_tokens),
    }
}

/// Chunk by heading level, keeping code blocks and tables intact.
fn chunk_by_heading(
    content: &str,
    split_level: HeadingLevel,
    max_tokens: u32,
) -> anyhow::Result<Vec<Chunk>> {
    let options = Options::all();
    let parser = Parser::new_ext(content, options);

    let mut chunks = Vec::new();
    let mut current_content = String::new();
    let mut heading_stack: Vec<(HeadingLevel, String)> = Vec::new();
    let mut in_code_block = false;
    let mut in_table = false;
    let mut in_list = false;
    let mut in_heading = false;
    let mut current_heading_text = String::new();
    let mut chunk_start_heading: Option<(HeadingLevel, String)> = None;

    for event in parser {
        match &event {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = true;
                if level == &split_level && !current_content.trim().is_empty() {
                    if let Some(ref h_heading) = chunk_start_heading {
                        let h_level = h_heading.0;
                        let h_text = h_heading.1.clone();
                        flush_chunk(&mut chunks, &mut current_content, h_level, Some(&h_text), &heading_stack);
                    }
                    chunk_start_heading = None;
                }
            }

            Event::End(TagEnd::Heading(level)) => {
                in_heading = false;
                let heading_text = current_heading_text.trim().to_string();
                current_heading_text.clear();

                if !heading_text.is_empty() {
                    heading_stack.retain(|(l, _)| l < level);
                    heading_stack.push((*level, heading_text.clone()));

                    if *level == split_level {
                        chunk_start_heading = Some((*level, heading_text));
                    }
                }
            }

            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
            }

            Event::Start(Tag::Table(_)) => {
                in_table = true;
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
            }

            Event::Start(Tag::List(_)) => {
                in_list = true;
            }
            Event::End(TagEnd::List(_)) => {
                in_list = false;
            }

            Event::Text(text) | Event::Code(text) => {
                if in_heading {
                    current_heading_text.push_str(text);
                }
                current_content.push_str(text);
            }
            Event::SoftBreak | Event::HardBreak => {
                current_content.push('\n');
            }
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {
                current_content.push('\n');
            }
            Event::Start(Tag::BlockQuote(_)) => {
                current_content.push('\n');
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                current_content.push('\n');
            }

            Event::Html(html) | Event::InlineHtml(html) => {
                current_content.push_str(html);
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                current_content.push_str(text);
            }
            Event::TaskListMarker(_checked) => {
                current_content.push_str("[ ] ")
            }

            Event::Rule => {
                current_content.push_str("\n---\n");
            }

            _ => {}
        }

        // Force split at token boundary if not inside structural elements
        if !in_code_block && !in_table && !in_list && !in_heading {
            let tc = estimate_tokens(&current_content);
            if tc > max_tokens as usize && !current_content.trim().is_empty() {
                if let Some(last_para_end) = current_content.rfind("\n\n") {
                    let first_part = current_content[..last_para_end].to_string();
                    let second_part = current_content[last_para_end + 2..].to_string();

                    if let Some(ref h_heading) = chunk_start_heading {
                        let h_level = h_heading.0;
                        let h_text = h_heading.1.clone();
                        flush_chunk(&mut chunks, &mut first_part.clone(), h_level, Some(&h_text), &heading_stack);
                    }
                    current_content = second_part;
                } else {
                    let char_limit = max_tokens as usize * 4;
                    if current_content.len() > char_limit {
                        let first_part = current_content[..char_limit].to_string();
                        let second_part = current_content[char_limit..].to_string();

                        if let Some(ref h_heading) = chunk_start_heading {
                            let h_level = h_heading.0;
                            let h_text = h_heading.1.clone();
                            flush_chunk(&mut chunks, &mut first_part.clone(), h_level, Some(&h_text), &heading_stack);
                        }
                        current_content = second_part;
                    }
                }
            }
        }
    }

    // Flush final chunk
    if !current_content.trim().is_empty() {
        let h_level = chunk_start_heading
            .as_ref()
            .map(|(l, _)| *l)
            .unwrap_or(HeadingLevel::H1);
        let h_text = chunk_start_heading.as_ref().map(|(_, t)| t.as_str());
        flush_chunk(&mut chunks, &mut current_content, h_level, h_text, &heading_stack);
    }

    // If no chunks were produced, fall back to single-file chunk
    if chunks.is_empty() {
        chunks.push(Chunk {
            content: content.to_string(),
            heading: None,
            parent_heading: None,
            parent_chain: vec![],
            chunk_type: "file".to_string(),
            token_count: estimate_tokens(content),
        });
    }

    Ok(chunks)
}

/// Flush accumulated content as a chunk.
fn flush_chunk(
    chunks: &mut Vec<Chunk>,
    content: &mut String,
    chunk_heading_level: HeadingLevel,
    chunk_heading_name: Option<&str>,
    heading_stack: &[(HeadingLevel, String)],
) {
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return;
    }

    let tc = estimate_tokens(&trimmed);

    // Use the chunk's own heading (from chunk_start_heading), not the deepest heading in the stack
    let heading = chunk_heading_name.map(|s| s.to_string());

    // Build parent chain: all headings from H1 down to the chunk heading level
    let parent_chain: Vec<String> = heading_stack
        .iter()
        .filter(|(l, _)| *l <= chunk_heading_level)
        .map(|(_, t)| t.clone())
        .collect();

    let parent_heading = if parent_chain.len() >= 1 {
        Some(parent_chain.last().cloned().unwrap_or_default())
    } else {
        None
    };

    let chunk_type = match chunk_heading_level {
        HeadingLevel::H1 => "h1_section",
        HeadingLevel::H2 => "h2_section",
        HeadingLevel::H3 => "h3_section",
        HeadingLevel::H4 => "h4_section",
        HeadingLevel::H5 => "h5_section",
        HeadingLevel::H6 => "h6_section",
    };

    chunks.push(Chunk {
        content: trimmed,
        heading,
        parent_heading,
        parent_chain,
        chunk_type: chunk_type.to_string(),
        token_count: tc,
    });

    content.clear();
}

/// Chunk by paragraph boundaries.
fn chunk_by_paragraph(content: &str, max_tokens: u32) -> anyhow::Result<Vec<Chunk>> {
    let options = Options::all();
    let parser = Parser::new_ext(content, options);

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0;
    let mut in_code_block = false;

    for event in parser {
        match &event {
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => in_code_block = false,
            Event::Text(text) | Event::Code(text) => {
                current.push_str(text);
            }
            Event::SoftBreak | Event::HardBreak => {
                current.push('\n');
            }
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {
                if !current.trim().is_empty() && !in_code_block {
                    let para_tokens = estimate_tokens(&current);
                    if current_tokens + para_tokens > max_tokens as usize && current_tokens > 0 {
                        chunks.push(Chunk {
                            content: current.trim().to_string(),
                            heading: None,
                            parent_heading: None,
                            parent_chain: vec![],
                            chunk_type: "paragraph".to_string(),
                            token_count: current_tokens,
                        });
                        current.clear();
                        current_tokens = 0;
                    }
                    current.push_str("\n\n");
                    current_tokens += para_tokens;
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                current.push_str(html);
            }
            _ => {}
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        chunks.push(Chunk {
            content: trimmed,
            heading: None,
            parent_heading: None,
            parent_chain: vec![],
            chunk_type: "paragraph".to_string(),
            token_count: current_tokens,
        });
    }

    if chunks.is_empty() {
        chunks.push(Chunk {
            content: content.to_string(),
            heading: None,
            parent_heading: None,
            parent_chain: vec![],
            chunk_type: "file".to_string(),
            token_count: estimate_tokens(content),
        });
    }

    Ok(chunks)
}

/// One chunk per entire file.
fn chunk_by_file(content: &str) -> anyhow::Result<Vec<Chunk>> {
    Ok(vec![Chunk {
        content: content.to_string(),
        heading: None,
        parent_heading: None,
        parent_chain: vec![],
        chunk_type: "file".to_string(),
        token_count: estimate_tokens(content),
    }])
}

/// Rough token estimate: ~4 chars per token for English text
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_headings() {
        let md = "# Title\n\nSome intro text.\n\n## Section A\n\nContent for section A.\n\n## Section B\n\nContent for section B.";
        let chunks = chunk(md, "section", 1000).unwrap();
        assert_eq!(chunks.len(), 2, "Should have 2 H2 chunks, got {}", chunks.len());
        assert_eq!(chunks[0].heading.as_deref(), Some("Section A"));
        assert_eq!(chunks[1].heading.as_deref(), Some("Section B"));
    }

    #[test]
    fn test_heading_hierarchy() {
        let md = "# Guide\n\nIntro.\n\n## API\n\nAPI details.\n\n### Auth\n\nAuth details.\n\n### Rate Limit\n\nRate limit details.\n\n## CLI\n\nCLI details.";
        let chunks = chunk(md, "section", 1000).unwrap();
        assert_eq!(chunks.len(), 2, "Should have 2 H2 chunks, got {}", chunks.len());

        let all_content: String = chunks.iter().map(|c| c.content.clone()).collect::<Vec<_>>().join("\n");
        assert!(all_content.contains("Auth"));
        assert!(all_content.contains("Rate Limit"));
        assert!(all_content.contains("CLI details"));
    }

    #[test]
    fn test_parent_chain() {
        let md = "# Project\n\n## Setup\n\n### Prerequisites\n\nInstall Rust.\n\n### Install\n\nRun cargo.";
        let chunks = chunk(md, "section", 1000).unwrap();
        assert_eq!(chunks.len(), 1, "Should have 1 H2 chunk, got {}", chunks.len());
        assert_eq!(chunks[0].parent_chain.len(), 2);
        assert_eq!(chunks[0].parent_chain[0], "Project");
        assert_eq!(chunks[0].parent_chain[1], "Setup");
    }

    #[test]
    fn test_code_block_integrity() {
        let md = "## Setup\n\nHere is some code:\n\n```rust\nfn main() {\n    println!(\"hello\");\n    let x = 42;\n}\n```\n\nAnd more text.";
        let chunks = chunk(md, "section", 1000).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("main()"));
        assert!(chunks[0].content.contains("let x = 42"));
    }

    #[test]
    fn test_chunk_by_file() {
        let md = "# A\n\nText.\n\n## B\n\nMore.\n\n### C\n\nEven more.";
        let chunks = chunk(md, "file", 100).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, "file");
    }

    #[test]
    fn test_chunk_by_h1() {
        let md = "# Doc One\n\nContent 1.\n\n# Doc Two\n\nContent 2.";
        let chunks = chunk(md, "h1", 1000).unwrap();
        assert_eq!(chunks.len(), 2, "Should split at H1 boundaries, got {}", chunks.len());
    }

    #[test]
    fn test_chunk_types() {
        let md = "# Title\n\n## H2 Section\n\nH2 content.\n\n### H3 Sub\n\nH3 content.";
        let chunks = chunk(md, "section", 1000).unwrap();
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(!c.chunk_type.is_empty());
        }
    }
}
