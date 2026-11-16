use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Maximum directory nesting below the requested root. Bounds a pathological
/// tree even when no symlink is involved.
const MAX_DIR_DEPTH: usize = 32;
/// Maximum number of candidate (markdown) files considered per request.
const MAX_CANDIDATE_FILES: usize = 10_000;
/// Maximum number of directory entries visited per request, so ignored files
/// cannot consume unbounded work either.
const MAX_WALK_ENTRIES: usize = 50_000;
/// Upper bound on the detailed issue list, so one bad tree cannot produce a
/// multi-megabyte response. `IngestReport::issues_truncated` records the cut.
const MAX_ISSUES: usize = 200;
/// Directories that never hold ingestible content.
const SKIPPED_DIR_NAMES: [&str; 2] = ["node_modules", "target"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawChunk {
    pub content: String,
    pub heading: Option<String>,
    pub source_file: String,
    pub source_hash: String,
    pub parent_context: Option<String>,
    pub chunk_type: String,
}

/// Whether a per-path detail describes benign skipping or a real failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueOutcome {
    Skipped,
    Failed,
}

impl IssueOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Stable, machine-readable reason for a skip or failure. These are the only
/// per-path detail exposed through the API: underlying OS/service error text is
/// logged but never returned to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueReason {
    /// File contents were not valid UTF-8.
    InvalidUtf8,
    /// OS-level read failure (permissions, I/O error, vanished entry).
    Unreadable,
    /// Entry is a symbolic link; traversal never follows links.
    Symlink,
    /// Directory nested deeper than `MAX_DIR_DEPTH`.
    DepthLimit,
    /// Candidate file budget exhausted.
    FileLimit,
    /// Directory-entry budget exhausted.
    EntryLimit,
    /// Markdown parsing failed.
    ChunkFailed,
    /// Nothing storable survived redaction/chunking.
    EmptyContent,
    /// Persisting the chunk failed (store or realm lookup).
    StoreFailed,
}

impl IssueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "invalid_utf8",
            Self::Unreadable => "unreadable",
            Self::Symlink => "symlink",
            Self::DepthLimit => "depth_limit",
            Self::FileLimit => "file_limit",
            Self::EntryLimit => "entry_limit",
            Self::ChunkFailed => "chunk_failed",
            Self::EmptyContent => "empty_content",
            Self::StoreFailed => "store_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestIssue {
    /// Path relative to the requested root where possible (no server layout leak).
    pub path: String,
    pub outcome: IssueOutcome,
    pub reason: IssueReason,
}

/// Truthful outcome of one ingestion request.
///
/// Counts are per *file* except `chunks`/`chunks_stored`:
/// - `attempted`: candidate markdown files considered (the single file for a
///   file request).
/// - `stored`: files that landed at least one chunk in the store.
/// - `skipped`: candidates with nothing storable, plus refused symlinks and
///   bounded-out entries.
/// - `failed`: files or subtrees that could not be read/parsed/stored.
/// - `ignored`: entries that are not ingestible content (non-markdown,
///   hidden/`node_modules`/`target` dirs, sockets/devices).
///
/// Ingester-level construction sets `stored` optimistically for files that
/// produced chunks; `finalize_store` restates it from actual persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub attempted: usize,
    pub stored: usize,
    pub skipped: usize,
    pub failed: usize,
    pub ignored: usize,
    pub chunks: usize,
    pub chunks_stored: usize,
    pub issues: Vec<IngestIssue>,
    pub issues_truncated: bool,
    /// Failures found before any store attempt; `finalize_store` adds
    /// store-only failures on top.
    pub(crate) base_failures: usize,
    /// Root used to render issue paths relative to the request.
    pub(crate) root: PathBuf,
    pub(crate) files_stored: BTreeSet<String>,
    pub(crate) files_store_failed: BTreeSet<String>,
}

