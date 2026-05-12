use crate::config::AppConfig;
use crate::engine::Engine;
use chrono::{Datelike, Local, Timelike};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

/// Background scheduler that triggers slumber on cron and idle.
pub struct Scheduler {
    engine: Arc<Engine>,
    config: AppConfig,
    last_activity: Arc<RwLock<Instant>>,
}

impl Scheduler {
    pub fn new(engine: Arc<Engine>, config: AppConfig) -> Self {
        Self {
            engine,
            config,
            last_activity: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Get a handle to reset the activity timer (call after each search/ingest/query).
    pub fn activity_handle(&self) -> Arc<RwLock<Instant>> {
        self.last_activity.clone()
    }

    /// Check if the consolidation schedule should fire now.
    /// Uses wall-clock time instead of elapsed-time checks so it properly
    /// matches cron expressions like "0 3 * * *" (daily at 3am).
    /// Also prevents double-firing: returns false if consolidation already
    /// ran in the past `min_interval_hours`.
    fn should_consolidate_now(
        &self,
        last_consolidation: &mut Option<chrono::DateTime<Local>>,
        min_interval_hours: u64,
    ) -> bool {
        let schedule = &self.config.slumber.consolidation_schedule;
        if schedule.is_empty() {
            return false;
        }

        let now = Local::now();

        // If we already consolidated recently, don't fire again
        if let Some(last) = last_consolidation {
            let elapsed = now.signed_duration_since(*last);
            if elapsed.num_hours() < min_interval_hours as i64 {
                return false;
            }
        }

        // Check if current wall-clock time matches the consolidation schedule
        if should_run_at_schedule(schedule) {
            *last_consolidation = Some(now);
            return true;
        }

        false
    }

    /// Run the scheduler loop. Blocks until the process is shut down.
    pub async fn run(self) -> anyhow::Result<()> {
        let idle_timeout = parse_duration(&self.config.slumber.idle_timeout)?;
        let cron_interval = parse_cron_to_duration(&self.config.slumber.cron_ingest)?;

        tracing::info!(
            "🕐 Scheduler started: idle_timeout={}, cron_interval={}",
            self.config.slumber.idle_timeout,
            self.config.slumber.cron_ingest
        );

        let mut idle_check = tokio::time::interval(Duration::from_secs(30));
        let mut cron_check = tokio::time::interval(cron_interval);

        // Skip the first immediate tick
        idle_check.tick().await;
        cron_check.tick().await;

        let mut last_cron_run = Instant::now() - cron_interval;
        let mut last_consolidation: Option<chrono::DateTime<Local>> = None;

        loop {
            tokio::select! {
                // Check idle timeout every 30 seconds
                _ = idle_check.tick() => {
                    let last = self.last_activity.read().await;
                    let idle_duration = last.elapsed();

                    if idle_duration >= idle_timeout {
                        tracing::info!(
                            "💤 Idle for {:?}, triggering slumber...",
                            idle_duration
                        );
                        drop(last); // Release read lock

                        // Check if consolidation should run (wall-clock schedule)
                        let force_consolidation = self.should_consolidate_now(
                            &mut last_consolidation,
                            12, // min 12h between consolidations
                        );

                        match self.engine.trigger_slumber(force_consolidation).await {
                            Ok(report) => {
                                tracing::info!(
                                    "💤 Idle slumber complete: dedup={} quant={} consolidated={} prune={} md={}",
                                    report.deduplicated,
                                    report.quantized,
                                    report.memories_consolidated,
                                    report.flagged_for_prune,
                                    report.memex8_md_written,
                                );
                            }
                            Err(e) => tracing::error!("Slumber failed: {}", e),
                        }

                        // Reset activity timer so we don't immediately trigger again
                        *self.last_activity.write().await = Instant::now();
                    }
                }

                // Check cron schedule
                _ = cron_check.tick() => {
                    // Only run if enough time has passed since last cron run
                    if last_cron_run.elapsed() >= cron_interval {
                        tracing::info!("🕐 Cron trigger — running slumber...");

                        // Check if consolidation should run (wall-clock schedule)
                        let force_consolidation = self.should_consolidate_now(
                            &mut last_consolidation,
                            12, // min 12h between consolidations
                        );

                        match self.engine.trigger_slumber(force_consolidation).await {
                            Ok(report) => {
                                tracing::info!(
                                    "🕐 Cron slumber complete: dedup={} quant={} consolidated={} prune={} md={}",
                                    report.deduplicated,
                                    report.quantized,
                                    report.memories_consolidated,
                                    report.flagged_for_prune,
                                    report.memex8_md_written,
                                );
                            }
                            Err(e) => tracing::error!("Cron slumber failed: {}", e),
                        }
                        last_cron_run = Instant::now();
                    }
                }
            }
        }
    }
}

/// Parse a duration string like "10m", "1h", "30s" into Duration.
fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Duration::from_secs(600)); // default 10m
    }

    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;

    match unit {
        "s" => Ok(Duration::from_secs(num)),
        "m" => Ok(Duration::from_secs(num * 60)),
        "h" => Ok(Duration::from_secs(num * 3600)),
        "d" => Ok(Duration::from_secs(num * 86400)),
        _ => anyhow::bail!("Unknown duration unit: {} (use s, m, h, d)", unit),
    }
}

