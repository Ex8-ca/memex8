use crate::config::AppConfig;

/// Print the webhook config users paste into their Hermes config.
pub fn configure(_config: &AppConfig, base_url: &str, api_key: &str) -> anyhow::Result<()> {
    println!("🧠 Add this to ~/.hermes/config.yaml:");
    println!();
    println!("webhooks:");
    println!("  on_conversation_end:");
    println!("    - url: {}/api/v1/webhooks/conversation", base_url);
    println!("      method: POST");
    println!("      headers:");
    println!("        Authorization: Bearer {}", api_key);
    println!();
    println!("Then restart Hermes.");
    Ok(())
}
