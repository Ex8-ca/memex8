//! Web-editable server settings.
//!
//! `config.toml` is bind-mounted read-only and read once at startup, so the UI
//! cannot edit it. Instead, a whitelist of safe tunables can be saved to an
//! overrides file (`settings.toml`, on the data volume) that is merged over
//! `config.toml` at the next start. Secrets, URLs and paths are deliberately
//! not editable here.
//!
//! Every route sits behind the normal auth middleware (session cookie or
//! Bearer key).

use crate::api::server::AppState;
use crate::config::AppConfig;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Kind {
    Float(f64, f64),
    Int(i64, i64),
    Bool,
}

struct Field {
    key: &'static str,
    group: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
}

const FIELDS: &[Field] = &[
    Field {
        key: "search.coverage.high",
        group: "Search",
        label: "Coverage: high",
        help: "Best-match score at or above which results count as well covered.",
        kind: Kind::Float(0.0, 1.0),
    },
    Field {
        key: "search.coverage.partial",
        group: "Search",
        label: "Coverage: partial",
        help: "Score below which results count as not covered. Must be lower than high.",
        kind: Kind::Float(0.0, 1.0),
    },
    Field {
        key: "slumber.dedup_threshold",
        group: "Slumber",
        label: "Duplicate similarity",
        help: "Memories at least this similar are treated as duplicates.",
        kind: Kind::Float(0.5, 1.0),
    },
    Field {
        key: "slumber.dedup_dry_run",
        group: "Slumber",
        label: "Dedup dry run",
        help: "On: duplicates are only reported. Off: slumber removes them.",
        kind: Kind::Bool,
    },
    Field {
        key: "slumber.auto_archive_days",
        group: "Slumber",
        label: "Auto-archive after (days)",
        help: "Untouched memories become prune-eligible after this many days.",
        kind: Kind::Int(1, 3650),
    },
    Field {
        key: "slumber.prune_threshold",
        group: "Slumber",
        label: "Prune threshold",
        help: "Memories whose recall score falls below this are archived.",
        kind: Kind::Float(0.0, 1.0),
    },
    Field {
        key: "slumber.decay_rate_per_day",
        group: "Slumber",
        label: "Decay rate per day",
        help: "How fast unused memories fade.",
        kind: Kind::Float(0.0, 1.0),
    },
    Field {
        key: "slumber.association_top_k",
        group: "Slumber",
        label: "Associations per memory",
        help: "Nearest neighbours linked to each memory.",
        kind: Kind::Int(1, 50),
    },
    Field {
        key: "slumber.association_min_strength",
        group: "Slumber",
        label: "Min association strength",
        help: "Weaker links are not stored.",
        kind: Kind::Float(0.0, 1.0),
    },
    Field {
        key: "slumber.topic_clusters_k",
        group: "Slumber",
        label: "Topic clusters",
        help: "Clusters used when grouping memories into realms.",
        kind: Kind::Int(1, 64),
    },
    Field {
        key: "verification.enabled",
        group: "Verification",
        label: "Verification sweep",
        help: "Periodically re-check memories for staleness.",
        kind: Kind::Bool,
    },
    Field {
        key: "verification.sample_size",
        group: "Verification",
        label: "Sample size",
        help: "Memories checked per sweep.",
        kind: Kind::Int(1, 500),
    },
    Field {
        key: "verification.min_interval_days",
        group: "Verification",
        label: "Re-check interval (days)",
        help: "Minimum days before a memory is re-checked.",
        kind: Kind::Int(1, 365),
    },
    Field {
        key: "verification.stale_threshold",
        group: "Verification",
        label: "Stale threshold",
        help: "Confidence below which a memory is marked stale.",
        kind: Kind::Float(0.0, 1.0),
    },
    Field {
        key: "verification.contradicted_threshold",
        group: "Verification",
        label: "Contradicted threshold",
        help: "Confidence below which a memory is marked contradicted.",
        kind: Kind::Float(0.0, 1.0),
    },
];

fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

fn json_lookup<'a>(v: &'a J, key: &str) -> Option<&'a J> {
    key.split('.').try_fold(v, |a, k| a.get(k))
}