impl Default for IngestReport {
    fn default() -> Self {
        Self {
            attempted: 0,
            stored: 0,
            skipped: 0,
            failed: 0,
            ignored: 0,
            chunks: 0,
            chunks_stored: 0,
            issues: Vec::new(),
            issues_truncated: false,
            base_failures: 0,
            root: PathBuf::new(),
            files_stored: BTreeSet::new(),
            files_store_failed: BTreeSet::new(),
        }
    }
}

impl IngestReport {
    /// Overall outcome: `"ingested"` (all candidates stored, none failed),
    /// `"partial"` (some stored, some failed), `"failed"` (nothing stored and
    /// something failed), `"empty"` (nothing to ingest, nothing failed).
    pub fn status(&self) -> &'static str {
        if self.failed > 0 {
            if self.stored > 0 {
                "partial"
            } else {
                "failed"
            }
        } else if self.stored > 0 {
            "ingested"
        } else {
            "empty"
        }
    }

    pub fn is_partial(&self) -> bool {
        self.failed > 0 && self.stored > 0
    }

    /// Record that one chunk from `source_file` was persisted.
    pub fn note_chunk_stored(&mut self, source_file: &str) {
        self.chunks_stored += 1;
        self.files_stored.insert(source_file.to_string());
    }

    /// Record that one chunk from `source_file` failed to persist, with a
    /// sanitized issue carrying only the relative path and a stable code.
    pub fn note_chunk_failed(&mut self, source_file: &str) {
        self.files_store_failed.insert(source_file.to_string());
        let rel = self.display(source_file);
        self.push_issue(IngestIssue {
            path: rel,
            outcome: IssueOutcome::Failed,
            reason: IssueReason::StoreFailed,
        });
    }

    /// Restate stored/failed from actual persistence. Safe to call once, after
    /// the store loop; a no-op when nothing was attempted against the store.
    pub fn finalize_store(&mut self) {
        if self.files_stored.is_empty() && self.files_store_failed.is_empty() {
            return;
        }
        self.stored = self.files_stored.len();
        let store_only_failures = self
            .files_store_failed
            .iter()
            .filter(|f| !self.files_stored.contains(*f))
            .count();
        self.failed = self.base_failures + store_only_failures;
    }

    /// Append a detail entry, enforcing the response-size cap.
    pub fn push_issue(&mut self, issue: IngestIssue) {
        if self.issues.len() >= MAX_ISSUES {
            self.issues_truncated = true;
            return;
        }
        self.issues.push(issue);
    }

    /// Render an absolute path relative to the request root.
    fn display(&self, path: &str) -> String {
        display_path(&self.root, Path::new(path))
    }
}

#[derive(Debug)]
pub struct IngestOutcome {
    pub chunks: Vec<RawChunk>,
    pub report: IngestReport,
}

pub struct Ingester {
    config: AppConfig,
}

/// Result of reading one candidate file.
enum Candidate {
    Chunks(Vec<RawChunk>),
    Empty,
    Failed(IssueReason),
}

impl Ingester {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn ingest_path(&self, path: &str, chunk_by: &str) -> anyhow::Result<IngestOutcome> {
        let p = Path::new(path);
        // `is_file`/`is_dir` follow a symlink for the *requested* root: an
        // operator may point at one deliberately. Links found during traversal
        // are never followed.
        if p.is_file() {
            Ok(self.ingest_single_file(p, chunk_by).await)
        } else if p.is_dir() {
            Ok(self.ingest_directory(p, chunk_by).await)
        } else {
            anyhow::bail!("Path not found: {}", path)
        }
    }

    async fn ingest_single_file(&self, path: &Path, chunk_by: &str) -> IngestOutcome {
        let root = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let mut report = IngestReport {
            attempted: 1,
            root,
            ..Default::default()
        };
        let mut chunks = Vec::new();

        match self.read_candidate(path, chunk_by) {
            Candidate::Chunks(found) => {
                report.stored = 1;
                report.chunks = found.len();
                chunks = found;
            }
            Candidate::Empty => {
                report.skipped = 1;
                report.push_issue(IngestIssue {
                    path: display_path(&report.root, path),
                    outcome: IssueOutcome::Skipped,
                    reason: IssueReason::EmptyContent,
                });
            }
            Candidate::Failed(reason) => {
                report.failed = 1;
                report.base_failures = 1;
                report.push_issue(IngestIssue {
                    path: display_path(&report.root, path),
                    outcome: IssueOutcome::Failed,
                    reason,
                });
            }
        }

        IngestOutcome { chunks, report }
    }

