use crate::{
    actions,
    config::Config,
    discovery::{self, ScanEvent},
    model::*,
    paths::Locations,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool, mpsc::SyncSender},
};

pub struct Engine {
    pub config: Config,
    pub locations: Locations,
    pub store: Store,
}
impl Engine {
    pub fn open(mut locations: Locations, config: Config) -> Result<Self> {
        config.validate()?;
        locations.init()?;
        let store = Store::open(&locations)?;
        Ok(Self {
            config,
            locations,
            store,
        })
    }
    pub fn scan(
        &self,
        roots: &[PathBuf],
        cancel: Arc<AtomicBool>,
        events: Option<&SyncSender<ScanEvent>>,
    ) -> Result<ScanResult> {
        let _lock = self.locations.lock()?;
        let mut result = discovery::scan(&self.config, &self.locations, roots, cancel, events)?;
        let old = self.store.artifacts()?;
        let old_by_id: HashMap<_, _> = old.iter().map(|a| (&a.artifact_id, a)).collect();
        let seen: HashSet<_> = result
            .artifacts
            .iter()
            .map(|a| a.artifact_id.clone())
            .collect();
        let quarantines = self.store.quarantines()?;
        self.store.conn.execute_batch("BEGIN IMMEDIATE")?;
        let persist = (|| -> Result<()> {
            for a in &mut result.artifacts {
                if let Some((approved, _)) = self.store.approval(&a.artifact_id)? {
                    a.state = if a.approval_digest.as_ref() == Some(&approved) {
                        ApprovalState::Approved
                    } else if a.observed_sha256.is_some() {
                        ApprovalState::Changed
                    } else {
                        ApprovalState::Unreadable
                    };
                }
                if let Some(previous) = old_by_id.get(&a.artifact_id) {
                    if previous.approval_digest != a.approval_digest || previous.state != a.state {
                        self.store.event(
                            "observation_changed",
                            Some(&a.artifact_id),
                            a.observed_sha256.as_deref(),
                        )?;
                    }
                } else {
                    self.store.event(
                        "discovered",
                        Some(&a.artifact_id),
                        a.observed_sha256.as_deref(),
                    )?;
                }
                self.store.save_artifact(a)?;
            }
            for mut a in old.iter().cloned() {
                if seen.contains(&a.artifact_id)
                    || !result
                        .coverage
                        .roots
                        .iter()
                        .any(|r| r.value == a.root.value)
                {
                    continue;
                }
                let path = a.path.to_path()?;
                let excluded =
                    self.locations.excluded(&path)
                        || self.config.exclusions.iter().any(|e| {
                            crate::config::expand(&e.path).is_ok_and(|p| path.starts_with(p))
                        });
                a.state = if excluded {
                    ApprovalState::Excluded
                } else if quarantines
                    .iter()
                    .any(|q| q.artifact.artifact_id == a.artifact_id && q.stage == "completed")
                    && !path.exists()
                {
                    ApprovalState::Quarantined
                } else if result.coverage.complete() && !path.exists() {
                    ApprovalState::Missing
                } else {
                    ApprovalState::Unreadable
                };
                a.allowed_next_actions = vec!["inspect".into(), "explain".into()];
                a.reasons.push(
                    "not observed in current scope; no mutation is allowed from a cached snapshot"
                        .into(),
                );
                self.store.save_artifact(&a)?;
                result.artifacts.push(a);
            }
            self.store.save_coverage(&result.coverage)?;
            Ok(())
        })();
        match persist {
            Ok(()) => self.store.conn.execute_batch("COMMIT")?,
            Err(e) => {
                self.store.conn.execute_batch("ROLLBACK")?;
                return Err(e);
            }
        }
        Ok(result)
    }
    pub fn inspect(&self, id: &str, content: bool, diff: bool) -> Result<serde_json::Value> {
        let a = self.store.artifact(id)?;
        let mut data = serde_json::json!({"artifact": a, "content_is_untrusted": true});
        if content || diff {
            let bytes = if a.category == "skill" {
                let (manifest, hash) =
                    discovery::manifest(&a.path.to_path()?, &AtomicBool::new(false))?;
                ensure!(
                    Some(hash) == a.observed_sha256,
                    "conflict: bundle changed since scan; refresh before review"
                );
                serde_json::to_vec_pretty(&manifest)?
            } else {
                let (bytes, identity) = discovery::read_snapshot(&a.path.to_path()?)?;
                ensure!(
                    Some(digest(&bytes)) == a.observed_sha256 && Some(identity) == a.identity,
                    "conflict: file changed since scan; refresh before review"
                );
                bytes
            };
            let truncated = bytes.len() > MAX_PREVIEW_BYTES;
            let current = escape(
                &String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PREVIEW_BYTES)]),
                true,
            );
            data["truncated"] = truncated.into();
            data["content"] = current.clone().into();
            data["snapshot_sha256"] = a.observed_sha256.clone().into();
            if a.category == "skill" {
                let markers: Vec<_> = std::iter::once("SKILL.md".to_owned())
                    .chain(
                        self.config
                            .rules
                            .iter()
                            .filter_map(|r| r.matcher.bundle_marker.clone()),
                    )
                    .collect();
                if let Some(entry) = a.bundle_manifest.iter().find(|e| {
                    e.entry_type == "file"
                        && e.path.to_path().is_ok_and(|p| {
                            p.components().count() == 1
                                && p.file_name().is_some_and(|n| {
                                    markers
                                        .iter()
                                        .any(|m| n.to_string_lossy().eq_ignore_ascii_case(m))
                                })
                        })
                }) {
                    let (bytes, _) =
                        discovery::read_snapshot(&a.path.to_path()?.join(entry.path.to_path()?))?;
                    ensure!(
                        Some(digest(&bytes)) == entry.sha256,
                        "conflict: skill instructions changed since preview"
                    );
                    data["entrypoint_path"] = serde_json::to_value(&entry.path)?;
                    data["entrypoint_content"] = escape(
                        &String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PREVIEW_BYTES)]),
                        true,
                    )
                    .into();
                    data["entrypoint_truncated"] = (bytes.len() > MAX_PREVIEW_BYTES).into();
                }
            }
            if diff {
                let baseline = self.store.approval(id)?.and_then(|(_, s)| s);
                data["baseline_available"] = baseline.is_some().into();
                if let Some(baseline) = baseline {
                    let before = escape(
                        &String::from_utf8_lossy(
                            &baseline[..baseline.len().min(MAX_PREVIEW_BYTES)],
                        ),
                        true,
                    );
                    data["diff"] = similar::TextDiff::configure()
                        .timeout(std::time::Duration::from_millis(200))
                        .diff_lines(&before, &current)
                        .unified_diff()
                        .context_radius(3)
                        .header("approved (untrusted)", "current (untrusted)")
                        .to_string()
                        .into();
                }
            }
        }
        Ok(data)
    }
    pub fn inspect_entry(&self, id: &str, relative: &Path) -> Result<serde_json::Value> {
        let a = self.store.artifact(id)?;
        ensure!(a.category == "skill", "requires a bundle artifact");
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid relative bundle entry"
        );
        let (manifest, hash) = discovery::manifest(&a.path.to_path()?, &AtomicBool::new(false))?;
        ensure!(
            Some(hash) == a.observed_sha256,
            "conflict: bundle changed since scan"
        );
        let native = NativePath::new(relative);
        let entry = manifest
            .iter()
            .find(|e| e.path.value == native.value && e.entry_type == "file")
            .context("unknown regular bundle entry")?;
        let (bytes, _) = discovery::read_snapshot(&a.path.to_path()?.join(relative))?;
        ensure!(
            Some(digest(&bytes)) == entry.sha256,
            "conflict: resource changed during preview"
        );
        Ok(
            serde_json::json!({"artifact_id":id,"entry":entry,"content_is_untrusted":true,"content":escape(&String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PREVIEW_BYTES)]),true),"truncated":bytes.len()>MAX_PREVIEW_BYTES}),
        )
    }
    pub fn plan(&self, kind: ActionKind, targets: &[(String, String)]) -> Result<ActionPlan> {
        let _lock = self.locations.lock()?;
        ensure!(!targets.is_empty(), "select at least one artifact");
        let mut items = Vec::new();
        let mut ids = HashSet::new();
        for (id, expected) in targets {
            ensure!(ids.insert(id), "duplicate artifact in plan");
            let a = self.store.artifact(id)?;
            let action = if kind == ActionKind::Approve {
                "approve"
            } else {
                "quarantine"
            };
            ensure!(
                a.allowed_next_actions.iter().any(|x| x == action),
                "this artifact is read-only for {action}"
            );
            ensure!(
                a.observed_sha256.as_ref() == Some(expected) && a.approval_digest.is_some(),
                "conflict: expected snapshot differs; rescan and review"
            );
            items.push(PlanItem {
                kind: kind.clone(),
                artifact: a,
                expected_sha256: expected.clone(),
                quarantine_id: None,
                destination: None,
                destination_parent_identity: None,
            });
        }
        self.save_new_plan(items)
    }
    pub fn mixed_plan(&self, items: Vec<PlanItem>) -> Result<ActionPlan> {
        let _lock = self.locations.lock()?;
        self.save_new_plan(items)
    }
    fn save_new_plan(&self, items: Vec<PlanItem>) -> Result<ActionPlan> {
        ensure!(!items.is_empty(), "empty action plan");
        for (i, a) in items.iter().enumerate() {
            let path_a = a.artifact.path.to_path()?;
            for b in items.iter().skip(i + 1) {
                let path_b = b.artifact.path.to_path()?;
                ensure!(path_a != path_b, "duplicate target in action plan");
                ensure!(
                    !(a.kind == ActionKind::Quarantine || b.kind == ActionKind::Quarantine)
                        || !(path_a.starts_with(&path_b) || path_b.starts_with(&path_a)),
                    "overlapping bundle/file mutations must be reviewed separately"
                );
            }
        }
        let plan = ActionPlan {
            plan_id: new_id(),
            created_at: now(),
            items,
            status: "pending".into(),
        };
        self.store.save_plan(&plan)?;
        Ok(plan)
    }
    pub fn restore_plan(&self, id: &str, destination: Option<&Path>) -> Result<ActionPlan> {
        let _lock = self.locations.lock()?;
        let q = self.store.quarantine(id)?;
        ensure!(
            q.stage == "completed",
            "quarantine requires recovery or was already restored"
        );
        let to = destination
            .map(crate::config::expand)
            .transpose()?
            .unwrap_or(q.artifact.path.to_path()?);
        ensure!(
            std::fs::symlink_metadata(&to).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "conflict: restore destination already exists or is inaccessible"
        );
        let parent =
            std::fs::canonicalize(to.parent().context("restore destination needs parent")?)?;
        ensure!(
            !self.locations.excluded(&parent),
            "cannot restore into private state"
        );
        let to = parent.join(to.file_name().context("missing destination filename")?);
        let parent_identity = FileIdentity::from_metadata(&std::fs::metadata(&parent)?);
        let item = PlanItem {
            kind: ActionKind::Restore,
            artifact: q.artifact,
            expected_sha256: q.payload_sha256,
            quarantine_id: Some(id.into()),
            destination: Some(NativePath::new(&to)),
            destination_parent_identity: Some(parent_identity),
        };
        self.save_new_plan(vec![item])
    }
    pub fn recover(&self) -> Result<Vec<ItemOutcome>> {
        let _lock = self.locations.lock()?;
        actions::recover(&self.store, &self.locations)
    }
    pub fn apply(&self, id: &str) -> Result<ApplyResult> {
        // Reconcile first; it acquires the same lock as scans and direct CLI actions.
        self.recover()?;
        let _lock = self.locations.lock()?;
        let mut plan = self.store.plan(id)?;
        ensure!(
            plan.status == "pending",
            "conflict: plan already applied or interrupted; create a new plan"
        );
        let roots: Vec<_> = plan
            .items
            .iter()
            .filter(|i| i.kind != ActionKind::Restore)
            .map(|i| i.artifact.root.to_path())
            .collect::<Result<_>>()?;
        let fresh = discovery::scan(
            &self.config,
            &self.locations,
            &roots,
            Arc::new(AtomicBool::new(false)),
            None,
        )?;
        let by_id: HashMap<_, _> = fresh
            .artifacts
            .into_iter()
            .map(|a| (a.artifact_id.clone(), a))
            .collect();
        let unresolved: Vec<_> = self
            .store
            .quarantines()?
            .into_iter()
            .filter(|q| !["completed", "restored", "aborted"].contains(&q.stage.as_str()))
            .collect();
        plan.status = "applying".into();
        self.store.save_plan(&plan)?;
        let mut outcomes = Vec::new();
        for item in &plan.items {
            let applied = (|| -> Result<Option<String>> {
                let target = item
                    .destination
                    .as_ref()
                    .unwrap_or(&item.artifact.path)
                    .to_path()?;
                for q in &unresolved {
                    for p in std::iter::once(&q.artifact.path).chain(q.restored_to.iter()) {
                        let p = p.to_path()?;
                        ensure!(
                            !target.starts_with(&p) && !p.starts_with(&target),
                            "conflict: unresolved recovery touches this target; retain both copies and review"
                        );
                    }
                }
                if item.kind == ActionKind::Restore {
                    return actions::restore(&self.store, &self.locations, item);
                }
                let a = by_id
                    .get(&item.artifact.artifact_id)
                    .context("conflict: target missing or root identity changed")?;
                ensure!(
                    a.observed_sha256.as_ref() == Some(&item.expected_sha256)
                        && a.approval_digest == item.artifact.approval_digest
                        && a.identity == item.artifact.identity,
                    "conflict: target changed since review"
                );
                ensure!(
                    a.approval_digest.is_some(),
                    "incomplete target cannot be approved or quarantined"
                );
                let allowed = if item.kind == ActionKind::Approve {
                    "approve"
                } else {
                    "quarantine"
                };
                ensure!(
                    a.allowed_next_actions.iter().any(|s| s == allowed),
                    "target is read-only"
                );
                verify_observation(a, &by_id, &mut HashSet::new())?;
                if item.kind == ActionKind::Approve {
                    let snapshot = if a.category == "skill" {
                        Some(serde_json::to_vec_pretty(&a.bundle_manifest)?)
                    } else {
                        let (bytes, id) = discovery::read_snapshot(&a.path.to_path()?)?;
                        ensure!(
                            Some(digest(&bytes)) == a.observed_sha256 && Some(id) == a.identity,
                            "conflict: content changed during approval"
                        );
                        Some(bytes)
                    };
                    let transaction = self.store.conn.unchecked_transaction()?;
                    self.store.approve(
                        a,
                        snapshot.as_deref().filter(|s| s.len() <= 256 * 1024),
                        "explicit_plan",
                    )?;
                    let mut approved = a.clone();
                    approved.state = ApprovalState::Approved;
                    self.store.save_artifact(&approved)?;
                    self.store.event(
                        "approved",
                        Some(&a.artifact_id),
                        a.observed_sha256.as_deref(),
                    )?;
                    transaction.commit()?;
                    Ok(None)
                } else {
                    actions::quarantine(&self.store, &self.locations, a)
                }
            })();
            outcomes.push(match applied {
                Ok(q) => ItemOutcome {
                    artifact_id: item.artifact.artifact_id.clone(),
                    status: "completed".into(),
                    detail: "requested operation completed".into(),
                    quarantine_id: q,
                },
                Err(e) => ItemOutcome {
                    artifact_id: item.artifact.artifact_id.clone(),
                    status: if e.to_string().starts_with("conflict:") {
                        "conflict"
                    } else {
                        "failed"
                    }
                    .into(),
                    detail: escape(&format!("{e:#}"), false),
                    quarantine_id: item.quarantine_id.clone(),
                },
            });
        }
        plan.status = if outcomes.iter().all(|o| o.status == "completed") {
            "completed"
        } else {
            "partial"
        }
        .into();
        self.store.save_plan(&plan)?;
        Ok(ApplyResult {
            plan_id: plan.plan_id,
            outcomes,
        })
    }
    pub fn doctor(&self) -> Result<serde_json::Value> {
        let _lock = self.locations.lock()?;
        let health: String = self
            .store
            .conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        let pending: Vec<_> = self
            .store
            .quarantines()?
            .into_iter()
            .filter(|q| !["completed", "restored", "aborted"].contains(&q.stage.as_str()))
            .collect();
        let quarantine_integrity:Vec<_>=self.store.quarantines()?.iter().filter(|q|q.stage=="completed").map(|q| {
            let verified=actions::payload_path(&self.locations,&q.quarantine_id).and_then(|p|actions::verify_payload(&p,q));
            serde_json::json!({"quarantine_id":q.quarantine_id,"valid":verified.is_ok(),"error":verified.err().map(|e|escape(&e.to_string(),false))})
        }).collect();
        Ok(
            serde_json::json!({"database":health,"quarantine_integrity":quarantine_integrity,"locations":{"state":NativePath::new(&self.locations.state),"data":NativePath::new(&self.locations.data),"config":NativePath::new(&self.locations.config)},"config_valid":true,"coordination":"exclusive per-user file lock; daemon uses the same engine","incomplete_operations":pending,"last_coverage":self.store.last_coverage()?,"native_mutation_backend":cfg!(any(target_os="macos",target_os="linux")),"limitations":["Post-write detection is not enforcement","Same-user processes can alter RuleScan state","Native release validation is required on each OS","No installed-agent loading/version adapter or semantic threat analyzer"]}),
        )
    }
}

