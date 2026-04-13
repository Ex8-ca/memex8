use crate::config::AppConfig;

/// Generate a pi.dev extension that registers memex8 tools
pub fn generate_extension(config: &AppConfig) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server.port);

    println!(
        r#"// memex8 pi.dev extension
// Place in: ~/.pi/agent/extensions/memex8.ts
// This extension registers memex8 memory tools for the pi coding agent.

const BASE_URL = "{}"#,
        base_url
    );
    println!(
        r#";
const API_KEY = process.env.MEMEX8_API_KEY || "";

async function memex8Fetch(endpoint: string, options: RequestInit = {{}}): Promise<any> {{
    const resp = await fetch(`${{BASE_URL}}/api/v1${{endpoint}}`, {{
        ...options,
        headers: {{
            "Authorization": `Bearer ${{API_KEY}}`,
            "Content-Type": "application/json",
            ...options.headers,
        }},
    }});
    if (!resp.ok) throw new Error(`memex8 error: ${{resp.status}}`);
    return resp.json();
}}

export const tools = {{
    memex8_search: {{
        description: "Search memex8 memory for relevant context",
        parameters: {{
            query: {{ type: "string" }},
            limit: {{ type: "number" }},
            realm: {{ type: "string" }},
        }},
        execute: async (params: any) => {{
            return memex8Fetch("/memories/search", {{
                method: "POST",
                body: JSON.stringify(params),
            }});
        }},
    }},

    memex8_store: {{
        description: "Store a memory in memex8",
        parameters: {{
            content: {{ type: "string" }},
            tags: {{ type: "array", items: {{ type: "string" }} }},
            realm_hint: {{ type: "string" }},
        }},
        execute: async (params: any) => {{
            return memex8Fetch("/memories", {{
                method: "POST",
                body: JSON.stringify(params),
            }});
        }},
    }},

    memex8_recall: {{
        description: "Get the most important memories",
        parameters: {{
            limit: {{ type: "number" }},
        }},
        execute: async (params: any) => {{
            const query = new URLSearchParams(params).toString();
            return memex8Fetch(`/memories/recall?${{query}}`);
        }},
    }},

    memex8_ingest: {{
        description: "Ingest a file or directory into memex8",
        parameters: {{
            path: {{ type: "string" }},
            chunk_by: {{ type: "string", enum: ["section", "paragraph", "file"] }},
        }},
        execute: async (params: any) => {{
            return memex8Fetch("/memories/ingest", {{
                method: "POST",
                body: JSON.stringify(params),
            }});
        }},
    }},
}};
"#
    );

    Ok(())
}