    async fn ingest_directory(&self, dir: &Path, chunk_by: &str) -> IngestOutcome {
        let walked = collect_candidates(dir);
        let mut report = IngestReport {
            skipped: walked.skipped,
            failed: walked.failed,
            ignored: walked.ignored,
            issues: walked.issues,
            issues_truncated: walked.issues_truncated,
            base_failures: walked.failed,
            root: dir.to_path_buf(),
            ..Default::default()
        };
        let mut all_chunks = Vec::new();

        for file in walked.files {
            report.attempted += 1;
            match self.read_candidate(&file, chunk_by) {
                Candidate::Chunks(chunks) => {
                    report.stored += 1;
                    report.chunks += chunks.len();
                    all_chunks.extend(chunks);
                }
                Candidate::Empty => {
                    report.skipped += 1;
                    report.push_issue(IngestIssue {
                        path: display_path(dir, &file),
                        outcome: IssueOutcome::Skipped,
                        reason: IssueReason::EmptyContent,
                    });
                }
                Candidate::Failed(reason) => {
                    report.failed += 1;
                    report.base_failures += 1;
                    report.push_issue(IngestIssue {
                        path: display_path(dir, &file),
                        outcome: IssueOutcome::Failed,
                        reason,
                    });
                }
            }
        }

        IngestOutcome {
            chunks: all_chunks,
            report,
        }
    }