/// Parse a cron expression into a Duration for periodic checking.
/// Supports basic intervals: "*/N * * * *" (every N minutes).
/// Falls back to a reasonable default for complex expressions.
fn parse_cron_to_duration(cron_expr: &str) -> anyhow::Result<Duration> {
    let cron_expr = cron_expr.trim();

    // Handle "*/N * * * *" pattern (every N minutes)
    if let Some(star_n) = cron_expr.strip_prefix("*/") {
        if let Some(space_idx) = star_n.find(' ') {
            let minutes: u64 = star_n[..space_idx]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid cron expression: {}", cron_expr))?;
            return Ok(Duration::from_secs(minutes * 60));
        }
    }

    // Handle standard cron expressions like "0 3 * * *" (daily at specific time)
    // For these, parse the hour and minute and return the appropriate interval
    let parts: Vec<&str> = cron_expr.split_whitespace().collect();
    if parts.len() == 5 {
        // minute hour day month weekday
        let hour = parts[1];
        let minute = parts[0];
        let day = parts[2];
        let month = parts[3];
        let weekday = parts[4];

        // If day, month, and weekday are all *, it's a daily schedule
        if day == "*" && month == "*" && weekday == "*" {
            let h: u64 = hour.parse().unwrap_or(0);
            let m: u64 = minute.parse().unwrap_or(0);
            let total_minutes = h * 60 + m;
            if total_minutes > 0 {
                // Run once per day at the specified time
                return Ok(Duration::from_secs(24 * 3600));
            }
        }

        // Hourly schedule: "0 * * * *"
        if hour == "*" && day == "*" && month == "*" && weekday == "*" {
            return Ok(Duration::from_secs(3600));
        }

        // Every N hours: "0 */4 * * *"
        if let Some(star_n) = hour.strip_prefix("*/") {
            let hours: u64 = star_n.parse().unwrap_or(4);
            return Ok(Duration::from_secs(hours * 3600));
        }
    }

    // Default: every 5 minutes
    tracing::warn!(
        "Complex cron expression '{}' not fully parsed, defaulting to 5 minutes",
        cron_expr
    );
    Ok(Duration::from_secs(300))
}

/// Check if the current time matches a cron expression.
/// Returns true if we should run the scheduled task now.
const CRON_TOLERANCE_MINUTES: u64 = 5;

pub fn should_run_at_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }

    let now = chrono::Local::now();
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }

    let current_minute = now.minute() as u64;
    let current_hour = now.hour() as u64;
    let current_day = now.day() as u64;
    let current_month = now.month() as u64;
    let current_weekday = now.weekday().num_days_from_sunday() as u64;

    // Apply tolerance window to the minute field. The cron ingest ticks every
    // 5 minutes, so exact minute matching means consolidation almost never fires.
    // Expand single-value minutes (e.g. "0") into a ±5 minute range.
    let minute_field = expand_minute_with_tolerance(parts[0], CRON_TOLERANCE_MINUTES);
    if !matches_field(&minute_field, current_minute) {
        return false;
    }
    if !matches_field(parts[1], current_hour) {
        return false;
    }
    if !matches_field(parts[2], current_day) {
        return false;
    }
    if !matches_field(parts[3], current_month) {
        return false;
    }
    if !matches_field(parts[4], current_weekday) {
        return false;
    }

    true
}

/// If the minute field is a single value (e.g. "0"), expand it to a range
/// with tolerance. Preserves *, */N, ranges, and comma-separated fields.
fn expand_minute_with_tolerance(field: &str, tolerance: u64) -> String {
    if field == "*" || field.starts_with("*/") || field.contains(',') || field.contains('-') {
        return field.to_string();
    }
    if let Ok(target) = field.parse::<u64>() {
        let start = target.saturating_sub(tolerance);
        let end = (target + tolerance).min(59);
        if start == end {
            return field.to_string();
        }
        return format!("{}-{}", start, end);
    }
    field.to_string()
}

/// Check if a value matches a cron field (minute, hour, etc.)
fn matches_field(field: &str, value: u64) -> bool {
    if field == "*" {
        return true;
    }

    // Handle "*/N" (every N)
    if let Some(star_n) = field.strip_prefix("*/") {
        if let Ok(n) = star_n.parse::<u64>() {
            if n > 0 {
                return value % n == 0;
            }
        }
    }

    // Handle specific value
    if let Ok(n) = field.parse::<u64>() {
        return value == n;
    }

    // Handle comma-separated values
    if field.contains(',') {
        return field
            .split(',')
            .map(|s| s.trim())
            .any(|s| matches_field(s, value));
    }

    // Handle ranges like "1-5"
    if field.contains('-') {
        let parts: Vec<&str> = field.split('-').collect();
        if parts.len() == 2 {
            if let (Ok(start), Ok(end)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
                return value >= start && value <= end;
            }
        }
    }

    false
}
