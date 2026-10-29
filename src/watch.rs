//! Foreground change monitor. Native events are hints that trigger a rescan of the
//! affected root; periodic full reconciliation (with full rehashing through
//! `Engine::scan`) is the source of truth. The watcher only observes: it never
//! approves or quarantines, and it detects changes after they are written.
use crate::{cli::Format, config, engine::Engine, model::*, paths};
use anyhow::{Context, Result};
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub const EVENT_SCHEMA_VERSION: u32 = 1;
pub const HEALTH_KEY: &str = "monitor_health";
pub const HEARTBEAT_SECS: u64 = 15;
const CURSOR_KEY: &str = "watch_cursor";
const CHANNEL_CAPACITY: usize = 1024;
const TICK: Duration = Duration::from_millis(200);
const MAX_CHANGES_PER_EVENT: usize = 500;
const MAX_ERROR_CHARS: usize = 512;
/// Service log (macOS LaunchAgent stdout/stderr) is truncated past this size.
pub const MAX_SERVICE_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchEventKind {
    Started,
    Reconciled,
    Degraded,
    Stopped,
}

/// One JSONL record. Paths are escaped native paths; file contents are never included.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchEvent {
    pub schema_version: u32,
    pub request_id: String,
    /// Persistent, strictly increasing cursor (shared with the audit event log).
    pub sequence: u64,
    pub created_at: u64,
    pub kind: WatchEventKind,
    pub operation: String,
    /// `completed` only when every reconciled root had complete coverage.
    pub completion: String,
    /// What caused this record: startup, native_event, overflow, periodic, poll_fallback, root_replaced, cancel.
    pub trigger: String,
    pub coverage: Option<Coverage>,
    pub profile_version: String,
    pub policy_version: String,
    /// Artifact observations recorded since the previous emitted cursor, deduplicated by artifact and digest.
    pub changes: Vec<ArtifactChange>,
    pub changes_truncated: bool,
    pub recovery: Vec<ItemOutcome>,
    pub health: MonitorHealth,
    pub exit_code: Option<i32>,
    pub errors: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactChange {
    pub sequence: u64,
    pub kind: String,
    pub artifact_id: Option<String>,
    pub observed_sha256: Option<String>,
    pub path: Option<NativePath>,
    pub category: Option<String>,
    pub state: Option<ApprovalState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RootFallback {
    pub root: NativePath,
    pub reason: String,
}

/// Persisted in the `monitor_health` setting and refreshed on every heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct MonitorHealth {
    pub pid: u32,
    pub version: String,
    pub executable: Option<NativePath>,
    /// starting, reconciling, watching, degraded, stopped
    pub status: String,
    pub started_at: u64,
    pub heartbeat_at: u64,
    pub heartbeat_interval_secs: u64,
    pub watched_roots: Vec<NativePath>,
    pub polling_fallbacks: Vec<RootFallback>,
    pub last_complete_reconcile: Option<u64>,
    pub last_reconcile: Option<u64>,
    pub last_error: Option<String>,
    /// Roots waiting for a debounced rescan (all roots after an overflow).
    pub queue: u64,
    pub pending_findings: u64,
    pub event_overflows: u64,
}

struct Root {
    requested: PathBuf,
    /// Canonical path used to match native event paths.
    path: PathBuf,
    identity: Option<(u64, u64)>,
    native: bool,
    coverage_complete: bool,
    first_dirty: Option<Instant>,
    last_dirty: Option<Instant>,
    last_poll: Instant,
}

