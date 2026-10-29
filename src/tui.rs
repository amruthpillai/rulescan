use crate::{config::Config, discovery::ScanEvent, engine::Engine, model::*, paths::Locations};
use anyhow::{Context, Result, bail};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Search,
    Filter,
    Palette,
    Help,
    Detail,
    Review,
    Quit,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Findings,
    Preview,
    Queue,
}

#[derive(Default)]
struct Filter {
    statuses: Vec<String>,
    agent: String,
    category: String,
    workspace: String,
    bundle: bool,
}

impl Filter {
    fn parse(input: &str) -> Result<Self> {
        let mut filter = Self::default();
        for term in input.split_whitespace() {
            let Some((field, value)) = term.split_once(':') else {
                bail!("Use field:value, for example status:new,changed agent:claude");
            };
            match field {
                "status" => {
                    let states: Vec<_> = value.split(',').map(str::to_owned).collect();
                    if states.iter().any(|s| {
                        ![
                            "all",
                            "new",
                            "changed",
                            "missing",
                            "unreadable",
                            "approved",
                            "quarantined",
                            "excluded",
                        ]
                        .contains(&s.as_str())
                    }) {
                        bail!(
                            "Unknown status; use new, changed, missing, unreadable, approved, quarantined, excluded, or all"
                        );
                    }
                    filter.statuses = if states.iter().any(|s| s == "all") {
                        vec![]
                    } else {
                        states
                    };
                }
                "agent" => filter.agent = value.to_lowercase(),
                "category" => filter.category = value.to_lowercase(),
                "workspace" => filter.workspace = value.to_lowercase(),
                "bundle" => match value {
                    "yes" | "true" => filter.bundle = true,
                    "no" | "false" => filter.bundle = false,
                    _ => bail!("bundle must be yes or no"),
                },
                _ => {
                    bail!("Unknown filter field; use status, agent, category, workspace, or bundle")
                }
            }
        }
        Ok(filter)
    }
    fn matches(&self, a: &Artifact) -> bool {
        (self.statuses.is_empty() || self.statuses.iter().any(|s| s == a.state.label()))
            && (self.agent.is_empty()
                || a.profiles
                    .iter()
                    .any(|p| p.to_lowercase().contains(&self.agent)))
            && (self.category.is_empty() || a.category.to_lowercase().contains(&self.category))
            && (self.workspace.is_empty()
                || a.root.display.to_lowercase().contains(&self.workspace))
            && (!self.bundle || a.category == "skill")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PreviewKey {
    id: String,
    digest: Option<String>,
    diff: bool,
}

enum IoEvent {
    Preview {
        key: PreviewKey,
        result: std::result::Result<String, String>,
    },
    Applied(std::result::Result<ApplyResult, String>),
    RestorePlan(std::result::Result<ActionPlan, String>),
    ScopesSaved(Box<std::result::Result<(Config, Vec<PathBuf>), String>>),
    Monitor(Option<serde_json::Value>),
    InventoryExtras {
        quarantine: Vec<QuarantineRecord>,
        monitor: Option<serde_json::Value>,
    },
}

struct App {
    config: Config,
    locations: Locations,
    roots: Vec<PathBuf>,
    artifacts: Vec<Artifact>,
    index: HashMap<String, usize>,
    selected_id: Option<String>,
    checked: HashSet<String>,
    queue: Vec<PlanItem>,
    outcomes: Vec<ItemOutcome>,
    coverage: Coverage,
    scan_running: bool,
    scan_cancel: Arc<AtomicBool>,
    scan_rx: Option<Receiver<ScanEvent>>,
    io_tx: SyncSender<IoEvent>,
    io_rx: Receiver<IoEvent>,
    workers: Vec<JoinHandle<()>>,
    previews: HashMap<PreviewKey, (String, bool)>,
    preview_pending: HashSet<PreviewKey>,
    preview_scroll: u16,
    modal_scroll: u16,
    mode: Mode,
    focus: Focus,
    query: String,
    filter_text: String,
    filter: Filter,
    draft: String,
    diff: bool,
    busy: bool,
    quarantine_view: bool,
    quarantine: Vec<QuarantineRecord>,
    quarantine_selected: usize,
    settings_roots: Vec<(PathBuf, bool)>,
    settings_selected: usize,
    message: String,
    color: bool,
    monitor: Option<serde_json::Value>,
    health_requested_at: Instant,
    health_pending: bool,
    exit_code: i32,
}

impl App {
    fn new(engine: Engine, roots: Vec<PathBuf>, no_color: bool) -> Result<Self> {
        let color = !no_color
            && std::env::var_os("NO_COLOR").is_none()
            && engine.config.ui.color != "never";
        let artifacts: Vec<_> = engine
            .store
            .artifacts()?
            .into_iter()
            .filter(|a| a.root.to_path().ok().is_some_and(|p| roots.contains(&p)))
            .collect();
        let quarantine = engine.store.quarantines()?;
        let coverage = engine.store.last_coverage()?.unwrap_or_default();
        let monitor = engine
            .store
            .setting::<serde_json::Value>("monitor_health")?;
        let (io_tx, io_rx) = mpsc::sync_channel(8);
        let filter_text = "status:new,changed,missing,unreadable".to_owned();
        let mut app = Self {
            config: engine.config,
            locations: engine.locations,
            roots,
            artifacts,
            index: HashMap::new(),
            selected_id: None,
            checked: HashSet::new(),
            queue: vec![],
            outcomes: vec![],
            coverage,
            scan_running: false,
            scan_cancel: Arc::new(AtomicBool::new(false)),
            scan_rx: None,
            io_tx,
            io_rx,
            workers: vec![],
            previews: HashMap::new(),
            preview_pending: HashSet::new(),
            preview_scroll: 0,
            modal_scroll: 0,
            mode: Mode::Normal,
            focus: Focus::Findings,
            query: String::new(),
            filter: Filter::parse(&filter_text)?,
            filter_text,
            draft: String::new(),
            diff: true,
            busy: false,
            quarantine_view: false,
            quarantine,
            quarantine_selected: 0,
            settings_roots: vec![],
            settings_selected: 0,
            message: "Read-only discovery. No content is approved automatically. w chooses scope; ? shows help.".into(),
            color,
            monitor,
            health_requested_at: Instant::now(),
            health_pending: false,
            exit_code: 0,
        };
        app.reindex();
        app.ensure_selection();
        Ok(app)
    }

    fn reindex(&mut self) {
        self.index = self
            .artifacts
            .iter()
            .enumerate()
            .map(|(i, a)| (a.artifact_id.clone(), i))
            .collect();
    }

    fn upsert(&mut self, artifact: Artifact) {
        if let Some(i) = self.index.get(&artifact.artifact_id) {
            self.artifacts[*i] = artifact;
        } else {
            self.index
                .insert(artifact.artifact_id.clone(), self.artifacts.len());
            self.artifacts.push(artifact);
        }
    }

    fn visible(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.artifacts
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                self.filter.matches(a)
                    && (query.is_empty()
                        || a.path.display.to_lowercase().contains(&query)
                        || a.category.to_lowercase().contains(&query))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected(&self) -> Option<&Artifact> {
        self.selected_id
            .as_ref()
            .and_then(|id| self.index.get(id))
            .and_then(|i| self.artifacts.get(*i))
    }

    fn ensure_selection(&mut self) {
        let visible = self.visible();
        if !visible
            .iter()
            .any(|i| Some(&self.artifacts[*i].artifact_id) == self.selected_id.as_ref())
        {
            self.selected_id = visible
                .first()
                .map(|i| self.artifacts[*i].artifact_id.clone());
            self.preview_scroll = 0;
        }
        self.quarantine_selected = self
            .quarantine_selected
            .min(self.quarantine.len().saturating_sub(1));
    }

    fn navigate(&mut self, delta: isize) {
        if self.quarantine_view {
            self.quarantine_selected = self
                .quarantine_selected
                .saturating_add_signed(delta)
                .min(self.quarantine.len().saturating_sub(1));
            return;
        }
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let current = visible
            .iter()
            .position(|i| Some(&self.artifacts[*i].artifact_id) == self.selected_id.as_ref())
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(visible.len() - 1);
        self.selected_id = Some(self.artifacts[visible[next]].artifact_id.clone());
        self.preview_scroll = 0;
    }

    fn start_scan(&mut self) {
        if self.scan_running || self.busy {
            self.message = "An operation is already running.".into();
            return;
        }
        self.scan_running = true;
        self.scan_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.scan_cancel.clone();
        let (tx, rx) = mpsc::sync_channel(128);
        self.scan_rx = Some(rx);
        self.previews.clear();
        let locations = self.locations.clone();
        let config = self.config.clone();
        let roots = self.roots.clone();
        let io_tx = self.io_tx.clone();
        self.workers.push(thread::spawn(move || {
            let result = (|| -> Result<()> {
                let engine = Engine::open(locations, config)?;
                let result = engine.scan(&roots, cancel, Some(&tx))?;
                let extras = IoEvent::InventoryExtras {
                    quarantine: engine.store.quarantines()?,
                    monitor: engine
                        .store
                        .setting::<serde_json::Value>("monitor_health")?,
                };
                let _ = io_tx.send(extras);
                let _ = tx.send(ScanEvent::Finished(Box::new(result)));
                Ok(())
            })();
            if let Err(e) = result {
                let _ = tx.send(ScanEvent::Failed(escape(&format!("{e:#}"), false)));
            }
        }));
    }

    fn pump(&mut self) {
        if let Some(rx) = &self.scan_rx {
            let mut events = Vec::new();
            // Limit work per frame so a discovery burst cannot starve keyboard input.
            for _ in 0..256 {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(_) => break,
                }
            }
            for event in events {
                match event {
                    ScanEvent::Found(artifact) => self.upsert(*artifact),
                    ScanEvent::Progress(entries) => self.coverage.entries_visited = entries,
                    ScanEvent::Finished(result) => {
                        self.exit_code = result.exit_code();
                        let ids: HashSet<_> =
                            result.artifacts.iter().map(|a| &a.artifact_id).collect();
                        self.artifacts.retain(|a| ids.contains(&a.artifact_id));
                        self.reindex();
                        for a in result.artifacts {
                            self.upsert(a);
                        }
                        self.coverage = result.coverage;
                        self.scan_running = false;
                        self.message = if self.coverage.complete() {
                            "Scan complete. Select a finding to review its escaped contents.".into()
                        } else {
                            format!(
                                "Incomplete coverage: {} gaps. ? shows coverage details; no clean verdict is implied.",
                                self.coverage.gaps.len()
                            )
                        };
                    }
                    ScanEvent::Failed(error) => {
                        self.scan_running = false;
                        self.exit_code = 5;
                        self.message = format!("Scan failed: {error}");
                    }
                }
            }
        }
        for _ in 0..16 {
            let Ok(event) = self.io_rx.try_recv() else {
                break;
            };
            match event {
                IoEvent::Preview { key, result } => {
                    self.preview_pending.remove(&key);
                    if self.previews.len() >= 32 {
                        self.previews.clear();
                    }
                    let ok = result.is_ok();
                    self.previews.insert(key, (result.unwrap_or_else(|e| format!("Preview unavailable: {e}\nRefresh and review a new snapshot before staging.")), ok));
                }
                IoEvent::InventoryExtras {
                    quarantine,
                    monitor,
                } => {
                    self.quarantine = quarantine;
                    self.monitor = monitor;
                }
                IoEvent::Applied(result) => {
                    self.busy = false;
                    self.mode = Mode::Normal;
                    match result {
                        Ok(result) => {
                            let completed: HashSet<_> = result
                                .outcomes
                                .iter()
                                .filter(|o| o.status == "completed")
                                .map(|o| o.artifact_id.clone())
                                .collect();
                            self.queue
                                .retain(|i| !completed.contains(&i.artifact.artifact_id));
                            let failed = result.outcomes.len() - completed.len();
                            self.message = format!(
                                "{} completed · {failed} conflicts/failures retained. Tab to actions for per-item outcomes.",
                                completed.len()
                            );
                            self.outcomes = result.outcomes;
                            self.start_scan();
                        }
                        Err(error) => {
                            self.message = format!(
                                "Apply failed: {error}. The queue remains; refresh before retrying."
                            )
                        }
                    }
                }
                IoEvent::Monitor(monitor) => {
                    self.monitor = monitor;
                    self.health_pending = false;
                }
                IoEvent::RestorePlan(result) => {
                    self.busy = false;
                    match result {
                        Ok(plan) => {
                            for item in plan.items {
                                self.queue.retain(|i| {
                                    i.artifact.artifact_id != item.artifact.artifact_id
                                });
                                self.queue.push(item);
                            }
                            self.mode = Mode::Review;
                            self.modal_scroll = 0;
                            self.message = "Restore staged. Review the exact destination; Enter applies and never replaces an existing entry.".into();
                        }
                        Err(error) => self.message = format!("Restore unavailable: {error}"),
                    }
                }
                IoEvent::ScopesSaved(result) => {
                    self.busy = false;
                    match *result {
                        Ok((config, roots)) => {
                            self.config = config;
                            self.roots = roots;
                            self.mode = Mode::Normal;
                            self.message =
                                "Scope saved explicitly. Refreshing the selected roots.".into();
                            self.start_scan();
                        }
                        Err(error) => self.message = format!("Scope was not saved: {error}"),
                    }
                }
            }
        }
        let mut i = 0;
        while i < self.workers.len() {
            if self.workers[i].is_finished() {
                if self.workers.swap_remove(i).join().is_err() {
                    self.scan_running = false;
                    self.busy = false;
                    self.message = "A worker stopped unexpectedly. Refresh; incomplete file operations remain recoverable.".into();
                }
            } else {
                i += 1;
            }
        }
        self.ensure_selection();
        self.request_preview();
        self.request_health();
    }

    fn request_health(&mut self) {
        if self.scan_running
            || self.busy
            || self.health_pending
            || self.health_requested_at.elapsed() < Duration::from_secs(10)
        {
            return;
        }
        self.health_pending = true;
        self.health_requested_at = Instant::now();
        let locations = self.locations.clone();
        let config = self.config.clone();
        let tx = self.io_tx.clone();
        self.workers.push(thread::spawn(move || {
            let monitor = Engine::open(locations, config)
                .and_then(|engine| engine.store.setting::<serde_json::Value>("monitor_health"))
                .ok()
                .flatten();
            let _ = tx.send(IoEvent::Monitor(monitor));
        }));
    }

    fn preview_key(&self) -> Option<PreviewKey> {
        self.selected().map(|a| PreviewKey {
            id: a.artifact_id.clone(),
            digest: a.observed_sha256.clone(),
            diff: self.diff,
        })
    }

    fn request_preview(&mut self) {
        if self.scan_running
            || self.busy
            || self.quarantine_view
            || self.focus == Focus::Queue
            || self.preview_pending.len() >= 2
        {
            return;
        }
        let Some(key) = self.preview_key() else {
            return;
        };
        if self.previews.contains_key(&key) || !self.preview_pending.insert(key.clone()) {
            return;
        }
        let locations = self.locations.clone();
        let config = self.config.clone();
        let tx = self.io_tx.clone();
        self.workers.push(thread::spawn(move || {
            let result = (|| -> Result<String> {
                let engine = Engine::open(locations, config)?;
                let data = engine.inspect(&key.id, true, key.diff)?;
                let content = if key.diff {
                    data["diff"].as_str().unwrap_or_else(|| data["content"].as_str().unwrap_or("No preview available"))
                } else { data["content"].as_str().unwrap_or("No preview available") };
                let mut text = content.to_owned();
                if let Some(entrypoint) = data["entrypoint_content"].as_str() {
                    let entrypoint_path = data["entrypoint_path"]["display"].as_str().unwrap_or("skill marker");
                    text.insert_str(0, &format!("Skill instructions · {} · untrusted\n{}\n\nBundle manifest / approved diff\n", escape(entrypoint_path, false), entrypoint));
                    if data["entrypoint_truncated"] == true { text.push_str("\n[Skill instruction preview truncated; complete bytes are hashed.]"); }
                }
                if key.diff && data["baseline_available"] == false { text.insert_str(0, "No retained approved text baseline; showing current content.\n\n"); }
                if data["truncated"] == true { text.push_str("\n\n[Preview truncated at 64 KiB; approval still hashes the complete bytes.]"); }
                Ok(escape(&text, true))
            })().map_err(|e| escape(&format!("{e:#}"), false));
            let _ = tx.send(IoEvent::Preview { key, result });
        }));
    }

    fn stage(&mut self, kind: ActionKind) {
        if self.scan_running || self.busy || self.quarantine_view {
            self.message = "Wait for discovery to complete; restore is available with r in the quarantine view.".into();
            return;
        }
        let preview_ok = self
            .preview_key()
            .as_ref()
            .and_then(|k| self.previews.get(k))
            .is_some_and(|(_, ok)| *ok);
        if !preview_ok {
            self.message =
                "Review the current safe preview before staging; refresh if it is unavailable."
                    .into();
            return;
        }
        let visible = self.visible();
        let selected_visible: Vec<_> = visible
            .iter()
            .filter(|i| self.checked.contains(&self.artifacts[**i].artifact_id))
            .copied()
            .collect();
        let targets = if selected_visible.is_empty() {
            self.selected_id
                .as_ref()
                .and_then(|id| self.index.get(id))
                .copied()
                .into_iter()
                .collect()
        } else {
            selected_visible
        };
        let action = if kind == ActionKind::Approve {
            "approve"
        } else {
            "quarantine"
        };
        let mut staged = 0;
        let mut readonly = 0;
        let mut decisions = Vec::new();
        for index in targets {
            let a = &self.artifacts[index];
            let Some(expected) = a.observed_sha256.clone() else {
                readonly += 1;
                continue;
            };
            if !a.allowed_next_actions.iter().any(|s| s == action) {
                readonly += 1;
                continue;
            }
            decisions.push(PlanItem {
                kind: kind.clone(),
                artifact: a.clone(),
                expected_sha256: expected,
                quarantine_id: None,
                destination: None,
                destination_parent_identity: None,
            });
            self.checked.remove(&a.artifact_id);
            staged += 1;
        }
        let replaced: HashSet<_> = decisions.iter().map(|i| &i.artifact.artifact_id).collect();
        self.queue
            .retain(|i| !replaced.contains(&i.artifact.artifact_id));
        self.queue.extend(decisions);
        self.message = format!(
            "Staged {staged} {action} decisions from visible rows only; {readonly} read-only items skipped. c reviews; u undoes."
        );
        self.advance_unresolved();
    }

    fn advance_unresolved(&mut self) {
        let visible = self.visible();
        let staged: HashSet<_> = self.queue.iter().map(|i| &i.artifact.artifact_id).collect();
        let current = visible
            .iter()
            .position(|i| Some(&self.artifacts[*i].artifact_id) == self.selected_id.as_ref())
            .unwrap_or(0);
        for offset in 1..=visible.len() {
            let index = visible[(current + offset) % visible.len()];
            let a = &self.artifacts[index];
            if a.state.actionable() && !staged.contains(&a.artifact_id) {
                self.selected_id = Some(a.artifact_id.clone());
                self.preview_scroll = 0;
                break;
            }
        }
    }

    fn apply_queue(&mut self) {
        if self.busy || self.scan_running || self.queue.is_empty() {
            self.message =
                "There are no ready decisions to apply, or discovery is still running.".into();
            return;
        }
        self.busy = true;
        let items = self.queue.clone();
        let locations = self.locations.clone();
        let config = self.config.clone();
        let tx = self.io_tx.clone();
        self.message =
            "Applying reviewed snapshots. Each item is revalidated; conflicts stay visible.".into();
        self.workers.push(thread::spawn(move || {
            let result = (|| -> Result<ApplyResult> {
                let engine = Engine::open(locations, config)?;
                let plan = engine.mixed_plan(items)?;
                engine.apply(&plan.plan_id)
            })()
            .map_err(|e| escape(&format!("{e:#}"), false));
            let _ = tx.send(IoEvent::Applied(result));
        }));
    }

    fn stage_restore(&mut self) {
        if self.busy || self.scan_running {
            self.message = "Wait for the current operation before planning a restore.".into();
            return;
        }
        let Some(q) = self.quarantine.get(self.quarantine_selected) else {
            return;
        };
        let id = q.quarantine_id.clone();
        let locations = self.locations.clone();
        let config = self.config.clone();
        let tx = self.io_tx.clone();
        self.busy = true;
        self.workers.push(thread::spawn(move || {
            let result = (|| -> Result<ActionPlan> {
                Engine::open(locations, config)?.restore_plan(&id, None)
            })()
            .map_err(|e| escape(&format!("{e:#}"), false));
            let _ = tx.send(IoEvent::RestorePlan(result));
        }));
    }

    fn open_settings(&mut self) {
        self.settings_roots = self.roots.iter().cloned().map(|p| (p, true)).collect();
        if let Some(base) = directories::BaseDirs::new() {
            let downloads = base.home_dir().join("Downloads");
            if !self.settings_roots.iter().any(|(p, _)| p == &downloads) {
                self.settings_roots.push((downloads, false));
            }
        }
        self.settings_selected = 0;
        self.mode = Mode::Settings;
    }

    fn save_settings(&mut self) {
        if self.scan_running || self.busy {
            self.message = "Wait for discovery before saving scope.".into();
            return;
        }
        let roots: Vec<_> = self
            .settings_roots
            .iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(p, _)| p.clone())
            .collect();
        if roots.is_empty() {
            self.message = "Choose at least one scan root.".into();
            return;
        }
        let mut config = self.config.clone();
        config.scan.roots = roots.clone();
        let path = self.locations.config.clone();
        let tx = self.io_tx.clone();
        self.busy = true;
        self.workers.push(thread::spawn(move || {
            let result = crate::cli::write_config_new(&path, &config)
                .map(|()| (config, roots)).map_err(|e| escape(&format!("{e:#}. Existing configuration is never overwritten; edit scan.roots in config.yaml to update it."), false));
            let _ = tx.send(IoEvent::ScopesSaved(Box::new(result)));
        }));
    }

    fn queue_text(&self) -> String {
        let mut text = format!(
            "{} staged decisions · visible selections only\n\n",
            self.queue.len()
        );
        if self.queue.is_empty() {
            text.push_str(
                "Use a to stage approval or x to stage quarantine after reviewing a preview.\n",
            );
        }
        for (i, item) in self.queue.iter().enumerate() {
            text.push_str(&format!(
                "{}. {:?} · {}\n   {}\n   SHA-256 {}\n",
                i + 1,
                item.kind,
                if item.artifact.category == "skill" {
                    "whole skill bundle"
                } else {
                    "individual file"
                },
                item.artifact.path.display,
                item.expected_sha256
            ));
            if let Some(destination) = &item.destination {
                text.push_str(&format!(
                    "   Restore to {} (must not exist)\n",
                    destination.display
                ));
            }
        }
        if !self.outcomes.is_empty() {
            text.push_str("\nLast application — per-item outcomes\n");
            for outcome in &self.outcomes {
                let path = self
                    .index
                    .get(&outcome.artifact_id)
                    .map(|i| self.artifacts[*i].path.display.as_str())
                    .unwrap_or(&outcome.artifact_id);
                text.push_str(&format!(
                    "{} · {}\n{}\n",
                    outcome.status, path, outcome.detail
                ));
            }
        }
        escape(&text, true)
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        let movement = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('j' | 'k')
        );
        if key.kind == KeyEventKind::Release || (key.kind == KeyEventKind::Repeat && !movement) {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.request_quit();
        }
        if self.busy {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                self.message = "Finishing the current operation; recoverable actions are never abandoned mid-application.".into();
            }
            return false;
        }
        if matches!(self.mode, Mode::Search | Mode::Filter | Mode::Palette) {
            return self.input_key(key);
        }
        if self.mode == Mode::Quit {
            return match key.code {
                KeyCode::Char('q') | KeyCode::Enter => true,
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    false
                }
                _ => false,
            };
        }
        if self.mode == Mode::Settings {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.settings_selected = (self.settings_selected + 1)
                        .min(self.settings_roots.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.settings_selected = self.settings_selected.saturating_sub(1)
                }
                KeyCode::Char(' ') => {
                    if let Some((_, enabled)) = self.settings_roots.get_mut(self.settings_selected)
                    {
                        *enabled = !*enabled;
                    }
                }
                KeyCode::Char('s') => self.save_settings(),
                KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                _ => {}
            }
            return false;
        }
        if matches!(self.mode, Mode::Help | Mode::Review | Mode::Detail) {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal_scroll = self.modal_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal_scroll = self.modal_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => self.modal_scroll = self.modal_scroll.saturating_add(10),
                KeyCode::PageUp => self.modal_scroll = self.modal_scroll.saturating_sub(10),
                KeyCode::Enter if self.mode == Mode::Review => self.apply_queue(),
                _ => {}
            }
            return false;
        }
        let mut code = key.code;
        if let KeyCode::Char(c) = code
            && let Some((action, _)) = self
                .config
                .ui
                .keymap
                .iter()
                .find(|(_, value)| value.starts_with(c))
        {
            code = KeyCode::Char(match action.as_str() {
                "approve" => 'a',
                "quarantine" => 'x',
                "undo" => 'u',
                "review" => 'c',
                "refresh" => 'r',
                "diff" => 'd',
                "help" => '?',
                "settings" => 'w',
                "quarantine_view" => 'Q',
                _ => c,
            });
        }
        match code {
            KeyCode::Char('q') => return self.request_quit(),
            KeyCode::Down | KeyCode::Char('j') if self.focus != Focus::Findings => {
                self.preview_scroll = self.preview_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus != Focus::Findings => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.preview_scroll = self.preview_scroll.saturating_add(10),
            KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(10),
            KeyCode::Down | KeyCode::Char('j') => self.navigate(1),
            KeyCode::Up | KeyCode::Char('k') => self.navigate(-1),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Findings => Focus::Preview,
                    Focus::Preview => Focus::Queue,
                    Focus::Queue => Focus::Findings,
                };
                self.preview_scroll = 0;
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Findings => Focus::Queue,
                    Focus::Preview => Focus::Findings,
                    Focus::Queue => Focus::Preview,
                };
                self.preview_scroll = 0;
            }
            KeyCode::Enter => {
                self.mode = Mode::Detail;
                self.modal_scroll = 0;
            }
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.draft = self.query.clone();
            }
            KeyCode::Char('f') => {
                self.mode = Mode::Filter;
                self.draft = self.filter_text.clone();
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Palette;
                self.draft.clear();
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                self.modal_scroll = 0;
            }
            KeyCode::Char('d') => {
                self.diff = !self.diff;
                self.preview_scroll = 0;
            }
            KeyCode::Char('a') => self.stage(ActionKind::Approve),
            KeyCode::Char('x') => self.stage(ActionKind::Quarantine),
            KeyCode::Char('u') => {
                self.queue.pop();
                self.message = "Removed the latest staged decision. Completed actions are restored through Q, then r.".into();
            }
            KeyCode::Char('c') => {
                self.mode = Mode::Review;
                self.modal_scroll = 0;
            }
            KeyCode::Char('r') => {
                if self.quarantine_view {
                    self.stage_restore();
                } else {
                    self.start_scan();
                }
            }
            KeyCode::Char('Q') => {
                self.quarantine_view = !self.quarantine_view;
                self.focus = Focus::Findings;
                self.preview_scroll = 0;
            }
            KeyCode::Char('w') => self.open_settings(),
            KeyCode::Esc => {
                self.query.clear();
                self.ensure_selection();
            }
            _ => {}
        }
        false
    }

    fn input_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.draft.pop();
            }
            KeyCode::Char(c) if !c.is_control() && self.draft.len() < 1024 => self.draft.push(c),
            KeyCode::Enter => match self.mode {
                Mode::Search => {
                    self.query = self.draft.clone();
                    self.mode = Mode::Normal;
                    self.ensure_selection();
                }
                Mode::Filter => match Filter::parse(&self.draft) {
                    Ok(filter) => {
                        self.filter = filter;
                        self.filter_text = self.draft.clone();
                        self.mode = Mode::Normal;
                        self.ensure_selection();
                    }
                    Err(e) => self.message = escape(&e.to_string(), false),
                },
                Mode::Palette => {
                    let commands = palette(&self.draft);
                    let command = commands.first().map(|(name, _)| *name).unwrap_or("");
                    self.mode = Mode::Normal;
                    match command {
                        "review actions" => {
                            self.mode = Mode::Review;
                            self.modal_scroll = 0;
                        }
                        "refresh scan" => self.start_scan(),
                        "quarantine and restore" => self.quarantine_view = !self.quarantine_view,
                        "scope settings" => self.open_settings(),
                        "help and coverage" => {
                            self.mode = Mode::Help;
                            self.modal_scroll = 0;
                        }
                        "show all findings" => {
                            self.filter = Filter::default();
                            self.filter_text = "status:all".into();
                            self.ensure_selection();
                        }
                        "show review queue" => {
                            self.filter_text = "status:new,changed,missing,unreadable".into();
                            self.filter = Filter::parse(&self.filter_text).unwrap_or_default();
                            self.ensure_selection();
                        }
                        "toggle baseline diff" => self.diff = !self.diff,
                        _ => self.message = "No matching command.".into(),
                    }
                }
                _ => {}
            },
            _ => {}
        }
        false
    }

    fn request_quit(&mut self) -> bool {
        if self.busy {
            self.message = "Finishing the current operation before leaving.".into();
            return false;
        }
        if self.queue.is_empty() {
            return true;
        }
        self.mode = Mode::Quit;
        false
    }

    fn toggle_selection(&mut self) {
        if !self.quarantine_view
            && let Some(id) = &self.selected_id
            && !self.checked.remove(id)
        {
            self.checked.insert(id.clone());
        }
    }

    fn accent(&self) -> Style {
        if self.color {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        }
    }
    fn border(&self, focus: Focus) -> Style {
        if self.focus == focus {
            self.accent()
        } else {
            Style::default()
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let [header, body, status, footer] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .areas(area);
        let state = if self.busy {
            "applying/planning"
        } else if self.scan_running {
            "scanning · read-only"
        } else if self.coverage.complete() {
            "scan complete"
        } else {
            "coverage incomplete"
        };
        let monitor = monitor_label(self.monitor.as_ref());
        let scope = self
            .roots
            .iter()
            .map(|p| NativePath::new(p).display)
            .collect::<Vec<_>>()
            .join(" · ");
        let header_text = vec![
            Line::from(vec![
                Span::styled("▣ rulescan", self.accent().add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    "  {state}  |  {} entries  |  {} gaps  |  {} staged",
                    self.coverage.entries_visited,
                    self.coverage.gaps.len(),
                    self.queue.len()
                )),
            ]),
            Line::raw(format!("Scope: {}", escape(&scope, false))),
            Line::raw(format!(
                "Monitor: {}  |  Candidate discovery does not prove safety",
                escape(&monitor, false)
            )),
        ];
        frame.render_widget(Paragraph::new(header_text), header);
        let (list_area, preview_area) = if area.width >= 110 {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .areas(body);
            (left, right)
        } else {
            let [top, bottom] =
                Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .areas(body);
            (top, bottom)
        };
        if self.quarantine_view {
            self.draw_quarantine(frame, list_area);
        } else {
            self.draw_findings(frame, list_area);
        }
        let preview = self.preview_text();
        let title = if self.focus == Focus::Queue {
            " Actions · c reviews · Enter applies only in review "
        } else if self.quarantine_view {
            " Recovery details · r previews restore "
        } else if self.diff {
            " Untrusted preview / approved diff · d toggles "
        } else {
            " Untrusted current content · d toggles "
        };
        frame.render_widget(
            Paragraph::new(preview)
                .wrap(Wrap { trim: false })
                .scroll((self.preview_scroll, 0))
                .block(Block::bordered().title(title).border_style(
                    if self.focus == Focus::Findings {
                        Style::default()
                    } else {
                        self.accent()
                    },
                )),
            preview_area,
        );
        frame.render_widget(
            Paragraph::new(escape(&self.message, false)).wrap(Wrap { trim: false }),
            status,
        );
        let shortcuts = if self.busy {
            "Applying/planning the reviewed operation; conflicts and recovery remain recorded."
        } else if self.scan_running {
            "j/k move  / search  f filter  w scope  ? coverage  q quit\nRead-only scan in progress; decisions unlock after reconciliation."
        } else if self.quarantine_view {
            "j/k move  r restore preview  Q findings  c review  ? help  q quit"
        } else {
            "j/k move  Space select  a approve  x quarantine  u undo  c review\n/ search  f filter  d diff  r refresh  Q restore  w scope  : commands  ? help  q quit"
        };
        frame.render_widget(Paragraph::new(shortcuts).style(self.accent()), footer);
        if self.mode != Mode::Normal {
            self.draw_modal(frame);
        }
    }

    fn draw_findings(&self, frame: &mut Frame, area: Rect) {
        let visible = self.visible();
        let staged_by_id: HashMap<_, _> = self
            .queue
            .iter()
            .map(|i| (&i.artifact.artifact_id, &i.kind))
            .collect();
        let mut selected = None;
        let rows: Vec<_> = visible
            .iter()
            .enumerate()
            .map(|(position, index)| {
                let a = &self.artifacts[*index];
                if Some(&a.artifact_id) == self.selected_id.as_ref() {
                    selected = Some(position);
                }
                let checked = if self.checked.contains(&a.artifact_id) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let staged = staged_by_id.get(&a.artifact_id).map_or("", |kind| {
                    if **kind == ActionKind::Approve {
                        " → approve"
                    } else {
                        " → quarantine"
                    }
                });
                ListItem::new(vec![
                    Line::raw(format!(
                        "{checked} {:<10} {}{staged}",
                        a.state.label(),
                        escape(&a.category, false)
                    )),
                    Line::raw(format!("    {}", a.relative_path.display)),
                    Line::raw(format!(
                        "    {} · {}",
                        escape(&a.profiles.join(","), false),
                        a.root.display
                    )),
                ])
            })
            .collect();
        let visible_selected = visible
            .iter()
            .filter(|i| self.checked.contains(&self.artifacts[**i].artifact_id))
            .count();
        let title = format!(
            " Findings {} / {} · {} selected here · {} ",
            visible.len(),
            self.artifacts.len(),
            visible_selected,
            escape(&self.query, false)
        );
        let list = List::new(rows)
            .block(
                Block::bordered()
                    .title(title)
                    .border_style(self.border(Focus::Findings)),
            )
            .highlight_symbol("› ")
            .highlight_style(self.accent().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, area, &mut state);
        if visible.is_empty() {
            let inner = area.inner(ratatui::layout::Margin::new(2, 2));
            frame.render_widget(Paragraph::new(if self.scan_running { "Discovering instruction candidates…" } else { "No findings match this view.\nf changes filters; : → show all findings.\nCoverage gaps remain visible in ? help." }).wrap(Wrap { trim: false }), inner);
        }
    }

    fn draw_quarantine(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<_> = self
            .quarantine
            .iter()
            .map(|q| {
                ListItem::new(vec![
                    Line::raw(format!(
                        "{} · {}",
                        escape(&q.stage, false),
                        q.artifact.path.display
                    )),
                    Line::raw(format!("  {}", q.quarantine_id)),
                ])
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::bordered()
                    .title(format!(
                        " Quarantine · {} retained records · r restores ",
                        self.quarantine.len()
                    ))
                    .border_style(self.border(Focus::Findings)),
            )
            .highlight_symbol("› ")
            .highlight_style(self.accent().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default()
            .with_selected((!self.quarantine.is_empty()).then_some(self.quarantine_selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn preview_text(&self) -> String {
        if self.focus == Focus::Queue {
            return self.queue_text();
        }
        if self.quarantine_view {
            return self.quarantine.get(self.quarantine_selected).map_or("No quarantine records. Quarantined content is never purged automatically.".into(), |q| {
                format!("Original: {}\nState: {}\nUnit: {}\nStored SHA-256: {}\n\nr prepares a restore to the original location. c reviews it before Enter applies.\n\nAn existing destination is never overwritten. For another destination use:\nrulescan restore {} --to PATH\n\nRecovery problems: rulescan recover and rulescan doctor", q.artifact.path.display, escape(&q.stage, false), escape(&q.artifact.category, false), q.payload_sha256, q.quarantine_id)
            });
        }
        let Some(a) = self.selected() else {
            return "Select a finding to inspect escaped contents or an approved-baseline diff.\n\nSpace selects. a/x stage. c reviews the complete plan.\nNo Markdown, hooks, links, or MCP servers execute.".into();
        };
        let mut text = format!(
            "{}\n{} · {}\nSHA-256 {}\n\n",
            a.path.display,
            a.state.label(),
            escape(&a.category, false),
            a.observed_sha256.as_deref().unwrap_or("unavailable")
        );
        if self.scan_running {
            text.push_str("Discovery is in progress. Preview and decisions unlock when the final snapshot is reconciled.");
            return text;
        }
        let key = self.preview_key();
        if let Some((preview, _)) = key.as_ref().and_then(|k| self.previews.get(k)) {
            text.push_str(preview);
        } else {
            text.push_str("Loading safe preview…");
        }
        text
    }

    fn draw_modal(&self, frame: &mut Frame) {
        let area = centered(frame.area());
        frame.render_widget(Clear, area);
        let (title, content) = match self.mode {
            Mode::Search => (
                " Search paths and names · Enter applies · Esc cancels ".to_owned(),
                format!("/ {}", escape(&self.draft, false)),
            ),
            Mode::Filter => (
                " Filter · Enter applies · Esc cancels ".into(),
                format!(
                    "{}\n\nFields: status, agent, category, workspace, bundle\nExamples:\nstatus:new,changed agent:claude\ncategory:skill bundle:yes\nworkspace:Projects status:all\n\nEmpty filter shows all candidates. Selections outside the visible filter are never staged by a batch action.",
                    escape(&self.draft, false)
                ),
            ),
            Mode::Palette => {
                let matches = palette(&self.draft)
                    .into_iter()
                    .map(|(name, detail)| format!("{name}\n  {detail}"))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                (
                    " Command palette · type to search · Enter runs first match ".into(),
                    format!(": {}\n\n{matches}", escape(&self.draft, false)),
                )
            }
            Mode::Review => (
                " Review decisions · Enter applies · Esc returns · j/k scroll ".into(),
                format!(
                    "{}\n\nEvery target is rehashed before application. Drift becomes a conflict. Batch items complete independently.\n\nQuarantine moves recoverable originals into private storage. Restore never replaces a destination. Nothing is permanently deleted.",
                    self.queue_text()
                ),
            ),
            Mode::Quit => (
                " Discard staged decisions? ".into(),
                format!(
                    "{} staged decisions have not been applied.\n\nEnter or q discards them and quits.\nEsc returns to review.\n\nCompleted actions and recoverable quarantine are retained.",
                    self.queue.len()
                ),
            ),
            Mode::Detail => (
                " Artifact detail · Esc returns · j/k scroll ".into(),
                self.detail_text(),
            ),
            Mode::Settings => {
                let choices = self
                    .settings_roots
                    .iter()
                    .enumerate()
                    .map(|(i, (p, enabled))| {
                        format!(
                            "{} {} {}",
                            if i == self.settings_selected {
                                "›"
                            } else {
                                " "
                            },
                            if *enabled { "[x]" } else { "[ ]" },
                            NativePath::new(p).display
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    " Scope settings · Space toggles · s saves · Esc cancels ".into(),
                    format!(
                        "{choices}\n\nTrust: exact approved content only. New or changed files always need review.\n\nDownloads is an explicit choice; broader home scanning is never enabled here.\n\ns creates a new private config with exactly these roots. Existing configuration is never overwritten; update its scan.roots manually.\n\nConfig: {}",
                        NativePath::new(&self.locations.config).display
                    ),
                )
            }
            Mode::Help => (
                " Help and coverage · Esc returns · j/k scroll ".into(),
                self.help_text(),
            ),
            Mode::Normal => return,
        };
        let widget = Paragraph::new(escape(&content, true))
            .wrap(Wrap { trim: false })
            .scroll((self.modal_scroll, 0))
            .block(Block::bordered().title(title).border_style(self.accent()));
        frame.render_widget(widget, area);
    }

    fn detail_text(&self) -> String {
        if self.quarantine_view {
            return self.preview_text();
        }
        let Some(a) = self.selected() else {
            return "No finding selected.".into();
        };
        format!(
            "{}\n\nArtifact ID: {}\nWorkspace: {}\nCategory: {}\nProfiles: {}\nMatched rules: {}\nState: {}\nAllowed actions: {}\n\nReasons:\n{}\n\n{}",
            a.path.display,
            a.artifact_id,
            a.root.display,
            escape(&a.category, false),
            escape(&a.profiles.join(", "), false),
            escape(&a.matching_rule_ids.join(", "), false),
            a.state.label(),
            escape(&a.allowed_next_actions.join(", "), false),
            a.reasons
                .iter()
                .map(|r| escape(r, false))
                .collect::<Vec<_>>()
                .join("\n"),
            self.preview_text()
        )
    }

    fn help_text(&self) -> String {
        let mut text = "RuleScan reviews local instruction candidates. It does not establish that content is safe, enforce agent behavior, or prevent a file from being consumed before a watcher notices it.\n\nj/k or arrows: move · Tab/Shift Tab: focus\nSpace: select, never delete\nEnter: detail · /: path/name search · f: field filters\nd: approved diff/current content\na: stage approval · x: stage quarantine · u: undo latest stage\nc: review queue, then Enter applies exact snapshots\nr: refresh · Q: quarantine view, r previews restore\nw: explicit scope setup · :: searchable commands · ?: help\nq/Ctrl C: quit, with a warning for staged decisions\nEsc: close or clear search · Page Up/Down: scroll preview\n\nBatch staging uses selected visible rows only. Approval binds to exact bytes and bundle resources; changes require new review. Quarantine retains recoverable originals. Restoring never overwrites an existing destination.\n\nRepeat/release events cannot stage decisions. Escaped previews never execute Markdown, terminal control sequences, hooks, imports, or links.\n\nConfigured roots:\n".to_owned();
        for root in &self.roots {
            text.push_str(&format!("{}\n", NativePath::new(root).display));
        }
        text.push_str("\nCoverage gaps:\n");
        if self.coverage.gaps.is_empty() {
            text.push_str(
                "No recorded gaps in the latest scan. This is not a complete protection verdict.\n",
            );
        }
        for gap in &self.coverage.gaps {
            text.push_str(&format!(
                "{} — {}\n",
                gap.path.display,
                escape(&gap.reason, false)
            ));
        }
        text.push_str("\nExplicit exclusions:\n");
        for exclusion in &self.coverage.exclusions {
            text.push_str(&format!(
                "{} — {}\n",
                exclusion.path.display,
                escape(&exclusion.reason, false)
            ));
        }
        if !self.config.ui.keymap.is_empty() {
            text.push_str(&format!(
                "\nAdditional key bindings:\n{}",
                serde_json::to_string_pretty(&self.config.ui.keymap).unwrap_or_default()
            ));
        }
        text
    }
}

fn palette(query: &str) -> Vec<(&'static str, &'static str)> {
    let query = query.to_lowercase();
    [
        (
            "review actions",
            "Review staged paths and exact snapshots before applying",
        ),
        (
            "refresh scan",
            "Reconcile current roots without approving content",
        ),
        (
            "quarantine and restore",
            "Inspect retained payloads and prepare safe restoration",
        ),
        (
            "scope settings",
            "Choose roots and explicitly create a configuration",
        ),
        (
            "help and coverage",
            "Keys, limitations, inaccessible paths, and exclusions",
        ),
        (
            "show all findings",
            "Include unchanged approved content in this view",
        ),
        (
            "show review queue",
            "Show new, changed, missing, and unreadable findings",
        ),
        (
            "toggle baseline diff",
            "Switch between escaped current content and approved diff",
        ),
    ]
    .into_iter()
    .filter(|(name, description)| {
        name.contains(&query) || description.to_lowercase().contains(&query)
    })
    .collect()
}

fn monitor_label(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return "unknown; use service status".into();
    };
    let status = value["status"].as_str().unwrap_or("unknown");
    if status == "stopped" {
        return "stopped".into();
    }
    let heartbeat = value["heartbeat_at"].as_u64().unwrap_or(0);
    let interval = value["heartbeat_interval_secs"].as_u64().unwrap_or(15);
    if now().saturating_sub(heartbeat) > interval.saturating_mul(3).max(45) {
        return "stale heartbeat; running state unknown".into();
    }
    let fallbacks = value["polling_fallbacks"].as_array().map_or(0, Vec::len);
    format!(
        "{} · {} polling fallbacks",
        escape(status, false),
        fallbacks
    )
}

fn centered(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).clamp(1, 110).min(area.width);
    let height = area.height.saturating_sub(2).max(1).min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

struct TerminalRestore;
impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

pub fn run(
    engine: Engine,
    roots: Vec<PathBuf>,
    no_color: bool,
    cancel: Arc<AtomicBool>,
) -> Result<i32> {
    let mut app = App::new(engine, roots, no_color)?;
    let mut terminal =
        ratatui::try_init().context("cannot initialize terminal; use --plain or --format json")?;
    let restore = TerminalRestore;
    app.start_scan();
    let result = (|| -> Result<i32> {
        loop {
            // Raw Ctrl-C is handled as a key below. OS termination cannot wait for
            // a confirmation in a terminal that may already have disconnected.
            if cancel.load(Ordering::Relaxed) {
                return Ok(130);
            }
            app.pump();
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if app.key(key) => return Ok(app.exit_code),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    })();
    app.scan_cancel.store(true, Ordering::Relaxed);
    // Restore the terminal before waiting for bounded filesystem work to finish.
    drop(restore);
    drop(app.scan_rx.take());
    drop(app.io_rx);
    for worker in app.workers {
        let _ = worker.join();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn artifact(id: &str, path: &str, state: ApprovalState) -> Artifact {
        let identity = FileIdentity {
            volume: 1,
            file: 1,
            links: 1,
            mode: 0o644,
            size: 4,
            modified_ns: 0,
        };
        Artifact {
            artifact_id: id.into(),
            root_id: "root".into(),
            root: NativePath::new(std::path::Path::new("/workspace")),
            root_identity: identity.clone(),
            path: NativePath::new(std::path::Path::new(path)),
            relative_path: NativePath::new(std::path::Path::new(path)),
            category: "instructions".into(),
            profiles: vec!["claude".into()],
            matching_rule_ids: vec!["claude.memory".into()],
            reasons: vec!["candidate only".into()],
            observed_sha256: Some("a".repeat(64)),
            approval_digest: Some("b".repeat(64)),
            identity: Some(identity),
            bundle_manifest: vec![],
            references: vec![],
            state,
            profile_version: "builtin-1".into(),
            policy_version: POLICY_VERSION.into(),
            allowed_next_actions: vec!["approve".into(), "quarantine".into()],
            observed_at: 0,
        }
    }

    fn app() -> App {
        let (io_tx, io_rx) = mpsc::sync_channel(8);
        let mut app = App {
            config: Config::default(),
            locations: Locations {
                state: PathBuf::from("/state"),
                data: PathBuf::from("/data"),
                config: PathBuf::from("/config.yaml"),
                cache: PathBuf::from("/cache"),
            },
            roots: vec![PathBuf::from("/workspace")],
            artifacts: vec![
                artifact("first", "/workspace/CLAUDE.md", ApprovalState::New),
                artifact("second", "/workspace/AGENTS.md", ApprovalState::Changed),
            ],
            index: HashMap::new(),
            selected_id: Some("first".into()),
            checked: HashSet::new(),
            queue: vec![],
            outcomes: vec![],
            coverage: Coverage::default(),
            scan_running: false,
            scan_cancel: Arc::new(AtomicBool::new(false)),
            scan_rx: None,
            io_tx,
            io_rx,
            workers: vec![],
            previews: HashMap::new(),
            preview_pending: HashSet::new(),
            preview_scroll: 0,
            modal_scroll: 0,
            mode: Mode::Normal,
            focus: Focus::Findings,
            query: String::new(),
            filter_text: "status:all".into(),
            filter: Filter::default(),
            draft: String::new(),
            diff: true,
            busy: false,
            quarantine_view: false,
            quarantine: vec![],
            quarantine_selected: 0,
            settings_roots: vec![],
            settings_selected: 0,
            message: "Read-only discovery".into(),
            color: false,
            monitor: None,
            health_requested_at: Instant::now(),
            health_pending: false,
            exit_code: 0,
        };
        app.reindex();
        for a in &app.artifacts {
            app.previews.insert(
                PreviewKey {
                    id: a.artifact_id.clone(),
                    digest: a.observed_sha256.clone(),
                    diff: true,
                },
                ("untrusted content".into(), true),
            );
        }
        app
    }

    #[test]
    fn repeat_events_cannot_stage_or_select_and_space_never_mutates() {
        let mut app = app();
        app.key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        app.key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(app.queue.is_empty());
        app.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.checked.len(), 1);
        assert!(app.queue.is_empty());
    }

    #[test]
    fn hidden_selections_do_not_expand_a_batch_and_staging_requires_preview() {
        let mut app = app();
        app.checked
            .extend(["first".to_owned(), "second".to_owned()]);
        app.query = "CLAUDE".into();
        app.stage(ActionKind::Approve);
        assert_eq!(app.queue.len(), 1);
        assert_eq!(app.queue[0].artifact.artifact_id, "first");
        assert!(app.checked.contains("second"));
        app.previews.clear();
        app.stage(ActionKind::Quarantine);
        assert_eq!(app.queue[0].kind, ActionKind::Approve);
    }

    #[test]
    fn streamed_results_preserve_selection_and_do_not_allow_partial_decisions() {
        let mut app = app();
        app.selected_id = Some("second".into());
        app.upsert(artifact(
            "third",
            "/workspace/new/SKILL.md",
            ApprovalState::New,
        ));
        app.ensure_selection();
        assert_eq!(app.selected_id.as_deref(), Some("second"));
        app.scan_running = true;
        app.stage(ActionKind::Approve);
        assert!(app.queue.is_empty());
        app.scan_running = false;
        app.stage(ActionKind::Approve);
        assert_eq!(app.queue.len(), 1);
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert!(app.queue.is_empty());
    }

    #[test]
    fn filter_fields_compose_without_changing_approval() {
        let a = artifact("a", "/workspace/CLAUDE.md", ApprovalState::Changed);
        assert!(
            Filter::parse(
                "status:new,changed agent:claude category:instructions workspace:workspace"
            )
            .unwrap()
            .matches(&a)
        );
        assert!(!Filter::parse("status:approved").unwrap().matches(&a));
        assert!(!Filter::parse("bundle:yes").unwrap().matches(&a));
        assert!(Filter::parse("status:trusted").is_err());
        assert!(Filter::parse("policy:permissive").is_err());
    }

    #[test]
    fn stale_health_is_never_presented_as_a_running_monitor() {
        let old = serde_json::json!({"status":"watching","heartbeat_at":now().saturating_sub(100),"heartbeat_interval_secs":15,"polling_fallbacks":[]});
        assert!(monitor_label(Some(&old)).contains("stale"));
        let current = serde_json::json!({"status":"degraded","heartbeat_at":now(),"heartbeat_interval_secs":15,"polling_fallbacks":[{"root":"one"}]});
        assert_eq!(
            monitor_label(Some(&current)),
            "degraded · 1 polling fallbacks"
        );
        assert!(monitor_label(None).contains("unknown"));
    }

    #[test]
    fn quitting_a_staged_plan_requires_a_second_explicit_action() {
        let mut app = app();
        app.stage(ActionKind::Approve);
        assert!(!app.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert_eq!(app.mode, Mode::Quit);
        assert!(!app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(app.queue.len(), 1);
        assert!(!app.request_quit());
        assert!(app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn narrow_dashboard_renders_labels_and_escaped_hostile_paths() {
        let mut app = app();
        app.artifacts[0] = artifact(
            "first",
            "/workspace/evil\u{1b}]52;c;secret\u{7}\n\u{202e}CLAUDE.md",
            ApprovalState::New,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("rulescan"));
        assert!(rendered.contains("new"));
        assert!(rendered.contains("Space select"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
        assert!(!rendered.contains('\u{202e}'));
        for (width, height) in [(40, 15), (1, 1), (140, 35)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
    }
}