    fn read_candidate(&self, path: &Path, chunk_by: &str) -> Candidate {
        // Read bytes first so invalid UTF-8 is classified precisely instead of
        // being folded into a generic IO error.
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("Cannot read {:?}: {}", path, e);
                return Candidate::Failed(IssueReason::Unreadable);
            }
        };
        let raw = match String::from_utf8(bytes) {
            Ok(raw) => raw,
            Err(_) => {
                tracing::warn!("Skipping non-UTF-8 file: {:?}", path);
                return Candidate::Failed(IssueReason::InvalidUtf8);
            }
        };

        let content = crate::engine::redaction::text(&raw);
        if !crate::engine::redaction::meaningful(&content) {
            tracing::info!("Skipped file: content empty after redaction");
            return Candidate::Empty;
        }
        let source_hash = Self::hash_content(&content);
        let source_file = path.to_string_lossy().to_string();

        let chunks = match crate::engine::chunker::chunk(
            &content,
            chunk_by,
            self.config.ingest.max_chunk_tokens,
        ) {
            Ok(chunks) => chunks,
            Err(e) => {
                tracing::warn!("Failed to chunk {:?}: {}", path, e);
                return Candidate::Failed(IssueReason::ChunkFailed);
            }
        };

        let mapped: Vec<RawChunk> = chunks
            .into_iter()
            .filter(|c| crate::engine::redaction::meaningful(&c.content))
            .map(|c| RawChunk {
                content: c.content,
                heading: c.heading,
                source_file: source_file.clone(),
                source_hash: source_hash.clone(),
                parent_context: c.parent_heading,
                chunk_type: chunk_by.to_string(),
            })
            .collect();

        if mapped.is_empty() {
            return Candidate::Empty;
        }
        Candidate::Chunks(mapped)
    }

    pub async fn start_watching(&self, path: &str) -> anyhow::Result<()> {
        // TODO: use notify crate for real-time file watching
        tracing::info!("File watching not yet implemented for: {}", path);
        Ok(())
    }

    fn hash_content(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Bounded, symlink-refusing directory walk.
struct Walk {
    files: Vec<PathBuf>,
    issues: Vec<IngestIssue>,
    skipped: usize,
    failed: usize,
    ignored: usize,
    issues_truncated: bool,
}

impl Walk {
    fn push(&mut self, root: &Path, path: &Path, outcome: IssueOutcome, reason: IssueReason) {
        match outcome {
            IssueOutcome::Skipped => self.skipped += 1,
            IssueOutcome::Failed => self.failed += 1,
        }
        if self.issues.len() >= MAX_ISSUES {
            self.issues_truncated = true;
            return;
        }
        self.issues.push(IngestIssue {
            path: display_path(root, path),
            outcome,
            reason,
        });
    }
}

fn collect_candidates(dir: &Path) -> Walk {
    let mut walk = Walk {
        files: Vec::new(),
        issues: Vec::new(),
        skipped: 0,
        failed: 0,
        ignored: 0,
        issues_truncated: false,
    };
    let mut visited = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];

    'walk: while let Some((current, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Cannot read directory {:?}: {}", current, e);
                walk.push(dir, &current, IssueOutcome::Failed, IssueReason::Unreadable);
                continue;
            }
        };

        for entry in entries {
            if visited >= MAX_WALK_ENTRIES {
                walk.push(dir, &current, IssueOutcome::Failed, IssueReason::EntryLimit);
                break 'walk;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    tracing::warn!("Cannot read entry in {:?}: {}", current, e);
                    walk.push(dir, &current, IssueOutcome::Failed, IssueReason::Unreadable);
                    continue;
                }
            };
            visited += 1;
            let path = entry.path();

            // `DirEntry::file_type` never follows symlinks, so a link is seen
            // as a link: loops and escapes outside the root are impossible.
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(e) => {
                    tracing::warn!("Cannot stat {:?}: {}", path, e);
                    walk.push(dir, &path, IssueOutcome::Failed, IssueReason::Unreadable);
                    continue;
                }
            };

            if file_type.is_symlink() {
                walk.push(dir, &path, IssueOutcome::Skipped, IssueReason::Symlink);
                continue;
            }

            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || SKIPPED_DIR_NAMES.contains(&name.as_ref()) {
                    walk.ignored += 1;
                    continue;
                }
                if depth + 1 > MAX_DIR_DEPTH {
                    walk.push(dir, &path, IssueOutcome::Failed, IssueReason::DepthLimit);
                    continue;
                }
                stack.push((path, depth + 1));
                continue;
            }

            if !file_type.is_file() {
                // sockets, fifos, device nodes
                walk.ignored += 1;
                continue;
            }

            let is_markdown = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            if !is_markdown {
                walk.ignored += 1;
                continue;
            }
            if walk.files.len() >= MAX_CANDIDATE_FILES {
                walk.push(dir, &path, IssueOutcome::Failed, IssueReason::FileLimit);
                break 'walk;
            }
            walk.files.push(path);
        }
    }

    // Deterministic order keeps issue lists and ingestion stable across runs.
    walk.files.sort();
    walk
}

