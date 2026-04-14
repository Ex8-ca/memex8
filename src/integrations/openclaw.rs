use crate::config::AppConfig;

/// Print the webhook config users paste into their OpenClaw config.
pub fn configure(
    _config: &AppConfig,
    base_url: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    println!("🦞 Add this to your OpenClaw config:");
    println!();
    println!("hooks:");
    println!("  on_conversation_end:");
    println!("    - type: webhook");
    println!("      url: {}/api/v1/webhooks/conversation", base_url);
    println!("      method: POST");
    println!("      headers:");
    println!("        Authorization: \"Bearer {}\"", api_key);
    println!("        Content-Type: application/json");
    println!("      body_template: |");
    println!("        {{{{");
    println!("          \"summary\": \"{{{{{{{{conversation_summary}}}}}}}}\",");
    println!("          \"source\": \"openclaw\",");
    println!("          \"platform\": \"{{{{{{{{platform_name}}}}}}}}\"");
    println!("        }}}}");
    println!();
    println!("  on_skill_executed:");
    println!("    - type: webhook");
    println!("      url: {}/api/v1/webhooks/skill", base_url);
    println!("      method: POST");
    println!("      headers:");
    println!("        Authorization: \"Bearer {}\"", api_key);
    println!("        Content-Type: application/json");
    println!("      body_template: |");
    println!("        {{{{");
    println!("          \"skill_name\": \"{{{{{{{{skill_name}}}}}}}}\",");
    println!("          \"skill_category\": \"{{{{{{{{skill_category}}}}}}}}\",");
    println!("          \"status\": \"{{{{{{{{skill_status}}}}}}}}\",");
    println!("          \"input\": {{{{{{{{skill_input}}}}}}}},");
    println!("          \"output\": {{{{{{{{skill_output}}}}}}}}");
    println!("        }}}}");
    println!();
    println!("Then restart OpenClaw.");
    Ok(())
}