pub fn run(
    engine: &Engine,
    roots: &[PathBuf],
    cancel: Arc<AtomicBool>,
    once: bool,
    format: Format,
) -> Result<i32> {
    anyhow::ensure!(!roots.is_empty(), "watch requires at least one root");
    // Single monitor per state directory; separate from the coordinator lock that scans take.
    let _monitor = paths::lock(&engine.locations.state.join("monitor.lock"))
        .context("another RuleScan watcher is already running for this state directory")?;
    let w = &engine.config.watch;
    let debounce = config::duration(&w.debounce)?;
    let max_delay = debounce.saturating_mul(10).max(Duration::from_secs(5));
    let reconcile_every = config::duration(&w.reconcile_every)?;
    let poll_every = config::duration(&w.poll_every)?;
    let mut out = Output {
        format,
        store: &engine.store,
        last: None,
        last_health_signature: None,
    };

    let mut health = MonitorHealth {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
        executable: std::env::current_exe().ok().map(|p| NativePath::new(&p)),
        status: "starting".into(),
        started_at: now(),
        heartbeat_at: now(),
        heartbeat_interval_secs: HEARTBEAT_SECS,
        ..Default::default()
    };
    let mut roots: Vec<Root> = roots
        .iter()
        .map(|r| {
            let requested = config::expand(r)?;
            let path = std::fs::canonicalize(&requested).unwrap_or_else(|_| requested.clone());
            Ok(Root {
                identity: identity(&path),
                requested,
                path,
                native: false,
                coverage_complete: false,
                first_dirty: None,
                last_dirty: None,
                last_poll: Instant::now(),
            })
        })
        .collect::<Result<_>>()?;
    health.watched_roots = roots.iter().map(|r| NativePath::new(&r.path)).collect();
    save_health(engine, &mut health);

    // Explicit recovery of interrupted user-initiated operations before claiming coverage.
    let recovery = match engine.recover() {
        Ok(r) => r,
        Err(e) => {
            health.last_error = Some(bounded(&format!("{e:#}")));
            vec![]
        }
    };

    let (tx, rx) = mpsc::sync_channel::<notify::Result<Event>>(CHANNEL_CAPACITY);
    let overflow = Arc::new(AtomicBool::new(false));
    let mut watcher = if once {
        None
    } else {
        let locations = engine.locations.clone();
        let overflow = overflow.clone();
        let handler = move |res: notify::Result<Event>| {
            // Drop events for RuleScan's own state/data/cache so our writes never trigger rescans.
            if let Ok(e) = &res
                && !e.need_rescan()
                && !e.paths.is_empty()
                && e.paths.iter().all(|p| locations.excluded(p))
            {
                return;
            }
            if let Err(mpsc::TrySendError::Full(_)) = tx.try_send(res) {
                overflow.store(true, Ordering::Relaxed);
            }
        };
        match RecommendedWatcher::new(handler, NotifyConfig::default().with_follow_symlinks(false))
        {
            Ok(w) => Some(w),
            Err(e) => {
                health.last_error = Some(bounded(&format!("native watcher unavailable: {e}")));
                None
            }
        }
    };
    if let Some(w) = watcher.as_mut() {
        for r in &mut roots {
            attach(w, r, &mut health);
        }
    } else if !once {
        for r in &roots {
            set_fallback(
                &mut health,
                r,
                "native watcher unavailable; periodic rescans",
            );
        }
    }

    // Startup: full reconciliation before declaring coverage current.
    let (result, mut errors) = reconcile(engine, &mut roots, &cancel, &mut health, None);
    let exit = result.as_ref().map(ScanResult::exit_code);
    let started_kind = if once {
        WatchEventKind::Reconciled
    } else {
        WatchEventKind::Started
    };
    out.emit(
        engine,
        started_kind,
        "startup",
        result.as_ref(),
        recovery,
        &mut health,
        exit.filter(|_| once),
        std::mem::take(&mut errors),
    )?;
    if once {
        health.status = "stopped".into();
        save_health(engine, &mut health);
        return match exit {
            Some(code) => Ok(code),
            None => Ok(5),
        };
    }

    let mut last_full = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut pending_trigger = "native_event";
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(TICK) {
            Ok(first) => {
                let mut batch = vec![first];
                while let Ok(e) = rx.try_recv() {
                    batch.push(e);
                    if batch.len() >= CHANNEL_CAPACITY {
                        break;
                    }
                }
                for item in batch {
                    match item {
                        Ok(event) if event.need_rescan() => {
                            mark_all(&mut roots);
                            pending_trigger = "overflow";
                        }
                        Ok(event) => {
                            for p in &event.paths {
                                mark(&mut roots, p);
                            }
                        }
                        Err(e) => {
                            health.last_error =
                                Some(bounded(&format!("native watcher error: {e}")));
                            if e.paths.is_empty() {
                                mark_all(&mut roots);
                            } else {
                                for p in &e.paths {
                                    mark(&mut roots, p);
                                }
                            }
                            {
                                for r in roots.iter_mut().filter(|r| {
                                    e.paths.is_empty()
                                        || e.paths.iter().any(|p| affects(&r.path, p))
                                }) {
                                    r.native = false;
                                    set_fallback(
                                        &mut health,
                                        r,
                                        "native watcher error; periodic rescans until reattached",
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if roots.iter().any(|r| r.native) {
                    health.last_error = Some("native watcher stopped; periodic rescans".into());
                    for r in &mut roots {
                        r.native = false;
                        set_fallback(&mut health, r, "native watcher stopped; periodic rescans");
                    }
                }
                std::thread::sleep(TICK);
            }
        }
        if overflow.swap(false, Ordering::Relaxed) {
            health.event_overflows += 1;
            mark_all(&mut roots);
            pending_trigger = "overflow";
        }
        for r in roots
            .iter_mut()
            .filter(|r| !r.native && r.last_poll.elapsed() >= poll_every)
        {
            r.last_poll = Instant::now();
            touch(r);
            pending_trigger = "poll_fallback";
        }
        health.queue = roots.iter().filter(|r| r.first_dirty.is_some()).count() as u64;

        let full_due = last_full.elapsed() >= reconcile_every;
        let due: Vec<usize> = if full_due {
            (0..roots.len()).collect()
        } else {
            roots
                .iter()
                .enumerate()
                .filter(|(_, r)| match (r.first_dirty, r.last_dirty) {
                    (Some(first), Some(last)) => {
                        last.elapsed() >= debounce || first.elapsed() >= max_delay
                    }
                    _ => false,
                })
                .map(|(i, _)| i)
                .collect()
        };
        if !due.is_empty() && !cancel.load(Ordering::Relaxed) {
            let trigger = if full_due {
                last_full = Instant::now();
                "periodic"
            } else {
                pending_trigger
            };
            let (result, errs) = reconcile(engine, &mut roots, &cancel, &mut health, Some(&due));
            if let Some(w) = watcher.as_mut() {
                reattach_replaced(w, &mut roots, &due, &mut health);
            }
            let kind = if result.is_none() || !health.polling_fallbacks.is_empty() {
                WatchEventKind::Degraded
            } else {
                WatchEventKind::Reconciled
            };
            out.emit(
                engine,
                kind,
                trigger,
                result.as_ref(),
                vec![],
                &mut health,
                None,
                errs,
            )?;
            pending_trigger = "native_event";
            last_heartbeat = Instant::now();
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_SECS) {
            last_heartbeat = Instant::now();
            save_health(engine, &mut health);
            crate::service::trim_log(&engine.locations);
        }
    }
    drop(watcher);
    health.status = "stopped".into();
    out.emit(
        engine,
        WatchEventKind::Stopped,
        "cancel",
        None,
        vec![],
        &mut health,
        Some(0),
        vec![],
    )?;
    Ok(0)
}

fn attach(w: &mut RecommendedWatcher, r: &mut Root, health: &mut MonitorHealth) {
    match w.watch(&r.path, RecursiveMode::Recursive) {
        Ok(()) => {
            r.native = true;
            health
                .polling_fallbacks
                .retain(|f| f.root != NativePath::new(&r.path));
        }
        Err(e) => {
            r.native = false;
            set_fallback(
                health,
                r,
                &format!(
                    "native watch failed ({}); periodic rescans",
                    bounded(&e.to_string())
                ),
            );
        }
    }
    // Parent watch catches deletion, rename, or replacement of the root itself.
    if let Some(parent) = r.path.parent() {
        let _ = w.watch(parent, RecursiveMode::NonRecursive);
    }
}

/// After a rescan, a root whose directory identity changed (replaced or recreated)
/// needs a fresh native watch; inotify watches follow the old inode.
fn reattach_replaced(
    w: &mut RecommendedWatcher,
    roots: &mut [Root],
    due: &[usize],
    health: &mut MonitorHealth,
) {
    for &i in due {
        let r = &mut roots[i];
        let current = identity(&r.path);
        if current != r.identity {
            let _ = w.unwatch(&r.path);
            r.identity = current;
            if current.is_some() {
                attach(w, r, health);
            } else {
                r.native = false;
                set_fallback(health, r, "root missing; waiting for recreation");
            }
        }
    }
}

fn set_fallback(health: &mut MonitorHealth, r: &Root, reason: &str) {
    let root = NativePath::new(&r.path);
    health.polling_fallbacks.retain(|f| f.root != root);
    health.polling_fallbacks.push(RootFallback {
        root,
        reason: bounded(reason),
    });
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_dir())
        .map(|m| {
            let id = FileIdentity::from_metadata(&m);
            (id.volume, id.file)
        })
}

fn touch(r: &mut Root) {
    let now = Instant::now();
    r.first_dirty.get_or_insert(now);
    r.last_dirty = Some(now);
}
fn mark_all(roots: &mut [Root]) {
    for r in roots {
        touch(r);
    }
}
/// Event inside a root, or on the root/one of its ancestors (replacement), dirties that root.
fn mark(roots: &mut [Root], p: &Path) {
    for r in roots.iter_mut().filter(|r| affects(&r.path, p)) {
        touch(r);
    }
}
fn affects(root: &Path, p: &Path) -> bool {
    p.starts_with(root) || root.starts_with(p)
}

/// Rescan the selected roots (all when `only` is None). Lock contention with a
/// concurrent CLI operation leaves the roots queued for the next tick.
fn reconcile(
    engine: &Engine,
    roots: &mut [Root],
    cancel: &Arc<AtomicBool>,
    health: &mut MonitorHealth,
    only: Option<&[usize]>,
) -> (Option<ScanResult>, Vec<String>) {
    let selected: Vec<usize> = only
        .map(<[usize]>::to_vec)
        .unwrap_or_else(|| (0..roots.len()).collect());
    let paths: Vec<PathBuf> = selected
        .iter()
        .map(|&i| roots[i].requested.clone())
        .collect();
    health.status = "reconciling".into();
    save_health(engine, health);
    let mut attempt = 0;
    let result = loop {
        match engine.scan(&paths, cancel.clone(), None) {
            Ok(r) => break Ok(r),
            Err(e)
                if attempt < 20
                    && !cancel.load(Ordering::Relaxed)
                    && format!("{e:#}").contains("another RuleScan operation") =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => break Err(e),
        }
    };
    match result {
        Ok(scan) => {
            for &i in &selected {
                roots[i].first_dirty = None;
                roots[i].last_dirty = None;
                roots[i].coverage_complete = scan.coverage.complete();
            }
            health.queue = roots.iter().filter(|r| r.first_dirty.is_some()).count() as u64;
            health.last_reconcile = Some(now());
            if scan.coverage.complete() && only.is_none_or(|o| o.len() == roots.len()) {
                health.last_complete_reconcile = Some(now());
            }
            let errors: Vec<String> = scan
                .coverage
                .gaps
                .iter()
                .take(20)
                .map(|g| bounded(&format!("coverage gap: {} — {}", g.path.display, g.reason)))
                .collect();
            if let Some(error) = errors.first() {
                health.last_error = Some(error.clone());
            }
            health.pending_findings = engine
                .store
                .artifacts()
                .map(|items| items.iter().filter(|a| a.state.actionable()).count() as u64)
                .unwrap_or(health.pending_findings);
            health.status = if roots.iter().all(|r| r.coverage_complete)
                && health.polling_fallbacks.is_empty()
            {
                "watching"
            } else {
                "degraded"
            }
            .into();
            if roots.iter().all(|r| r.coverage_complete) && health.polling_fallbacks.is_empty() {
                health.last_error = None;
            }
            (Some(scan), errors)
        }
        Err(e) => {
            for &i in &selected {
                roots[i].coverage_complete = false;
            }
            let msg = bounded(&format!("reconcile failed: {e:#}"));
            health.last_error = Some(msg.clone());
            health.status = "degraded".into();
            (None, vec![msg])
        }
    }
}

fn save_health(engine: &Engine, health: &mut MonitorHealth) {
    health.heartbeat_at = now();
    // Health is best effort; a busy database must not stop monitoring.
    let _ = engine.store.save_setting(HEALTH_KEY, health);
}

fn bounded(s: &str) -> String {
    let s = escape(s, false);
    if s.chars().count() <= MAX_ERROR_CHARS {
        s
    } else {
        s.chars().take(MAX_ERROR_CHARS).chain("…".chars()).collect()
    }
}

struct Output<'a> {
    format: Format,
    store: &'a crate::store::Store,
    last: Option<WatchEventKind>,
    last_health_signature: Option<String>,
}
impl Output<'_> {
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        engine: &Engine,
        kind: WatchEventKind,
        trigger: &str,
        scan: Option<&ScanResult>,
        recovery: Vec<ItemOutcome>,
        health: &mut MonitorHealth,
        exit_code: Option<i32>,
        errors: Vec<String>,
    ) -> Result<()> {
        let cursor: u64 = self.store.setting(CURSOR_KEY)?.unwrap_or(0);
        let (mut changes, max_seen) = changes_since(engine, cursor)?;
        let truncated = changes.len() > MAX_CHANGES_PER_EVENT;
        changes.truncate(MAX_CHANGES_PER_EVENT);
        // Low noise: repeated quiet rescans of the same status only refresh the heartbeat.
        let quiet = matches!(kind, WatchEventKind::Reconciled | WatchEventKind::Degraded)
            && trigger != "periodic";
        let signature = digest(&serde_json::to_vec(&(
            &health.status,
            &health.polling_fallbacks,
            &health.last_error,
            &health.watched_roots,
        ))?);
        if quiet
            && self.last == Some(kind)
            && self.last_health_signature.as_ref() == Some(&signature)
            && changes.is_empty()
            && errors.is_empty()
        {
            save_health(engine, health);
            return Ok(());
        }
        self.last = Some(kind);
        self.last_health_signature = Some(signature);
        // The watch record itself takes the next sequence from the persistent audit log.
        self.store
            .event(&format!("watch.{}", kind_label(kind)), None, None)?;
        let sequence = self.store.conn.last_insert_rowid().max(0) as u64;
        self.store
            .save_setting(CURSOR_KEY, &sequence.max(max_seen))?;
        save_health(engine, health);
        let complete = scan.is_some_and(|s| s.coverage.complete()) && errors.is_empty();
        let event = WatchEvent {
            schema_version: EVENT_SCHEMA_VERSION, request_id:new_id(), sequence, created_at: now(), kind, operation: "watch".into(),
            completion: match (kind, scan) { (WatchEventKind::Stopped, _) => "completed", (_, None) => "failed", _ if complete => "completed", _ => "incomplete" }.into(),
            trigger: trigger.into(), coverage: scan.map(|s| s.coverage.clone()),
            profile_version: PROFILE_VERSION.into(), policy_version: POLICY_VERSION.into(),
            changes, changes_truncated: truncated, recovery, health: health.clone(), exit_code, errors,
            note: "Changes are detected after they are written; this is not enforcement. Review with rulescan.".into(),
        };
        let mut stdout = std::io::stdout().lock();
        if self.format == Format::Text {
            writeln!(stdout, "{}", text_line(&event))?;
            for c in &event.changes {
                writeln!(
                    stdout,
                    "  {:<20} {:<12} {}",
                    escape(&c.kind, false),
                    c.state.map_or("-", ApprovalState::label),
                    c.path.as_ref().map_or("-", |p| p.display.as_str())
                )?;
            }
            for e in &event.errors {
                writeln!(stdout, "  {e}")?;
            }
        } else {
            serde_json::to_writer(&mut stdout, &event)?;
            writeln!(stdout)?;
        }
        stdout.flush()?;
        Ok(())
    }
}

fn kind_label(kind: WatchEventKind) -> &'static str {
    match kind {
        WatchEventKind::Started => "started",
        WatchEventKind::Reconciled => "reconciled",
        WatchEventKind::Degraded => "degraded",
        WatchEventKind::Stopped => "stopped",
    }
}

