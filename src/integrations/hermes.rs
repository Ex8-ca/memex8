use crate::config::AppConfig;

/// Print Hermes MCP server configuration
pub fn print_mcp_config(config: &AppConfig) -> anyhow::Result<()> {
    println!("# Hermes MCP Server Configuration");
    println!("# Add to ~/.hermes/config.yaml\n");

    println!("mcp_servers:");
    println!("  memex8:");

    // Check if we should use stdio or HTTP
    println!("    # Option 1: stdio transport (run memex8 locally)");
    println!("    transport: stdio");
    println!("    command: memex8");
    println!("    args: [\"mcp\"]");
    println!();
    println!("    # Option 2: HTTP/SSE transport");
    println!("    # transport: http");
    println!("    # url: http://localhost:{}/mcp", config.server.mcp_port);
    println!();
    println!("# After adding, restart Hermes. The following tools will be available:");
    println!("#   memex8_search, memex8_store, memex8_recall, memex8_get,");
    println!("#   memex8_ingest, memex8_realms_list, memex8_upvote, memex8_stats");

    Ok(())
}