fn toml_lookup<'a>(v: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(v, |a, k| a.get(k))
}

fn set_toml(root: &mut toml::Value, key: &str, val: toml::Value) {
    let mut cur = root;
    let parts: Vec<&str> = key.split('.').collect();
    for (i, part) in parts.iter().enumerate() {
        let table = cur.as_table_mut().expect("overrides root is a table");
        if i == parts.len() - 1 {
            table.insert((*part).to_string(), val);
            return;
        }
        cur = table
            .entry((*part).to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
    }
}

fn coerce(f: &Field, v: &J) -> Result<toml::Value, String> {
    match f.kind {
        Kind::Bool => v
            .as_bool()
            .map(toml::Value::Boolean)
            .ok_or_else(|| format!("{}: expected true or false", f.key)),
        Kind::Float(lo, hi) => {
            let x = v
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| format!("{}: expected a number", f.key))?;
            if x < lo || x > hi {
                return Err(format!("{}: must be between {lo} and {hi}", f.key));
            }
            Ok(toml::Value::Float(x))
        }
        Kind::Int(lo, hi) => {
            let x = v
                .as_i64()
                .or_else(|| v.as_f64().filter(|x| x.fract() == 0.0).map(|x| x as i64))
                .ok_or_else(|| format!("{}: expected a whole number", f.key))?;
            if x < lo || x > hi {
                return Err(format!("{}: must be between {lo} and {hi}", f.key));
            }
            Ok(toml::Value::Integer(x))
        }
    }
}

fn read_overrides(path: &std::path::Path) -> toml::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str::<toml::Value>(&t).ok())
        .filter(|v| v.is_table())
        .unwrap_or_else(|| toml::Value::Table(Default::default()))
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn toml_to_json(v: &toml::Value) -> J {
    match v {
        toml::Value::Float(x) => json!(round4(*x)),
        toml::Value::Integer(x) => json!(x),
        toml::Value::Boolean(x) => json!(x),
        other => json!(other.to_string()),
    }
}

fn same(running: &J, pending: &J) -> bool {
    match (running.as_f64(), pending.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-4,
        _ => running == pending,
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

pub async fn get(State(st): State<Arc<AppState>>) -> Response {
    let running = serde_json::to_value(&st.config).unwrap_or(J::Null);
    let path = st.config.settings_path();
    let overrides = read_overrides(&path);
    let mut restart_required = false;
    let fields: Vec<J> = FIELDS
        .iter()
        .map(|f| {
            let value = json_lookup(&running, f.key)
                .map(|v| v.as_f64().map(|x| json!(round4(x))).unwrap_or(v.clone()))
                .unwrap_or(J::Null);
            let pending = toml_lookup(&overrides, f.key).map(toml_to_json);
            let changed = pending.as_ref().is_some_and(|p| !same(&value, p));
            restart_required |= changed;
            let (ty, min, max) = match f.kind {
                Kind::Float(a, b) => ("float", json!(a), json!(b)),
                Kind::Int(a, b) => ("int", json!(a), json!(b)),
                Kind::Bool => ("bool", J::Null, J::Null),
            };
            json!({
                "key": f.key, "group": f.group, "label": f.label, "help": f.help,
                "type": ty, "min": min, "max": max,
                "value": value, "pending": pending, "changed": changed,
            })
        })
        .collect();
    Json(json!({
        "fields": fields,
        "restart_required": restart_required,
        "overrides_active": overrides.as_table().is_some_and(|t| !t.is_empty()),
        "overrides_path": path.display().to_string(),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct PutBody {
    pub values: BTreeMap<String, J>,
}

pub async fn put(State(st): State<Arc<AppState>>, Json(body): Json<PutBody>) -> Response {
    let path = st.config.settings_path();
    let mut overrides = read_overrides(&path);
    for (key, val) in &body.values {
        let Some(f) = field(key) else {
            return err(
                StatusCode::BAD_REQUEST,
                format!("{key}: not an editable setting"),
            );
        };
        match coerce(f, val) {
            Ok(v) => set_toml(&mut overrides, key, v),
            Err(e) => return err(StatusCode::BAD_REQUEST, e),
        }
    }

    // Cross-field validation: the full config must still be valid with the
    // overrides applied (e.g. coverage.partial < coverage.high).
    let mut merged = match toml::Value::try_from(&st.config) {
        Ok(v) => v,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("config serialize: {e}"),
            )
        }
    };
    AppConfig::merge_toml(&mut merged, &overrides);
    if let Err(e) = AppConfig::from_value(&merged) {
        return err(StatusCode::BAD_REQUEST, e.to_string());
    }

    let text = format!(
        "# Written by the memex8 web UI. Merged over config.toml at startup.\n# Delete this file (or use Reset in Settings) to return to config.toml.\n{}",
        toml::to_string_pretty(&overrides).unwrap_or_default()
    );
    let tmp = path.with_extension("toml.tmp");
    let written = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not write {}: {e}", path.display()),
        );
    }
    tracing::info!("settings overrides saved to {}", path.display());
    Json(json!({ "ok": true, "restart_required": true })).into_response()
}

pub async fn reset(State(st): State<Arc<AppState>>) -> Response {
    let path = st.config.settings_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not remove {}: {e}", path.display()),
            )
        }
    }
    Json(json!({ "ok": true, "restart_required": true })).into_response()
}

