//! File watcher powered by the `notify` crate.
//!
//! Watches configured directories for file changes, debounces events at 500ms,
//! compares SHA-256 hashes to skip unchanged files, and reports which files changed.


use crate::config::WatchConfig;
use notify::{
    event::{AccessKind, AccessMode, ModifyKind},
    EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};

/// Debounce delay after the last filesystem event before reporting changes.
const DEBOUNCE_DELAY: Duration = Duration::from_millis(500);

/// A filesystem change event reported by the watcher.
#[derive(Debug, Clone)]
pub enum FileChangeEvent {
    /// File content was modified (old_hash → new_hash).
    Modified {
        path: PathBuf,
        new_hash: String,
        watch_config: WatchConfig,
    },
    /// New file detected.
    Created {
        path: PathBuf,
        hash: String,
        watch_config: WatchConfig,
    },
    /// File deleted.
    Deleted {
        path: PathBuf,
        watch_config: WatchConfig,
    },
}

/// A single watched directory with its configuration.
#[derive(Debug, Clone)]
struct WatchEntry {
    config: WatchConfig,
    /// SHA-256 hashes of all known files in this watch (path → hash).
    known_hashes: Arc<RwLock<HashMap<PathBuf, String>>>,
}

use std::sync::Arc;

/// Manages file watchers for multiple directories.
/// Reports change events over a channel; the caller handles re-ingestion.
pub struct FileWatcher {
    /// Map of absolute path → watch entry.
    watches: Arc<RwLock<HashMap<PathBuf, WatchEntry>>>,
    /// The underlying notify watcher (None until `start` is called).
    notify_watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    /// Channel sender for file paths that triggered events.
    event_tx: mpsc::UnboundedSender<PathBuf>,
}

