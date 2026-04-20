use clap::{Parser, Subcommand};

mod api;
mod config;
mod engine;
mod integrations;
mod mcp;
mod storage;
mod web;

#[derive(Parser)]
#[command(name = "memex8")]
#[command(about = "Self-hosted AI memory system with Qdrant and ScalarQuant compression")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to config file
    #[arg(long, env = "MEMEX8_CONFIG", default_value = "config.toml")]
    config: String,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive setup wizard
    Init,

    /// Show current configuration
    ConfigShow,

    /// Ingest a .md file or directory
    Ingest {
        /// Path to file or directory
        path: String,

        /// Chunk strategy: section, paragraph, or file
        #[arg(long, default_value = "section")]
        chunk_by: String,

        /// Hint for realm assignment
        #[arg(long)]
        realm_hint: Option<String>,

        /// Start watching for changes (foreground)
        #[arg(long)]
        watch: bool,
    },

    /// Add a directory to the persistent watch list
    Watch {
        #[command(subcommand)]
        action: WatchActions,
    },

    /// Semantic search across all memories
    Search {
        /// Search query
        query: String,

        /// Filter to a specific realm
        #[arg(long)]
        realm: Option<String>,

        /// Number of results
        #[arg(long, default_value = "10")]
        limit: usize,

        /// Minimum similarity score (0.0-1.0)
        #[arg(long, default_value = "0.3")]
        min_score: f32,
    },

    /// Get a specific memory by ID
    Get {
        /// Memory UUID
        id: String,
    },

    /// Get highest-importance memories (wakeup context)
    Recall {
        /// Number of memories to recall
        #[arg(long, default_value = "10")]
        limit: usize,

        /// Filter to a specific realm
        #[arg(long)]
        realm: Option<String>,
    },

    /// Manage knowledge realms
    Realms {
        #[command(subcommand)]
        action: RealmActions,
    },

    /// Upvote a memory (increase importance)
    Upvote {
        /// Memory UUID
        id: String,
    },

    /// Show prune review queue
    Prune,

    /// Archive a memory
    Archive {
        /// Memory UUID
        id: String,
    },

    /// Permanently delete a memory
    Delete {
        /// Memory UUID
        id: String,

        /// Skip confirmation
        #[arg(long)]
        force: bool,
    },

    /// Edit a memory in $EDITOR
    Edit {
        /// Memory UUID
        id: String,
    },

    /// Slumber management
    Slumber {
        #[command(subcommand)]
        action: SlumberActions,
    },

    /// Start REST API + WebSocket server
    Serve {
        /// Host to bind to
        #[arg(long)]
        host: Option<String>,

        /// Port to bind to
        #[arg(long)]
        port: Option<u16>,
    },

    /// Start MCP server
    Mcp {
        /// Transport type
        #[arg(long, default_value = "stdio")]
        transport: String,

        /// Port for SSE transport
        #[arg(long)]
        port: Option<u16>,
    },

    /// Start background daemon (cron + idle slumber scheduler)
    Daemon,

    /// Show integration config for an AI agent (copy-paste into agent config)
    Integration {
        /// Target platform: openclaw, hermes, or pi
        platform: String,
    },

    /// Show system statistics
    Stats,

    /// Export all memories as JSON
    Export {
        /// Output file path
        #[arg(default_value = "memex8_export.json")]
        path: String,
    },

    /// Import memories from JSON
    Import {
        /// Input file path
        path: String,
        /// Reuse stored vectors instead of re-embedding (requires export with vectors)
        #[arg(long, default_value = "true")]
        reuse_vectors: bool,
    },

    /// Diagnose connectivity issues
    Doctor,
}

#[derive(Subcommand)]
enum WatchActions {
    /// Add a directory to watch
    Add {
        path: String,
        #[arg(long, default_value = "5m")]
        poll_interval: String,
        #[arg(long)]
        realm_hint: Option<String>,
        #[arg(long, default_value = "section")]
        chunk_by: String,
    },
    /// List watched directories
    List,
    /// Remove a watched directory
    Remove { path: String },
}

#[derive(Subcommand)]
enum RealmActions {
    /// List all realms
    List,
    /// Create a user-pinned realm
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Show realm details and top memories
    Show { name: String },
    /// Merge two realms
    Merge { a: String, b: String },
    /// Force-split a realm
    Split { name: String },
}

#[derive(Subcommand)]
enum SlumberActions {
    /// Show slumber state and schedule
    Status,
    /// Manually trigger slumber pipeline
    Trigger,
    /// Pause slumber (during heavy use)
    Pause,
    /// Resume slumber
    Resume,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging — always stderr so MCP can use stdout for JSON-RPC
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log_level.clone().into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Load config
    let config = config::AppConfig::load(&cli.config)?;