fn verify_observation(
    a: &Artifact,
    inventory: &HashMap<String, Artifact>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    if !seen.insert(a.artifact_id.clone()) {
        return Ok(());
    }
    let root = a.root.to_path()?;
    #[cfg(unix)]
    let root_meta = crate::paths::open_dir(&root)?.metadata()?;
    #[cfg(windows)]
    let root_meta = std::fs::symlink_metadata(&root)?;
    ensure!(
        FileIdentity::from_metadata(&root_meta).same_object(&a.root_identity),
        "conflict: root changed during batch"
    );
    if a.category == "skill" {
        let (_, hash) = discovery::manifest(&a.path.to_path()?, &AtomicBool::new(false))?;
        ensure!(
            Some(hash) == a.observed_sha256,
            "conflict: bundle changed during batch"
        );
    } else {
        let (bytes, id) = discovery::read_snapshot(&a.path.to_path()?)?;
        ensure!(
            Some(digest(&bytes)) == a.observed_sha256 && Some(id) == a.identity,
            "conflict: file changed during batch"
        );
    }
    for reference in &a.references {
        let child = reference
            .artifact_id
            .as_ref()
            .and_then(|id| inventory.get(id))
            .context("conflict: unresolved dependency during batch")?;
        verify_observation(child, inventory, seen)?;
    }
    Ok(())
}
