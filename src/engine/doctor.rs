use crate::config::AppConfig;

pub async fn run(config: &AppConfig) -> anyhow::Result<()> {
    println!("🩺 memex8 doctor — running diagnostics...\n");

    // Check Qdrant connectivity
    print!("  Qdrant ({}) ... ", config.qdrant.url);
    match reqwest::Client::new().get(&format!("{}/healthz", config.qdrant.url)).send().await {
        Ok(resp) if resp.status().is_success() => println!("✅ OK"),
        Ok(resp) => println!("❌ HTTP {}", resp.status()),
        Err(e) => println!("❌ Connection failed: {}", e),
    }

    // Check embedding provider
    match config.embedding.provider.as_str() {
        "ollama" => {
            print!("  Ollama ({}) ... ", config.embedding.ollama.url);
            match reqwest::Client::new()
                .get(&format!("{}/api/tags", config.embedding.ollama.url))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await?;
                    let models: Vec<&str> = body["models"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|m| m["name"].as_str()).collect())
                        .unwrap_or_default();
                    let has_model = models.iter().any(|m| m.contains(&config.embedding.model));
                    if has_model {
                        println!("✅ Model '{}' found", config.embedding.model);
                    } else {
                        println!("⚠️  Model '{}' not found. Available: {:?}", config.embedding.model, models);
                        println!("     Run: ollama pull {}", config.embedding.model);
                    }
                }
                Err(e) => println!("❌ Connection failed: {}", e),
                resp => println!("❌ HTTP {:?}", resp),
            }
        }
        "openai" => {
            print!("  OpenAI API ... ");
            match config.openai_api_key() {
                Some(_) => println!("✅ API key set"),
                None => println!("❌ OPENAI_API_KEY not set"),
            }
        }
        _ => println!("⚠️  Unknown embedding provider: {}", config.embedding.provider),
    }

    // Check config validity
    print!("  Config ... ");
    if config.embedding.dimensions > 0 {
        println!("✅ OK ({}d embeddings)", config.embedding.dimensions);
    } else {
        println!("❌ Invalid dimensions: {}", config.embedding.dimensions);
    }

    println!("\n✅ Doctor complete.");
    Ok(())
}