/// Render `path` relative to `root`, falling back to the file name when the
/// path is outside the root (or is the root itself).
fn display_path(root: &Path, path: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(root) {
        if !rel.as_os_str().is_empty() {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn ingester() -> Ingester {
        Ingester::new(AppConfig::default())
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    async fn ingest_dir(dir: &Path) -> IngestOutcome {
        ingester()
            .ingest_path(dir.to_str().unwrap(), "section")
            .await
            .unwrap()
    }

    fn issue_for<'a>(report: &'a IngestReport, path: &str) -> Option<&'a IngestIssue> {
        report.issues.iter().find(|i| i.path == path)
    }

    #[tokio::test]
    async fn directory_counts_stored_skipped_and_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("good.md"),
            b"# Title\n\nThis body has enough words to survive redaction.\n",
        );
        write_file(&dir.path().join("empty.md"), b"   \n\n");
        write_file(&dir.path().join("notes.txt"), b"not markdown\n");

        let outcome = ingest_dir(dir.path()).await;
        let report = &outcome.report;

        assert_eq!(report.attempted, 2, "only .md candidates are attempted");
        assert_eq!(report.stored, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.ignored, 1);
        assert!(report.chunks >= 1);
        assert_eq!(report.status(), "ingested");
        assert!(outcome
            .chunks
            .iter()
            .all(|c| c.source_file.ends_with("good.md")));
        let empty = issue_for(report, "empty.md").expect("empty file must be reported");
        assert_eq!(empty.outcome, IssueOutcome::Skipped);
        assert_eq!(empty.reason, IssueReason::EmptyContent);
    }

    #[tokio::test]
    async fn invalid_utf8_file_fails_without_hiding_siblings() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("good.md"),
            b"# Title\n\nReadable body text for the good file.\n",
        );
        write_file(&dir.path().join("bad.md"), &[b'#', b' ', 0xff, 0xfe, 0x00]);

        let outcome = ingest_dir(dir.path()).await;
        let report = &outcome.report;

        assert_eq!(report.attempted, 2);
        assert_eq!(report.stored, 1, "the readable sibling must still ingest");
        assert_eq!(report.failed, 1);
        assert_eq!(report.status(), "partial");
        assert!(report.is_partial());

        let bad = issue_for(report, "bad.md").expect("invalid UTF-8 must be reported");
        assert_eq!(bad.outcome, IssueOutcome::Failed);
        assert_eq!(bad.reason, IssueReason::InvalidUtf8);
        assert_eq!(bad.reason.as_str(), "invalid_utf8");
        assert!(outcome
            .chunks
            .iter()
            .all(|c| !c.source_file.ends_with("bad.md")));
    }

    #[tokio::test]
    async fn unreadable_subtree_is_reported_and_siblings_ingest() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("good.md"),
            b"# Title\n\nReadable body text for the good file.\n",
        );
        let locked = dir.path().join("locked");
        write_file(&locked.join("hidden.md"), b"# Hidden\n\nSecret body.\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Running as root defeats mode bits; skip rather than assert a lie.
            if std::fs::read_dir(&locked).is_ok() {
                eprintln!("skipping unreadable-subtree assertions: running with read bypass");
                std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
                return;
            }

            let outcome = ingest_dir(dir.path()).await;
            let report = &outcome.report;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

            assert_eq!(report.stored, 1);
            assert_eq!(report.failed, 1);
            assert_eq!(report.status(), "partial");
            let issue = issue_for(report, "locked").expect("unreadable subtree must be reported");
            assert_eq!(issue.outcome, IssueOutcome::Failed);
            assert_eq!(issue.reason, IssueReason::Unreadable);
        }
        #[cfg(not(unix))]
        {
            let _ = ingest_dir(dir.path()).await;
        }
    }

    #[tokio::test]
    async fn symlinked_file_outside_root_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        write_file(
            &outside.path().join("secret.md"),
            b"# Secret\n\nContent outside the requested root.\n",
        );
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("good.md"),
            b"# Title\n\nReadable body text for the good file.\n",
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.md"),
                dir.path().join("link.md"),
            )
            .unwrap();

            let outcome = ingest_dir(dir.path()).await;
            let report = &outcome.report;

            assert_eq!(report.attempted, 1, "symlink must not become a candidate");
            assert_eq!(report.stored, 1);
            assert_eq!(report.failed, 0);
            assert_eq!(report.status(), "ingested");
            let issue = issue_for(report, "link.md").expect("symlink must be reported");
            assert_eq!(issue.outcome, IssueOutcome::Skipped);
            assert_eq!(issue.reason, IssueReason::Symlink);
            assert!(
                outcome
                    .chunks
                    .iter()
                    .all(|c| !c.content.contains("outside the requested root")),
                "content behind a symlink must never be ingested"
            );
        }
    }

    #[tokio::test]
    async fn symlink_loop_terminates_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("sub/note.md"),
            b"# Note\n\nBody text here.\n",
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/loop")).unwrap();

            let outcome =
                tokio::time::timeout(std::time::Duration::from_secs(10), ingest_dir(dir.path()))
                    .await
                    .expect("traversal must not loop");
            let report = &outcome.report;

            assert_eq!(report.attempted, 1);
            assert_eq!(report.stored, 1);
            assert_eq!(
                outcome.chunks.len(),
                1,
                "the loop must not duplicate content"
            );
            let issue = issue_for(report, "sub/loop").expect("loop link must be reported");
            assert_eq!(issue.reason, IssueReason::Symlink);
        }
    }

    #[tokio::test]
    async fn depth_limit_is_reported_as_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().to_path_buf();
        for i in 0..(MAX_DIR_DEPTH + 3) {
            deep = deep.join(format!("d{i}"));
        }
        write_file(&deep.join("deep.md"), b"# Deep\n\nBody text.\n");

        let outcome = ingest_dir(dir.path()).await;
        let report = &outcome.report;

        assert_eq!(report.stored, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.status(), "failed");
        assert!(report
            .issues
            .iter()
            .any(|i| i.reason == IssueReason::DepthLimit));
    }

    #[tokio::test]
    async fn hidden_and_build_dirs_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("good.md"), b"# Title\n\nBody text.\n");
        write_file(&dir.path().join(".hidden/h.md"), b"# Hidden\n\nBody.\n");
        write_file(&dir.path().join("target/t.md"), b"# Build\n\nBody.\n");
        write_file(
            &dir.path().join("node_modules/n.md"),
            b"# Modules\n\nBody.\n",
        );

        let outcome = ingest_dir(dir.path()).await;
        assert_eq!(outcome.report.attempted, 1);
        assert_eq!(outcome.report.stored, 1);
        assert_eq!(outcome.report.ignored, 3);
    }

    #[tokio::test]
    async fn empty_directory_reports_empty_not_ingested() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = ingest_dir(dir.path()).await;

        assert_eq!(outcome.report.attempted, 0);
        assert_eq!(outcome.report.stored, 0);
        assert_eq!(outcome.report.failed, 0);
        assert_eq!(outcome.report.status(), "empty");
    }

    #[tokio::test]
    async fn single_file_is_one_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("one.md");
        write_file(&file, b"# Title\n\nBody text long enough to chunk.\n");

        let outcome = ingester()
            .ingest_path(file.to_str().unwrap(), "section")
            .await
            .unwrap();
        assert_eq!(outcome.report.attempted, 1);
        assert_eq!(outcome.report.stored, 1);
        assert_eq!(outcome.report.ignored, 0);
        assert_eq!(outcome.report.status(), "ingested");
    }

    #[tokio::test]
    async fn missing_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let err = ingester()
            .ingest_path(missing.to_str().unwrap(), "section")
            .await
            .expect_err("missing path must error");
        assert!(err.to_string().contains("Path not found"));
    }

    #[test]
    fn finalize_store_restates_failed_files() {
        let mut report = IngestReport {
            attempted: 2,
            stored: 2,
            root: PathBuf::from("/root"),
            ..Default::default()
        };
        report.note_chunk_stored("/root/a.md");
        report.note_chunk_failed("/root/b.md");
        report.finalize_store();

        assert_eq!(report.stored, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.chunks_stored, 1);
        assert_eq!(report.status(), "partial");
        assert!(report
            .issues
            .iter()
            .any(|i| i.path == "b.md" && i.reason == IssueReason::StoreFailed));
    }

    #[test]
    fn issue_list_is_capped() {
        let mut report = IngestReport::default();
        for i in 0..(MAX_ISSUES + 5) {
            report.push_issue(IngestIssue {
                path: format!("f{i}.md"),
                outcome: IssueOutcome::Failed,
                reason: IssueReason::Unreadable,
            });
        }
        assert_eq!(report.issues.len(), MAX_ISSUES);
        assert!(report.issues_truncated);
    }
}
