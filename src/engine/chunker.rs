use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    pub heading: Option<String>,
    pub parent_heading: Option<String>,
    pub token_count: usize,
}

/// Chunk markdown content by strategy: "section" (H2), "paragraph", or "file"
pub fn chunk(content: &str, strategy: &str, max_tokens: u32) -> anyhow::Result<Vec<Chunk>> {
    match strategy {
        "section" => chunk_by_section(content, max_tokens),
        "paragraph" => chunk_by_paragraph(content, max_tokens),
        "file" => chunk_by_file(content),
        _ => chunk_by_section(content, max_tokens),
    }
}

fn chunk_by_section(content: &str, max_tokens: u32) -> anyhow::Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut parent_heading: Option<String> = None;
    let mut current_content = String::new();

    for line in content.lines() {
        if line.starts_with("## ") {
            // Save previous section
            if !current_content.trim().is_empty() {
                let tc = estimate_tokens(&current_content);
                if tc > max_tokens as usize {
                    // Split at ### boundaries
                    chunks.extend(split_large_section(
                        &current_content,
                        current_heading.as_deref(),
                        parent_heading.as_deref(),
                        max_tokens,
                    ));
                } else {
                    chunks.push(Chunk {
                        content: current_content.trim().to_string(),
                        heading: current_heading.clone(),
                        parent_heading: parent_heading.clone(),
                        token_count: tc,
                    });
                }
            }
            current_heading = Some(line.trim_start_matches("## ").trim().to_string());
            current_content = line.to_string() + "\n";
        } else if line.starts_with("# ") {
            parent_heading = Some(line.trim_start_matches("# ").trim().to_string());
        } else {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    // Flush last section
    if !current_content.trim().is_empty() {
        let tc = estimate_tokens(&current_content);
        chunks.push(Chunk {
            content: current_content.trim().to_string(),
            heading: current_heading,
            parent_heading,
            token_count: tc,
        });
    }

    if chunks.is_empty() {
        chunks.push(Chunk {
            content: content.to_string(),
            heading: None,
            parent_heading: None,
            token_count: estimate_tokens(content),
        });
    }

    Ok(chunks)
}

fn chunk_by_paragraph(content: &str, max_tokens: u32) -> anyhow::Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let paragraphs: Vec<&str> = content.split("\n\n").collect();
    let mut current = String::new();
    let mut current_tokens = 0;

    for para in paragraphs {
        let para_tokens = estimate_tokens(para);
        if current_tokens + para_tokens > max_tokens as usize && !current.is_empty() {
            chunks.push(Chunk {
                content: current.trim().to_string(),
                heading: None,
                parent_heading: None,
                token_count: current_tokens,
            });
            current.clear();
            current_tokens = 0;
        }
        current.push_str(para);
        current.push_str("\n\n");
        current_tokens += para_tokens;
    }

    if !current.trim().is_empty() {
        chunks.push(Chunk {
            content: current.trim().to_string(),
            heading: None,
            parent_heading: None,
            token_count: current_tokens,
        });
    }

    Ok(chunks)
}

fn chunk_by_file(content: &str) -> anyhow::Result<Vec<Chunk>> {
    Ok(vec![Chunk {
        content: content.to_string(),
        heading: None,
        parent_heading: None,
        token_count: estimate_tokens(content),
    }])
}

fn split_large_section(
    content: &str,
    heading: Option<&str>,
    parent_heading: Option<&str>,
    max_tokens: u32,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0;

    for line in content.lines() {
        if line.starts_with("### ") && current_tokens > max_tokens as usize / 2 {
            if !current.trim().is_empty() {
                chunks.push(Chunk {
                    content: current.trim().to_string(),
                    heading: heading.map(|h| h.to_string()),
                    parent_heading: parent_heading.map(|h| h.to_string()),
                    token_count: current_tokens,
                });
            }
            current = line.to_string() + "\n";
            current_tokens = estimate_tokens(line);
        } else {
            current.push_str(line);
            current.push('\n');
            current_tokens += estimate_tokens(line);
        }
    }

    if !current.trim().is_empty() {
        chunks.push(Chunk {
            content: current.trim().to_string(),
            heading: heading.map(|h| h.to_string()),
            parent_heading: parent_heading.map(|h| h.to_string()),
            token_count: current_tokens,
        });
    }

    chunks
}

/// Rough token estimate: ~4 chars per token for English text
fn estimate_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}