impl FileWatcher {
    /// Create a new FileWatcher.
    /// Returns the watcher and a receiver for change events.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<PathBuf>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<PathBuf>();
        let watcher = Self {
            watches: Arc::new(RwLock::new(HashMap::new())),
            notify_watcher: Arc::new(Mutex::new(None)),
            event_tx,
        };
        (watcher, event_rx)
    }

    /// Start the notify watcher for the given watch configurations.
    pub async fn start(&self, watches: Vec<WatchConfig>) -> anyhow::Result<()> {
        // Stop any existing watcher
        {
            let mut guard = self.notify_watcher.lock().await;
            *guard = None;
        }

        let mut registered = 0;

        for watch_config in watches {
            let path = PathBuf::from(&watch_config.path);
            let abs_path = path.canonicalize().or_else(|_| {
                Ok::<_, std::io::Error>(path.clone())
            })?;

            let hashes = Self::scan_directory(&abs_path)?;
            tracing::info!(
                "📂 Watching '{}' — {} files (hashes recorded)",
                abs_path.display(),
                hashes.len()
            );

            let entry = WatchEntry {
                config: watch_config,
                known_hashes: Arc::new(RwLock::new(hashes)),
            };

            self.watches.write().await.insert(abs_path.clone(), entry);
            registered += 1;
        }

        // Create the notify watcher
        let event_tx = self.event_tx.clone();
        let mut notify_watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let should_process = matches!(
                        event.kind,
                        EventKind::Modify(ModifyKind::Data(_))
                            | EventKind::Modify(ModifyKind::Name(_))
                            | EventKind::Create(_)
                            | EventKind::Remove(_)
                            | EventKind::Access(AccessKind::Close(AccessMode::Write))
                    );

                    if should_process {
                        for path in &event.paths {
                            if path.extension().map(|e| e == "md").unwrap_or(false) {
                                let _ = event_tx.send(path.clone());
                            }
                        }
                    }
                }
            },
            notify::Config::default().with_poll_interval(Duration::from_secs(2)),
        )?;

        // Register all watch directories with notify
        for path in self.watches.read().await.keys() {
            if path.exists() {
                if let Err(e) = notify_watcher.watch(path, RecursiveMode::Recursive) {
                    tracing::warn!("Could not watch {}: {}", path.display(), e);
                }
            }
        }

        {
            let mut guard = self.notify_watcher.lock().await;
            *guard = Some(notify_watcher);
        }

        tracing::info!("👁️  File watcher started: {} directories registered", registered);
        Ok(())
    }

    /// Add a directory to the watch list at runtime.
    pub async fn add_watch(&self, config: WatchConfig) -> anyhow::Result<()> {
        let path = PathBuf::from(&config.path);
        let abs_path = path.canonicalize().or_else(|_| Ok::<_, std::io::Error>(path.clone()))?;

        if self.watches.read().await.contains_key(&abs_path) {
            tracing::info!("Already watching: {}", abs_path.display());
            return Ok(());
        }

        let hashes = Self::scan_directory(&abs_path)?;
        let entry = WatchEntry {
            config: config.clone(),
            known_hashes: Arc::new(RwLock::new(hashes)),
        };

        self.watches.write().await.insert(abs_path.clone(), entry);

        // Register with notify if running
        if let Some(watcher) = self.notify_watcher.lock().await.as_mut() {
            if abs_path.exists() {
                let _ = watcher.watch(&abs_path, RecursiveMode::Recursive);
            }
        }

        tracing::info!("📂 Added watch: {} (chunk: {})", abs_path.display(), config.chunk_by);
        Ok(())
    }

    /// Remove a directory from the watch list.
    pub async fn remove_watch(&self, path: &str) -> anyhow::Result<()> {
        let path = PathBuf::from(path);
        let abs_path = path.canonicalize().or_else(|_| Ok::<_, std::io::Error>(path.clone()))?;

        if let Some(watcher) = self.notify_watcher.lock().await.as_mut() {
            let _ = watcher.unwatch(&abs_path);
        }

        self.watches.write().await.remove(&abs_path);
        tracing::info!("🗑️  Removed watch: {}", abs_path.display());
        Ok(())
    }

    /// List all currently watched directories.
    pub async fn list_watches(&self) -> Vec<(String, String, Option<String>, String)> {
        let watches = self.watches.read().await;
        watches
            .iter()
            .map(|(path, entry)| {
                (
                    path.display().to_string(),
                    entry.config.chunk_by.clone(),
                    entry.config.realm_hint.clone(),
                    entry.config.poll_interval.clone(),
                )
            })
            .collect()
    }

    /// Get watch configs.
    pub async fn get_watch_configs(&self) -> Vec<WatchConfig> {
        self.watches.read().await.values().map(|e| e.config.clone()).collect()
    }

    /// Check which files have actually changed and return the list of modified paths.
    /// This does SHA-256 comparison to skip files that haven't changed.
    pub async fn check_changes(&self, paths: &[PathBuf]) -> Vec<FileChangeEvent> {
        let mut events = Vec::new();
        let watches = self.watches.read().await;

        for path in paths {
            // Find which watch this file belongs to
            let matching = watches.iter().find(|(watch_path, _)| {
                path.starts_with(watch_path.as_path())
            });

            let Some((_watch_path, entry)) = matching else {
                continue;
            };

            let watch_config = entry.config.clone();

            if !path.exists() {
                // File was deleted
                entry.known_hashes.write().await.remove(path);
                events.push(FileChangeEvent::Deleted {
                    path: path.clone(),
                    watch_config,
                });
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read {}: {}", path.display(), e);
                    continue;
                }
            };

            let new_hash = Self::hash_content(&content);
            let known_hashes = entry.known_hashes.read().await;

            match known_hashes.get(path) {
                Some(old_hash) if old_hash == &new_hash => {
                    // Unchanged — skip
                    continue;
                }
                Some(_) => {
                    // Modified
                    events.push(FileChangeEvent::Modified {
                        path: path.clone(),
                        new_hash: new_hash.clone(),
                        watch_config,
                    });
                }
                None => {
                    // New file
                    events.push(FileChangeEvent::Created {
                        path: path.clone(),
                        hash: new_hash.clone(),
                        watch_config,
                    });
                }
            }
            drop(known_hashes);

            // Update the hash
            entry.known_hashes.write().await.insert(path.clone(), new_hash);
        }

        events
    }

    /// Scan a directory and compute SHA-256 hashes for all .md files.
    fn scan_directory(dir: &Path) -> anyhow::Result<HashMap<PathBuf, String>> {
        let mut hashes = HashMap::new();
        Self::walk_md_files(dir, &mut hashes)?;
        Ok(hashes)
    }

    fn walk_md_files(dir: &Path, hashes: &mut HashMap<PathBuf, String>) -> anyhow::Result<()> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with('.') || name == "node_modules"
                            || name == "target" || name == ".git"
                        {
                            continue;
                        }
                    }
                    Self::walk_md_files(&path, hashes)?;
                } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        hashes.insert(path.clone(), Self::hash_content(&content));
                    }
                }
            }
        }
        Ok(())
    }

    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Persist the current watch list to the config file.
    pub async fn persist_watches(&self, config_path: &str) -> anyhow::Result<()> {
        let watches = self.get_watch_configs().await;

        let content = std::fs::read_to_string(config_path)
            .unwrap_or_else(|_| Self::default_config_content());

        let mut config_toml: toml::Value =
            toml::from_str(&content).unwrap_or(toml::Value::Table(Default::default()));

        let watch_array: Vec<toml::Value> = watches
            .iter()
            .map(|w| {
                let mut table = toml::map::Map::new();
                table.insert("path".to_string(), toml::Value::String(w.path.clone()));
                table.insert("chunk_by".to_string(), toml::Value::String(w.chunk_by.clone()));
                table.insert("poll_interval".to_string(), toml::Value::String(w.poll_interval.clone()));
                if let Some(ref hint) = w.realm_hint {
                    table.insert("realm_hint".to_string(), toml::Value::String(hint.clone()));
                }
                toml::Value::Table(table)
            })
            .collect();

        if let toml::Value::Table(ref mut table) = config_toml {
            table.insert("watch".to_string(), toml::Value::Array(watch_array));
        }

        let new_content = toml::to_string_pretty(&config_toml)?;
        std::fs::write(config_path, new_content)?;
        tracing::info!("💾 Persisted {} watch configs to {}", watches.len(), config_path);
        Ok(())
    }

    fn default_config_content() -> String {
        "[server]\nhost = \"0.0.0.0\"\nport = 8080\nmcp_port = 8081\n\n[auth]\napi_key_env = \"MEMEX8_API_KEY\"\n\n[embedding]\nprovider = \"ollama\"\nmodel = \"nomic-embed-text\"\ndimensions = 768\n\n[qdrant]\nurl = \"http://localhost:6333\"\n\nwatch = []\n".to_string()
    }
}
