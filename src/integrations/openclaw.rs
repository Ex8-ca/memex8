use crate::config::AppConfig;

/// Print OpenClaw webhook hook configuration
pub fn print_hooks(config: &AppConfig) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server.port);
    let api_key = config.api_key().unwrap_or_else(|| "YOUR_API_KEY".into());

    println!("# OpenClaw Hook Configuration");
    println!("# Add to your OpenClaw workspace config\n");

    println!("hooks:");
    println!("  on_conversation_end:");
    println!("    - type: webhook");
    println!("      url: {}/api/v1/memories", base_url);
    println!("      method: POST");
    println!("      headers:");
    println!("        Authorization: \"Bearer {}\"", api_key);
    println!("        Content-Type: application/json");
    println!("      body_template: |");
    println!("        {{{{");
    println!("          \"content\": \"{{{{conversation_summary}}}}\",");
    println!("          \"tags\": [\"conversation\", \"{{{{platform}}}}\"],");
    println!("          \"source\": \"openclaw\"");
    println!("        }}}}");
    println!();

    println!("  on_skill_executed:");
    println!("    - type: webhook");
    println!("      url: {}/api/v1/memories", base_url);
    println!("      method: POST");
    println!("      headers:");
    println!("        Authorization: \"Bearer {}\"", api_key);
    println!("        Content-Type: application/json");
    println!("      body_template: |");
    println!("        {{{{");
    println!("          \"content\": \"# {{skill_name}}\\n{{{{skill_output}}}}\",");
    println!("          \"tags\": [\"skill\", \"{{{{skill_category}}}}\"],");
    println!("          \"source\": \"openclaw\"");
    println!("        }}}}");

    Ok(())
}
