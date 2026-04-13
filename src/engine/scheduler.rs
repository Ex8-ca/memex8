use crate::config::AppConfig;
use crate::engine::Engine;
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

                        match self.engine.trigger_slumber().await {
                            Ok(report) => {
                                tracing::info!(
                                    "💤 Idle slumber complete: dedup={} quant={} prune={} md={}",
                                    report.deduplicated,
                                    report.quantized,
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
                        match self.engine.trigger_slumber().await {
                            Ok(report) => {
                                tracing::info!(
                                    "🕐 Cron slumber complete: dedup={} quant={} prune={} md={}",
                                    report.deduplicated,
                                    report.quantized,
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
    let num: u64 = num_str.parse().map_err(|_| anyhow::anyhow!("Invalid duration: {}", s))?;

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
            let minutes: u64 = star_n[..space_idx].parse().map_err(|_| {
                anyhow::anyhow!("Invalid cron expression: {}", cron_expr)
            })?;
            return Ok(Duration::from_secs(minutes * 60));
        }
    }

    // Default: every 5 minutes
    tracing::warn!(
        "Complex cron expression '{}' not fully parsed, defaulting to 5 minutes",
        cron_expr
    );
    Ok(Duration::from_secs(300))
}
