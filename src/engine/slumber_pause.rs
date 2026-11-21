//! The operator pause for slumber (scheduled and explicit runs).
//!
//! The flag lives in a file on the data volume, so it survives a restart and
//! any process sharing the volume sees it: the daemon's scheduler, the REST
//! routes, and the CLI, which can pause maintenance without building an Engine
//! (that needs Qdrant, and an operator may need to pause precisely when the
//! store is degraded). The file is the truth; the in-memory flag only mirrors
//! it for cheap status reads and is refreshed from the file by [`SlumberPause::refresh`].
use crate::config::AppConfig;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct SlumberPause {
    path: String,
    paused: AtomicBool,
}

impl SlumberPause {
    /// The pause flag for the data volume `config` points at.
    pub fn for_config(config: &AppConfig) -> Self {
        Self::at(pause_path(config))
    }

    fn at(path: String) -> Self {
        let paused = AtomicBool::new(read_flag(&path));
        Self { path, paused }
    }

    /// Where the flag is persisted.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// In-memory view of the flag, as of the last [`refresh`](Self::refresh) or [`set`](Self::set).
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Re-read the persisted flag.
    ///
    /// Every scheduler tick and every trigger goes through here, so a pause
    /// written by another process (a CLI sharing the data volume, an operator's
    /// script) takes effect without restarting the daemon.
    pub fn refresh(&self) -> bool {
        let paused = read_flag(&self.path);
        if paused != self.paused.swap(paused, Ordering::SeqCst) {
            tracing::info!(
                paused,
                path = %self.path,
                "Slumber pause flag changed on disk"
            );
        }
        paused
    }

    /// Persist the flag, then report success.
    ///
    /// The persisted write is what makes the pause real: it survives a restart
    /// and is visible to the scheduler of a *different* process on the same data
    /// volume. A write failure is returned, never swallowed, because reporting a
    /// pause the daemon would not honour is exactly the failure this replaces.
    pub fn set(&self, paused: bool) -> anyhow::Result<()> {
        write_flag(&self.path, paused)?;
        self.paused.store(paused, Ordering::SeqCst);
        if paused {
            tracing::info!(
                "⏸️  Slumber paused ({}); scheduled runs are refused",
                self.path
            );
        } else {
            tracing::info!("▶️  Slumber resumed");
        }
        Ok(())
    }
}

/// Where the pause flag is persisted.
///
/// It sits beside the compressed index, i.e. on the data volume, so it survives
/// a daemon restart and is visible to any other process (the CLI) that resolves
/// the same config and environment.
fn pause_path(config: &AppConfig) -> String {
    let (index_path, _) = super::compressed_index::compressed_index_paths(config);
    std::path::Path::new(&index_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join("slumber_paused.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("slumber_paused.json"))
        .to_string_lossy()
        .into_owned()
}

/// Read the persisted pause flag.
///
/// A missing, unreadable or malformed file means "not paused": the flag is an
/// operator convenience, so a truncated write must not silently disable
/// maintenance forever.
fn read_flag(path: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => value
                .get("paused")
                .and_then(|paused| paused.as_bool())
                .unwrap_or(false),
            Err(e) => {
                tracing::warn!("Ignoring malformed slumber pause file {path}: {e}");
                false
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            tracing::warn!("Slumber pause file {path} is unreadable: {e}");
            false
        }
    }
}

/// Durably write the pause flag (temporary sibling, then rename) before any
/// caller reports success.
fn write_flag(path: &str, paused: bool) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "paused": paused,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(parent) = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temp = format!("{path}.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&body)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_flag(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "memex8-slumber-pause-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("slumber_paused.json")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn flag_is_durable_and_fails_open() {
        let path = scratch_flag("roundtrip");
        // A missing flag must mean "not paused": otherwise a fresh data volume
        // could never run maintenance.
        assert!(!read_flag(&path));
        write_flag(&path, true).expect("write pause");
        assert!(read_flag(&path), "persisted pause was not readable");
        assert!(
            !std::path::Path::new(&format!("{path}.tmp")).exists(),
            "atomic write left a temporary file behind"
        );
        write_flag(&path, false).expect("write resume");
        assert!(!read_flag(&path));
        // A truncated flag fails open (with a warning) instead of disabling
        // maintenance silently and forever.
        std::fs::write(&path, b"{not json").expect("corrupt flag");
        assert!(!read_flag(&path));
        std::fs::remove_file(&path).ok();
    }

    /// The daemon and the CLI are two `SlumberPause` values over one file.
    #[test]
    fn a_pause_set_by_one_process_is_seen_by_another_on_refresh() {
        let path = scratch_flag("two-processes");
        let daemon = SlumberPause::at(path.clone());
        let cli = SlumberPause::at(path.clone());
        assert!(!daemon.is_paused());

        cli.set(true).expect("pause");
        assert!(
            !daemon.is_paused(),
            "the mirror only changes on refresh or set"
        );
        assert!(daemon.refresh());
        assert!(daemon.is_paused());

        cli.set(false).expect("resume");
        assert!(!daemon.refresh());
        assert!(!daemon.is_paused());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_new_instance_starts_from_the_persisted_flag() {
        let path = scratch_flag("restart");
        SlumberPause::at(path.clone()).set(true).expect("pause");
        assert!(SlumberPause::at(path.clone()).is_paused());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_write_failure_is_returned_and_leaves_the_mirror_alone() {
        let dir = std::env::temp_dir().join(format!("memex8-pause-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The flag path is an existing directory, so the rename cannot succeed.
        let path = dir.join("flag").to_string_lossy().into_owned();
        std::fs::create_dir_all(&path).unwrap();
        let pause = SlumberPause::at(path);
        assert!(pause.set(true).is_err());
        assert!(!pause.is_paused());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_path_sits_beside_the_compressed_index() {
        let mut config = AppConfig::default();
        config.turbovec.index_path = "/data/vol/memories.tv".into();
        assert_eq!(pause_path(&config), "/data/vol/slumber_paused.json");
    }
}