    match cli.command {
        Commands::Init => {
            println!("🧠 memex8 init — setting up your memory palace...\n");
            // TODO: interactive wizard
            println!("Configuration written to {}", cli.config);
        }
        Commands::ConfigShow => {
            println!("{}", toml::to_string_pretty(&config)?);
        }
        Commands::Ingest {
            path,
            chunk_by,
            realm_hint,
            watch,
        } => {
            let mut engine = engine::Engine::new(config).await?;
            engine.set_config_path(&cli.config);
            engine.ingest_path(&path, &chunk_by, realm_hint.as_deref()).await?;
            if watch {
                engine.watch_path(&path).await?;
            }
        }
        Commands::Watch { action } => {
            let mut engine = engine::Engine::new(config).await?;
            engine.set_config_path(&cli.config);
            match action {
                WatchActions::Add {
                    path,
                    poll_interval,
                    realm_hint,
                    chunk_by,
                } => {
                    engine
                        .watch_add(&path, &poll_interval, realm_hint.as_deref(), &chunk_by)
                        .await?;
                    println!("✅ Watching: {}", path);
                }
                WatchActions::List => {
                    engine.watch_list().await?;
                }
                WatchActions::Remove { path } => {
                    engine.watch_remove(&path).await?;
                    println!("🗑️  Removed watch: {}", path);
                }
            }
        }
        Commands::Search {
            query,
            realm,
            limit,
            min_score,
        } => {
            let engine = engine::Engine::new(config).await?;
            let results = engine.search(&query, realm.as_deref(), None, limit, 0, min_score).await?;
            for (i, result) in results.iter().enumerate() {
                println!(
                    "{}. [{}] {:.2} — {}",
                    i + 1,
                    result.realm_name,
                    result.score,
                    result.heading.as_deref().unwrap_or(&result.content.chars().take(80).collect::<String>())
                );
                println!("   ID: {}", result.id);
                println!();
            }
            println!("{} result(s)", results.len());
        }
        Commands::Get { id } => {
            let engine = engine::Engine::new(config).await?;
            let memory = engine.get_memory(&id).await?;
            println!("{}", serde_json::to_string_pretty(&memory)?);
        }
        Commands::Recall { limit, realm } => {
            let engine = engine::Engine::new(config).await?;
            let memories = engine.recall(limit, realm.as_deref()).await?;
            for m in &memories {
                println!("• [{}] (importance: {:.2}) {}", m.realm_name, m.importance, m.heading.as_deref().unwrap_or(&m.content.chars().take(80).collect::<String>()));
            }
        }
        Commands::Realms { action } => {
            let engine = engine::Engine::new(config).await?;
            match action {
                RealmActions::List => {
                    let realms = engine.list_realms().await?;
                    for r in &realms {
                        println!("• {} ({} memories)", r.name, r.memory_count);
                    }
                }
                RealmActions::Create { name, description } => {
                    engine.create_realm(&name, description.as_deref()).await?;
                    println!("✅ Created realm: {}", name);
                }
                RealmActions::Show { name } => {
                    let realm = engine.show_realm(&name).await?;
                    println!("{}", serde_json::to_string_pretty(&realm)?);
                }
                RealmActions::Merge { a, b } => {
                    engine.merge_realms(&a, &b).await?;
                    println!("✅ Merged '{}' into '{}'", b, a);
                }
                RealmActions::Split { name } => {
                    engine.split_realm(&name).await?;
                    println!("✅ Split realm: {}", name);
                }
            }
        }
        Commands::Upvote { id } => {
            let engine = engine::Engine::new(config).await?;
            engine.upvote(&id).await?;
            println!("👍 Upvoted: {}", id);
        }
        Commands::Prune => {
            let engine = engine::Engine::new(config).await?;
            let queue = engine.prune_queue().await?;
            println!("⚠️  Prune review queue ({} items):\n", queue.len());
            for m in &queue {
                println!("• {} [{}] — importance: {:.2}, last accessed: {}",
                    m.id, m.realm_name, m.importance, m.last_accessed);
            }
        }
        Commands::Archive { id } => {
            let engine = engine::Engine::new(config).await?;
            engine.archive_memory(&id).await?;
            println!("📦 Archived: {}", id);
        }
        Commands::Delete { id, force } => {
            if !force {
                println!("⚠️  Permanently delete memory {}? [y/N] ", id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled.");
                    return Ok(());
                }
            }
            let engine = engine::Engine::new(config).await?;
            engine.delete_memory(&id).await?;
            println!("🗑️  Deleted: {}", id);
        }
        Commands::Edit { id } => {
            let engine = engine::Engine::new(config).await?;
            let memory = engine.get_memory(&id).await?;

            // Open $EDITOR with memory content
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
            let tmp = std::env::temp_dir().join(format!("memex8_edit_{}.md", id));
            std::fs::write(&tmp, &memory.content)?;

            let status = std::process::Command::new(&editor)
                .arg(&tmp)
                .status()?;

            if status.success() {
                let new_content = std::fs::read_to_string(&tmp)?;
                if new_content != memory.content {
                    engine.edit_memory(&id, &new_content).await?;
                    println!("✏️  Updated memory: {}", id);
                } else {
                    println!("No changes detected.");
                }
            } else {
                eprintln!("Editor exited with status: {}", status);
            }

            let _ = std::fs::remove_file(&tmp);
        }
        Commands::Slumber { action } => {
            let engine = engine::Engine::new(config).await?;
            match action {
                SlumberActions::Status => {
                    let status = engine.slumber_status().await;
                    println!("{}", serde_json::to_string_pretty(&status)?);
                }
                SlumberActions::Trigger => {
                    println!("💤 Triggering slumber...");
                    engine.trigger_slumber().await?;
                    println!("✅ Slumber complete.");
                }
                SlumberActions::Pause => {
                    engine.pause_slumber().await;
                    println!("⏸️  Slumber paused.");
                }
                SlumberActions::Resume => {
                    engine.resume_slumber().await;
                    println!("▶️  Slumber resumed.");
                }
            }
        }
        Commands::Serve { host, port } => {
            let h = host.as_deref().unwrap_or(&config.server.host);
            let p = port.unwrap_or(config.server.port);
            api::server::run(config.clone(), h, p).await?;
        }
        Commands::Mcp { transport, port } => {
            match transport.as_str() {
                "stdio" => mcp::server::run_stdio(config.clone()).await?,
                "sse" => {
                    let p = port.unwrap_or(config.server.mcp_port);
                    mcp::server::run_sse(config.clone(), p).await?;
                }
                _ => anyhow::bail!("Unknown MCP transport: {}", transport),
            }
        }
        Commands::Daemon => {
            let engine = std::sync::Arc::new(engine::Engine::new(config.clone()).await?);
            tracing::info!("🧠 memex8 daemon starting...");
            let scheduler = engine::scheduler::Scheduler::new(engine.clone(), config.clone());
            let activity_handle = engine.activity_handle();

            // Start file watchers if configured
            let watch_handle = {
                let engine = engine.clone();
                let watch_rx = engine.start_watchers().await?;
                watch_rx.map(|rx| {
                    tokio::spawn(async move {
                        if let Err(e) = engine.handle_watch_events(rx).await {
                            tracing::error!("File watcher event handler error: {}", e);
                        }
                    })
                })
            };

            // Run the scheduler loop (blocks until shutdown)
            scheduler.run().await?;

            // Cancel watch handler on shutdown
            if let Some(handle) = watch_handle {
                handle.abort();
            }
        }
        Commands::Integration { platform } => {
            let base_url = std::env::var("MEMEX8_URL")
                .unwrap_or_else(|_| format!("http://localhost:{}", config.server.port));
            let api_key = config.api_key().unwrap_or_else(|| "YOUR_API_KEY".into());

            match platform.as_str() {
                "openclaw" => integrations::openclaw::configure(&config, &base_url, &api_key)?,
                "hermes" => integrations::hermes::configure(&config, &base_url, &api_key)?,
                "pi" => integrations::pi::generate_extension(&config)?,
                _ => anyhow::bail!("Unknown platform: {}. Use: openclaw, hermes, pi", platform),
            }
        }
        Commands::Stats => {
            let engine = engine::Engine::new(config).await?;
            let stats = engine.stats().await?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        Commands::Export { path } => {
            let engine = engine::Engine::new(config).await?;
            engine.export(&path).await?;
            println!("📤 Exported to: {}", path);
        }
        Commands::Import { path, reuse_vectors } => {
            let engine = engine::Engine::new(config).await?;
            let count = engine.import(&path, reuse_vectors).await?;
            if reuse_vectors {
                println!("📥 Imported {} memories from: {} (vectors reused)", count, path);
            } else {
                println!("📥 Imported {} memories from: {} (re-embedded)", count, path);
            }
        }
        Commands::Doctor => {
            println!("🩺 memex8 doctor — running diagnostics...\n");
            engine::doctor::run(&config).await?;
        }
    }

    Ok(())
}
