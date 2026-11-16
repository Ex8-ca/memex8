use crate::config::AppConfig;
use crate::engine::Engine;
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use cron::Schedule;
use std::str::FromStr;
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
        let schedule = parse_cron_schedule(&self.config.slumber.cron_ingest)?;
        let mut next_cron_run = next_cron_after(&schedule, Local::now())?;

        tracing::info!(
            "🕐 Scheduler started: idle_timeout={}, cron_schedule={}",
            self.config.slumber.idle_timeout,
            self.config.slumber.cron_ingest
        );

        let mut idle_check = tokio::time::interval(Duration::from_secs(30));
        // Poll wall time so clock changes do not turn cron into an elapsed timer.
        let mut cron_check = tokio::time::interval(Duration::from_secs(1));
        cron_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Skip the first immediate tick
        idle_check.tick().await;
        cron_check.tick().await;

        let mut last_consolidation: Option<chrono::DateTime<Local>> = None;
        // Pause is re-read on every tick; log the transition once, not every tick.
        let mut pause_announced = false;

        // Dreams use a wall-clock check every minute, not the slumber tick: the
        // consolidation window assumed 5-minute ticks and went silent when ticks
        // became hourly (see the test below). One attempt per local day at most, so a
        // failing backend isn't retried every minute for a whole hour.
        let mut dream_check = tokio::time::interval(Duration::from_secs(60));
        dream_check.tick().await;
        let mut last_dream_attempt: Option<chrono::NaiveDate> = None;

        loop {
            tokio::select! {
                // Check idle timeout every 30 seconds
                _ = idle_check.tick() => {
                    // An operator pause stops scheduled slumber. It is re-read
                    // here every tick, so a pause set out-of-band (CLI, HTTP,
                    // another process on the data volume) is honoured without a
                    // restart. The activity timer is deliberately not reset, so
                    // unpausing triggers on the very next tick instead of waiting
                    // out another full idle timeout.
                    if self.engine.refresh_slumber_pause().await {
                        if !pause_announced {
                            tracing::info!("⏸️  Slumber paused — idle and cron triggers skipped");
                            pause_announced = true;
                        }
                        continue;
                    }
                    pause_announced = false;

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

                _ = dream_check.tick() => {
                    let now = Local::now().naive_local();
                    let due = super::dream::dream_due(
                        self.engine.dream_config(),
                        now,
                        self.engine.dream_store().last_date(),
                    );
                    if due && last_dream_attempt != Some(now.date()) {
                        last_dream_attempt = Some(now.date());
                        let engine = self.engine.clone();
                        tokio::spawn(async move {
                            if let Err(e) = engine.dream_now().await {
                                tracing::warn!("🌙 Scheduled dream failed: {}", e);
                            }
                        });
                    }
                }

                // Check cron schedule
                _ = cron_check.tick() => {
                    // Same pause gate as the idle path: a paused engine runs no
                    // scheduled slumber at all.
                    if self.engine.refresh_slumber_pause().await {
                        if !pause_announced {
                            tracing::info!("⏸️  Slumber paused — cron trigger skipped");
                            pause_announced = true;
                        }
                        continue;
                    }
                    pause_announced = false;

                    // Coalesce missed occurrences into one attempt, never replay a backlog.
                    if Local::now() >= next_cron_run {
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
                        next_cron_run = next_cron_after(&schedule, Local::now())?;
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

/// Parse the configured five-field cron in local wall-clock time.
/// The cron crate includes seconds and numbers weekdays 1=Sunday..7=Saturday;
/// our existing Unix-style configuration uses 0/7=Sunday..6=Saturday.
pub(crate) fn parse_cron_schedule(expression: &str) -> anyhow::Result<Schedule> {
    let mut fields: Vec<String> = expression.split_whitespace().map(str::to_owned).collect();
    anyhow::ensure!(fields.len() == 5, "Expected five cron fields: {expression}");
    // Reject zero steps explicitly (including range/list steps).
    for field in &fields {
        for item in field.split(',') {
            if let Some((_, step)) = item.split_once('/') {
                anyhow::ensure!(
                    step.parse::<u32>().is_ok_and(|n| n > 0),
                    "Invalid cron step: {item}"
                );
            }
        }
    }
    // The crate uses 1=Sunday..7=Saturday, while this five-field config uses
    // Unix 0/7=Sunday..6=Saturday. Translating an all-numeric field is safe,
    // and an all-named field needs no translation. Mixing them (MON,3) would
    // silently interpret 3 as Tuesday rather than Unix Wednesday. A bare
    // weekday step (5/2 or MON/2) has no portable endpoint, so require an
    // explicit range (1-5/2) or wildcard (*/2) instead.
    let weekday = &fields[4];
    let has_name = weekday.chars().any(|c| c.is_ascii_alphabetic());
    let has_number = weekday.chars().any(|c| c.is_ascii_digit());
    anyhow::ensure!(
        !has_name || !has_number,
        "Ambiguous cron weekday '{weekday}': do not mix names and numbers; use all names or all Unix numbers"
    );
    anyhow::ensure!(
        !has_name || !weekday.contains('/'),
        "Ambiguous cron weekday '{weekday}': use explicit numeric ranges for steps, or names without steps"
    );
    for item in weekday.split(',') {
        if let Some((base, _)) = item.split_once('/') {
            anyhow::ensure!(
                base == "*" || base.contains('-'),
                "Ambiguous cron weekday step '{item}': use an explicit range or */step"
            );
        }
    }
    // Expand numeric weekdays using the same Unix matching semantics as the
    // existing scheduler, then translate to the crate's one-based weekdays.
    if fields[4]
        .chars()
        .all(|c| c.is_ascii_digit() || "*,-/".contains(c))
    {
        let mut days = Vec::new();
        for item in fields[4].split(',') {
            let (base, step) = match item.split_once('/') {
                Some((base, step)) => (base, step.parse::<u32>()?),
                None => (item, 1),
            };
            let (start, end) = if base == "*" {
                (0, 6)
            } else if let Some((a, b)) = base.split_once('-') {
                (a.parse::<u32>()?, b.parse::<u32>()?)
            } else {
                let day = base.parse::<u32>()?;
                (day, if item.contains('/') { 7 } else { day })
            };
            anyhow::ensure!(start <= end && end <= 7, "Invalid cron weekday: {item}");
            for day in (start..=end).step_by(step as usize) {
                days.push((day % 7 + 1).to_string());
            }
        }
        fields[4] = days.join(",");
    }
    Ok(Schedule::from_str(&format!("0 {}", fields.join(" ")))?)
}

fn next_cron_after<Tz: TimeZone>(
    schedule: &Schedule,
    now: DateTime<Tz>,
) -> anyhow::Result<DateTime<Tz>> {
    schedule
        .after(&now)
        .next()
        .ok_or_else(|| anyhow::anyhow!("Cron schedule has no future occurrence"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<chrono::FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn next(expression: &str, now: &str) -> DateTime<chrono::FixedOffset> {
        next_cron_after(&parse_cron_schedule(expression).unwrap(), at(now)).unwrap()
    }

    #[test]
    fn audit_midnight_is_not_five_minutes() {
        assert_eq!(
            next("0 0 * * *", "2026-11-15T12:17:00-06:00"),
            at("2026-11-16T00:00:00-06:00")
        );
    }

    #[test]
    fn audit_half_past_is_hourly() {
        let due = next("30 * * * *", "2026-11-15T12:17:00-06:00");
        assert_eq!(due, at("2026-11-15T12:30:00-06:00"));
        assert_eq!(
            next_cron_after(&parse_cron_schedule("30 * * * *").unwrap(), due).unwrap(),
            at("2026-11-15T13:30:00-06:00")
        );
    }

    #[test]
    fn audit_zero_step_is_rejected() {
        for expr in [
            "*/0 * * * *",
            "0 */0 * * *",
            "0 0 * * */0",
            "61 * * * *",
            "garbage",
            "",
        ] {
            assert!(parse_cron_schedule(expr).is_err(), "{expr}");
        }
    }

    #[test]
    fn reject_ambiguous_weekday_expressions() {
        for weekday in ["MON,3", "1,FRI", "SUN,0-2", "5/2", "MON/2", "MON-FRI/2"] {
            let expression = format!("0 9 * * {weekday}");
            let error = parse_cron_schedule(&expression)
                .err()
                .expect(&expression)
                .to_string();
            assert!(
                error.contains("weekday") && error.contains(weekday),
                "{expression}: {error}"
            );
        }
        // Explicit ranges and wildcard steps are unambiguous Unix numbering.
        for weekday in ["0,3", "1-5", "1-5/2", "*/2", "MON,FRI"] {
            assert!(
                parse_cron_schedule(&format!("0 9 * * {weekday}")).is_ok(),
                "{weekday}"
            );
        }
        assert_eq!(
            next("0 9 * * 1-5/2", "2026-11-14T12:17:00-06:00"),
            at("2026-11-16T09:00:00-06:00")
        );
    }

    #[test]
    fn audit_calendar_steps_and_weekdays() {
        assert_eq!(
            next("*/15 * * * *", "2026-11-15T12:17:01-06:00"),
            at("2026-11-15T12:30:00-06:00")
        );
        assert_eq!(
            next("0 */4 * * *", "2026-11-15T12:17:00-06:00"),
            at("2026-11-15T16:00:00-06:00")
        );
        assert_eq!(
            next("0 0 1 * *", "2026-11-15T12:17:00-06:00"),
            at("2026-12-01T00:00:00-06:00")
        );
        for sunday in ["0", "7", "SUN"] {
            assert_eq!(
                next(&format!("0 0 * * {sunday}"), "2026-11-14T12:17:00-06:00"),
                at("2026-11-15T00:00:00-06:00")
            );
        }
        assert_eq!(
            next("0 9 * * 1-5", "2026-11-14T12:17:00-06:00"),
            at("2026-11-16T09:00:00-06:00")
        );
        assert!(next_cron_after(
            &parse_cron_schedule("0 0 30 2 *").unwrap(),
            at("2026-11-15T00:00:00Z")
        )
        .is_err());
    }

    /// Does any tick of a fixed-interval timer started at `start_min` (minutes
    /// after midnight) land inside `target_min ± tolerance` during one day?
    /// Mirrors the scheduler: `tokio::time::interval` ticks are offset from
    /// process start, and `should_run_at_schedule` allows ±CRON_TOLERANCE_MINUTES.
    fn tick_hits_window(
        start_min: u32,
        interval_min: u32,
        target_min: u32,
        tolerance: u32,
    ) -> bool {
        (0..(24 * 60) / interval_min).any(|i| {
            let t = (start_min + i * interval_min) % (24 * 60);
            t.abs_diff(target_min) <= tolerance
        })
    }

    /// Regression pin for the silent consolidation (no summaries after Oct 30
    /// 2026): with 5-minute ticks every start time hits 03:00 ± 5, but with the
    /// hourly `cron_ingest = "0 * * * *"` only start minutes within ±5 of the
    /// hour do (11 of 60). Most restarts therefore disabled consolidation.
    #[test]
    fn hourly_ticks_usually_miss_the_consolidation_window() {
        let target = 3 * 60;
        let tol = super::CRON_TOLERANCE_MINUTES as u32;
        assert!((0..60).all(|m| tick_hits_window(m, 5, target, tol)));
        let hits = (0..60)
            .filter(|m| tick_hits_window(*m, 60, target, tol))
            .count();
        assert_eq!(hits, 11);
    }
}