fn text_line(e: &WatchEvent) -> String {
    let gaps = e.coverage.as_ref().map_or(0, |c| c.gaps.len());
    format!(
        "RuleScan watch #{} {} ({}) · {} · {} changes · {} coverage gaps · {} polling fallbacks",
        e.sequence,
        kind_label(e.kind),
        e.trigger,
        e.completion,
        e.changes.len(),
        gaps,
        e.health.polling_fallbacks.len()
    )
}

/// Artifact events written by scans since `cursor`. Watch bookkeeping rows are skipped.
fn changes_since(engine: &Engine, cursor: u64) -> Result<(Vec<ArtifactChange>, u64)> {
    let mut stmt = engine.store.conn.prepare("SELECT sequence,kind,artifact_id,digest FROM events WHERE sequence > ?1 AND kind NOT LIKE 'watch.%' ORDER BY sequence LIMIT ?2")?;
    let rows = stmt.query_map(
        params![cursor as i64, (MAX_CHANGES_PER_EVENT + 1) as i64],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        },
    )?;
    let mut max_seen = cursor;
    let mut changes = Vec::new();
    for row in rows {
        let (seq, kind, artifact_id, digest) = row?;
        max_seen = max_seen.max(seq as u64);
        if kind.starts_with("watch.") {
            continue;
        }
        let artifact = artifact_id
            .as_deref()
            .and_then(|id| engine.store.artifact(id).ok());
        changes.push(ArtifactChange {
            sequence: seq as u64,
            kind: escape(&kind, false),
            artifact_id,
            observed_sha256: digest,
            path: artifact.as_ref().map(|a| a.path.clone()),
            category: artifact.as_ref().map(|a| a.category.clone()),
            state: artifact.map(|a| a.state),
        });
    }
    Ok((changes, max_seen))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, paths::Locations};

    fn engine(dir: &Path) -> Engine {
        Engine::open(
            Locations::resolve(Some(&dir.join("rs"))).unwrap(),
            Config::default(),
        )
        .unwrap()
    }

    #[test]
    fn complete_subset_does_not_hide_another_roots_coverage_gap() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path());
        let good = temp.path().join("good");
        let missing = temp.path().join("missing");
        std::fs::create_dir(&good).unwrap();
        let mut roots: Vec<_> = [good, missing.clone()]
            .into_iter()
            .map(|p| Root {
                requested: p.clone(),
                path: p,
                identity: None,
                native: true,
                coverage_complete: false,
                first_dirty: None,
                last_dirty: None,
                last_poll: Instant::now(),
            })
            .collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut health = MonitorHealth::default();
        assert!(
            reconcile(&engine, &mut roots, &cancel, &mut health, None)
                .0
                .is_some()
        );
        assert_eq!(health.status, "degraded");
        assert!(
            reconcile(&engine, &mut roots, &cancel, &mut health, Some(&[0]))
                .0
                .unwrap()
                .coverage
                .complete()
        );
        assert_eq!(health.status, "degraded");
        std::fs::create_dir(missing).unwrap();
        reconcile(&engine, &mut roots, &cancel, &mut health, None);
        assert_eq!(health.status, "watching");
        assert!(health.last_complete_reconcile.is_some());
    }

    #[test]
    fn watch_bookkeeping_cannot_hide_truncated_artifact_events() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path());
        let transaction = engine.store.conn.unchecked_transaction().unwrap();
        for _ in 0..600 {
            engine.store.event("watch.reconciled", None, None).unwrap();
            engine.store.event("discovered", None, None).unwrap();
        }
        transaction.commit().unwrap();
        let (changes, _) = changes_since(&engine, 0).unwrap();
        assert_eq!(changes.len(), MAX_CHANGES_PER_EVENT + 1);
        assert!(changes.iter().all(|e| e.kind == "discovered"));
    }

    #[test]
    fn affects_root_descendants_and_ancestors_only() {
        let root = Path::new("/w/project");
        assert!(affects(root, Path::new("/w/project/AGENTS.md")));
        assert!(affects(root, Path::new("/w/project")));
        assert!(affects(root, Path::new("/w")));
        assert!(!affects(root, Path::new("/w/project-other/AGENTS.md")));
        assert!(!affects(root, Path::new("/w/sibling")));
    }

    #[test]
    fn bounded_escapes_and_truncates() {
        let s = bounded(&format!("bad\x1b[31m{}", "x".repeat(2000)));
        assert!(!s.contains('\x1b'));
        assert!(s.chars().count() <= MAX_ERROR_CHARS + 1);
    }

    #[test]
    fn once_reconciles_emits_cursor_and_returns_scan_code() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), "untrusted body text").unwrap();
        let e = engine(tmp.path());
        let code = run(
            &e,
            std::slice::from_ref(&root),
            Arc::new(AtomicBool::new(false)),
            true,
            Format::Jsonl,
        )
        .unwrap();
        assert_eq!(code, 1, "new instruction is actionable");
        let first: u64 = e.store.setting(CURSOR_KEY).unwrap().unwrap();
        let health: MonitorHealth = e.store.setting(HEALTH_KEY).unwrap().unwrap();
        assert!(health.last_complete_reconcile.is_some());
        assert_eq!(health.status, "stopped");
        // Nothing was approved or quarantined by the watcher.
        assert!(
            e.store
                .approval(&e.store.artifacts().unwrap()[0].artifact_id)
                .unwrap()
                .is_none()
        );
        assert!(e.store.quarantines().unwrap().is_empty());
        run(
            &e,
            std::slice::from_ref(&root),
            Arc::new(AtomicBool::new(false)),
            true,
            Format::Jsonl,
        )
        .unwrap();
        let second: u64 = e.store.setting(CURSOR_KEY).unwrap().unwrap();
        assert!(second > first, "cursor persists and advances");
        let (changes, _) = changes_since(&e, first).unwrap();
        assert!(changes.is_empty(), "unchanged content is not re-reported");
    }

    #[test]
    fn single_instance_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path());
        let _held = paths::lock(&e.locations.state.join("monitor.lock")).unwrap();
        let err = run(
            &e,
            &[tmp.path().to_owned()],
            Arc::new(AtomicBool::new(false)),
            true,
            Format::Jsonl,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("already running"));
    }

    #[test]
    fn cancellation_is_prompt_and_change_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        let state = tmp.path().to_owned();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let r = root.clone();
        let handle = std::thread::spawn(move || {
            let e = engine(&state);
            let code = run(&e, &[r], flag, false, Format::Jsonl).unwrap();
            let changes = changes_since(&e, 0).unwrap().0;
            (code, changes)
        });
        std::thread::sleep(Duration::from_millis(1500));
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        // Debounce (500 ms) plus native latency; poll the shared database for the observation.
        std::thread::sleep(Duration::from_millis(3000));
        let started = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let (code, changes) = handle.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "idle cancel took {:?}",
            started.elapsed()
        );
        assert_eq!(code, 0);
        assert!(
            changes.iter().any(|c| c.kind == "discovered"),
            "native event triggered a rescan"
        );
    }
}