/// Exit cleanly shortly after replying; the container's `unless-stopped`
/// restart policy brings the server back with the new settings.
pub async fn restart() -> Response {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        tracing::warn!("restart requested from the web UI, exiting");
        std::process::exit(0);
    });
    Json(json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_key_exists_in_the_default_config() {
        let cfg = serde_json::to_value(AppConfig::default()).unwrap();
        for f in FIELDS {
            assert!(
                json_lookup(&cfg, f.key).is_some(),
                "missing in config: {}",
                f.key
            );
        }
    }

    #[test]
    fn default_values_pass_their_own_ranges() {
        let cfg = serde_json::to_value(AppConfig::default()).unwrap();
        for f in FIELDS {
            let v = json_lookup(&cfg, f.key).unwrap();
            assert!(coerce(f, v).is_ok(), "default out of range: {}", f.key);
        }
    }

    #[test]
    fn coerce_enforces_type_and_range() {
        let f = field("slumber.dedup_threshold").unwrap();
        assert!(coerce(f, &json!(0.9)).is_ok());
        assert!(coerce(f, &json!(1.5)).is_err());
        assert!(coerce(f, &json!("0.9")).is_err());
        let i = field("slumber.auto_archive_days").unwrap();
        assert!(coerce(i, &json!(14)).is_ok());
        assert!(coerce(i, &json!(14.0)).is_ok());
        assert!(coerce(i, &json!(14.5)).is_err());
        assert!(coerce(i, &json!(0)).is_err());
        let b = field("verification.enabled").unwrap();
        assert!(coerce(b, &json!(true)).is_ok());
        assert!(coerce(b, &json!(1)).is_err());
    }

    #[test]
    fn unknown_and_secret_keys_are_not_editable() {
        assert!(field("auth.api_key_env").is_none());
        assert!(field("qdrant.url").is_none());
        assert!(field("server.port").is_none());
    }

    #[test]
    fn set_toml_builds_nested_tables() {
        let mut root = toml::Value::Table(Default::default());
        set_toml(&mut root, "search.coverage.high", toml::Value::Float(0.7));
        set_toml(
            &mut root,
            "search.coverage.partial",
            toml::Value::Float(0.3),
        );
        assert_eq!(
            toml_lookup(&root, "search.coverage.high")
                .unwrap()
                .as_float(),
            Some(0.7)
        );
        assert_eq!(
            toml_lookup(&root, "search.coverage.partial")
                .unwrap()
                .as_float(),
            Some(0.3)
        );
    }

    #[test]
    fn cross_field_validation_rejects_partial_above_high() {
        let mut merged = toml::Value::try_from(AppConfig::default()).unwrap();
        let mut over = toml::Value::Table(Default::default());
        set_toml(
            &mut over,
            "search.coverage.partial",
            toml::Value::Float(0.9),
        );
        AppConfig::merge_toml(&mut merged, &over);
        assert!(AppConfig::from_value(&merged).is_err());
    }
}
