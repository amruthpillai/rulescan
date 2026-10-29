#[cfg(unix)]
use rulescan::actions;
use rulescan::{
    config::{Config, Exclusion},
    engine::Engine,
    model::*,
    paths::Locations,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    engine: Engine,
}
impl Fixture {
    fn new() -> Self {
        Self::with_config(Config::default())
    }
    fn with_config(config: Config) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let locations = Locations::resolve(Some(&temp.path().join("private"))).unwrap();
        let engine = Engine::open(locations, config).unwrap();
        Self {
            _temp: temp,
            root,
            engine,
        }
    }
    fn write(&self, p: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn scan(&self) -> ScanResult {
        self.engine
            .scan(
                std::slice::from_ref(&self.root),
                Arc::new(AtomicBool::new(false)),
                None,
            )
            .unwrap()
    }
    fn find(&self, name: &str) -> Artifact {
        self.scan()
            .artifacts
            .into_iter()
            .find(|a| a.path.to_path().unwrap() == self.root.join(name))
            .unwrap()
    }
    #[cfg(unix)]
    fn approve(&self, a: &Artifact) {
        let p = self
            .engine
            .plan(
                ActionKind::Approve,
                &[(a.artifact_id.clone(), a.observed_sha256.clone().unwrap())],
            )
            .unwrap();
        let result = self.engine.apply(&p.plan_id).unwrap();
        assert!(result.success(), "{result:?}");
    }
    #[cfg(unix)]
    fn quarantine(&self, a: &Artifact) -> String {
        let p = self
            .engine
            .plan(
                ActionKind::Quarantine,
                &[(a.artifact_id.clone(), a.observed_sha256.clone().unwrap())],
            )
            .unwrap();
        let result = self.engine.apply(&p.plan_id).unwrap();
        assert!(result.success(), "{result:?}");
        result.outcomes[0].quarantine_id.clone().unwrap()
    }
}

#[test]
fn discovery_includes_hidden_ignored_dependencies_and_global_root_context() {
    let f = Fixture::new();
    f.write(".gitignore", "node_modules\n.hidden\n");
    for p in [
        "AGENTS.md",
        ".hidden/CLAUDE.md",
        "node_modules/dependency/AGENTS.override.md",
        ".github/instructions/test.instructions.md",
        ".cursor/rules/team.mdc",
        ".codex/config.toml",
        ".claude/rules/private.md",
    ] {
        f.write(p, "harmless");
    }
    let result = f.scan();
    assert!(result.coverage.complete(), "{:?}", result.coverage.gaps);
    assert_eq!(result.artifacts.len(), 7);
    let global = f
        .engine
        .scan(
            &[f.root.join(".codex"), f.root.join(".claude")],
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .unwrap();
    assert_eq!(global.artifacts.len(), 2);
    assert!(global.artifacts.iter().all(|a| !a.reasons.is_empty()));
}

#[cfg(windows)]
#[test]
fn windows_inventory_does_not_offer_unqualified_approval_or_mutations() {
    let f = Fixture::new();
    f.write("AGENTS.md", "review only");
    let a = f.find("AGENTS.md");
    assert!(
        a.allowed_next_actions
            .iter()
            .all(|action| action == "inspect" || action == "explain")
    );
    assert!(
        f.engine
            .plan(
                ActionKind::Approve,
                &[(a.artifact_id.clone(), a.observed_sha256.clone().unwrap())]
            )
            .is_err()
    );
    assert!(
        f.engine
            .plan(
                ActionKind::Quarantine,
                &[(a.artifact_id, a.observed_sha256.unwrap())]
            )
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn approval_is_exact_and_does_not_trust_new_content() {
    let f = Fixture::new();
    f.write("AGENTS.md", "safe one");
    let first = f.find("AGENTS.md");
    assert_eq!(first.state, ApprovalState::New);
    f.approve(&first);
    assert_eq!(f.find("AGENTS.md").state, ApprovalState::Approved);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(f.root.join("AGENTS.md"))
        .unwrap();
    let before = file.metadata().unwrap().modified().unwrap();
    f.write("AGENTS.md", "safe two");
    file.set_times(fs::FileTimes::new().set_modified(before))
        .unwrap();
    assert_eq!(f.find("AGENTS.md").state, ApprovalState::Changed);
    f.write("nested/AGENTS.md", "new");
    assert_eq!(f.find("nested/AGENTS.md").state, ApprovalState::New);
    fs::remove_file(f.root.join("AGENTS.md")).unwrap();
    assert_eq!(f.find("AGENTS.md").state, ApprovalState::Missing);
}

#[cfg(unix)]
#[test]
fn bundle_approval_covers_resources_additions_removals_and_modes() {
    let f = Fixture::new();
    f.write("skills/example/SKILL.md", "harmless skill");
    f.write("skills/example/script.sh", "echo harmless");
    let a = f.find("skills/example");
    assert_eq!(a.category, "skill");
    assert_eq!(a.bundle_manifest.len(), 2);
    f.approve(&a);
    f.write("skills/example/resource.txt", "new resource");
    let b = f.find("skills/example");
    assert_eq!(b.state, ApprovalState::Changed);
    f.approve(&b);
    fs::remove_file(f.root.join("skills/example/resource.txt")).unwrap();
    assert_eq!(f.find("skills/example").state, ApprovalState::Changed);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let a = f.find("skills/example");
        f.approve(&a);
        fs::set_permissions(
            f.root.join("skills/example/script.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(f.find("skills/example").state, ApprovalState::Changed);
    }
}

#[cfg(unix)]
#[test]
fn imports_invalidate_dependents_and_cycles_are_incomplete() {
    let f = Fixture::new();
    f.write("CLAUDE.md", "@shared.txt\n");
    f.write("shared.txt", "original");
    let a = f.find("CLAUDE.md");
    assert_eq!(a.references.len(), 1);
    f.approve(&a);
    f.write("shared.txt", "modified");
    assert_eq!(f.find("CLAUDE.md").state, ApprovalState::Changed);
    f.write("shared.txt", "@CLAUDE.md\n");
    let scan = f.scan();
    assert_eq!(scan.exit_code(), 3);
    assert!(scan.artifacts.iter().all(|a| a.approval_digest.is_none()));
}

#[cfg(unix)]
#[test]
fn imports_propagate_through_existing_candidates_regardless_of_discovery_order() {
    let f = Fixture::new();
    f.write("AGENTS.md", "@shared.txt\n");
    f.write("CLAUDE.md", "@AGENTS.md\n");
    f.write("shared.txt", "original");
    let a = f.find("CLAUDE.md");
    assert!(
        f.scan()
            .artifacts
            .iter()
            .any(|a| a.path.to_path().unwrap() == f.root.join("shared.txt"))
    );
    f.approve(&a);
    f.write("shared.txt", "changed");
    assert_eq!(f.find("CLAUDE.md").state, ApprovalState::Changed);
}

#[test]
fn outside_and_remote_imports_do_not_become_approval() {
    let f = Fixture::new();
    fs::write(f.root.parent().unwrap().join("outside.md"), "not read").unwrap();
    f.write(
        "CLAUDE.md",
        "@../outside.md\n@https://example.invalid/rules.md\n",
    );
    let scan = f.scan();
    assert_eq!(scan.artifacts.len(), 1);
    assert_eq!(scan.exit_code(), 3);
    assert!(scan.artifacts[0].approval_digest.is_none());
}

#[cfg(unix)]
#[test]
fn unresolved_link_imports_cannot_be_approved_even_when_other_targets_are_readable() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.write("CLAUDE.md", "@alias.md\n");
    f.write("real.txt", "original");
    symlink(f.root.join("real.txt"), f.root.join("alias.md")).unwrap();
    let a = f.find("CLAUDE.md");
    assert!(a.approval_digest.is_none());
    assert!(
        f.engine
            .plan(
                ActionKind::Approve,
                &[(a.artifact_id, a.observed_sha256.unwrap())]
            )
            .is_err()
    );
}

#[test]
fn exceeded_import_budget_is_never_a_partial_approval() {
    let f = Fixture::new();
    let mut imports = String::new();
    for n in 0..129 {
        let name = format!("shared-{n}.txt");
        imports.push_str(&format!("@{name}\n"));
        f.write(&name, "shared");
    }
    f.write("CLAUDE.md", imports);
    let a = f.find("CLAUDE.md");
    assert!(a.approval_digest.is_none());
    assert!(
        f.engine
            .plan(
                ActionKind::Approve,
                &[(a.artifact_id, a.observed_sha256.unwrap())]
            )
            .is_err()
    );
}

#[test]
fn skill_entrypoint_and_resources_can_be_reviewed_as_untrusted_text() {
    let f = Fixture::new();
    f.write("skills/test/SKILL.md", "review these instructions");
    f.write(
        "skills/test/scripts/action.txt",
        "untrusted resource\x1b[2J",
    );
    let a = f.find("skills/test");
    let preview = f.engine.inspect(&a.artifact_id, true, true).unwrap();
    assert_eq!(preview["entrypoint_content"], "review these instructions");
    let resource = f
        .engine
        .inspect_entry(&a.artifact_id, Path::new("scripts/action.txt"))
        .unwrap();
    let equivalent = f
        .engine
        .inspect_entry(&a.artifact_id, Path::new("scripts//action.txt"))
        .unwrap();
    assert_eq!(equivalent["content"], resource["content"]);
    #[cfg(windows)]
    assert_eq!(
        f.engine
            .inspect_entry(&a.artifact_id, Path::new(r"scripts\action.txt"))
            .unwrap()["content"],
        resource["content"]
    );
    assert_eq!(resource["content_is_untrusted"], true);
    assert!(!resource["content"].as_str().unwrap().contains('\x1b'));
    assert!(
        f.engine
            .inspect_entry(&a.artifact_id, Path::new("../outside"))
            .is_err()
    );
}

#[test]
fn yaml_aliases_are_rejected_before_expansion_but_quoted_globs_work() {
    assert!(rulescan::config::reject_yaml_aliases(b"rules: &many [*many]").is_err());
    assert!(rulescan::config::reject_yaml_aliases(b"glob: '**/AGENTS.md' # &comment\n").is_ok());
    assert!(rulescan::config::reject_yaml_aliases(b"reason: 'a '' '\nroots: &x [*x]\n").is_err());
}

#[test]
fn invisible_unicode_and_nonstandard_spaces_remain_visible_in_previews() {
    assert_eq!(
        escape("a\u{ad}\u{34f}\u{e0001}\u{a0}b", false),
        "a\\u{ad}\\u{34f}\\u{e0001}\\u{a0}b"
    );
    assert_eq!(escape("a\nb", true), "a\nb");
}

#[test]
fn custom_rules_are_literal_relative_and_explainable() {
    let config:Config=serde_yaml_ng::from_str("version: 1\nrules:\n  - id: company\n    match: {basename: TEAM_GUIDE.md, case: insensitive}\n    category: instructions\n  - id: custom\n    match: {glob: '**/.my-agent/**', case: sensitive}\n    category: agent_configuration\n").unwrap();
    let f = Fixture::with_config(config);
    f.write("deep/team_guide.MD", "hi");
    f.write(".my-agent/custom.json", "{}");
    f.write("TEAM_GUIDE.md.extra", "no");
    let scan = f.scan();
    assert_eq!(scan.artifacts.len(), 2);
    assert!(
        scan.artifacts
            .iter()
            .all(|a| a.matching_rule_ids.iter().any(|id| id.starts_with("user.")))
    );
}

#[test]
fn own_state_and_explicit_exclusions_are_visible_but_not_scanned() {
    let mut f = Fixture::new();
    f.write("visible/AGENTS.md", "hi");
    f.write("excluded/AGENTS.md", "hi");
    f.engine.config.exclusions.push(Exclusion {
        path: f.root.join("excluded"),
        reason: "test fixture".into(),
    });
    fs::write(f.engine.locations.data.join("AGENTS.md"), "private").unwrap();
    let scan = f
        .engine
        .scan(
            &[f.root.parent().unwrap().to_owned()],
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .unwrap();
    assert_eq!(scan.artifacts.len(), 1);
    assert!(scan.coverage.exclusions.len() >= 4);
    f.write("skills/SKILL.md", "bundle");
    f.write("skills/secret/AGENTS.md", "secret");
    f.engine.config.exclusions.push(Exclusion {
        path: f.root.join("skills/secret"),
        reason: "excluded".into(),
    });
    let a = f.find("skills");
    assert_eq!(a.state, ApprovalState::Unreadable);
    assert!(a.approval_digest.is_none());
    assert!(a.bundle_manifest.is_empty());
}

#[cfg(unix)]
#[test]
fn changed_profile_revision_requires_reapproval() {
    let mut f = Fixture::new();
    f.write("AGENTS.md", "same");
    let a = f.find("AGENTS.md");
    f.approve(&a);
    f.engine.config.profiles.insert("cursor".into(), false);
    assert_eq!(f.find("AGENTS.md").state, ApprovalState::Changed);
}

#[cfg(unix)]
#[test]
fn stale_plan_conflicts_without_approving_or_moving() {
    let f = Fixture::new();
    f.write("AGENTS.md", "reviewed");
    let a = f.find("AGENTS.md");
    let plan = f
        .engine
        .plan(
            ActionKind::Approve,
            &[(a.artifact_id.clone(), a.observed_sha256.unwrap())],
        )
        .unwrap();
    f.write("AGENTS.md", "unreviewed");
    let result = f.engine.apply(&plan.plan_id).unwrap();
    assert!(!result.success());
    assert_eq!(result.outcomes[0].status, "conflict");
    assert!(f.engine.store.approval(&a.artifact_id).unwrap().is_none());
    assert!(f.engine.apply(&plan.plan_id).is_err());
}

#[test]
fn inspect_refuses_bytes_that_drifted_after_scan() {
    let f = Fixture::new();
    f.write("AGENTS.md", "old");
    let a = f.find("AGENTS.md");
    f.write("AGENTS.md", "new");
    assert!(
        f.engine
            .inspect(&a.artifact_id, true, false)
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
}

#[test]
fn oversized_files_and_cancelled_scans_are_incomplete() {
    let f = Fixture::new();
    f.write("AGENTS.md", vec![0; MAX_FILE_BYTES as usize + 1]);
    let result = f.scan();
    assert_eq!(result.exit_code(), 3);
    assert_eq!(result.artifacts[0].state, ApprovalState::Unreadable);
    let result = f
        .engine
        .scan(
            std::slice::from_ref(&f.root),
            Arc::new(AtomicBool::new(true)),
            None,
        )
        .unwrap();
    assert_eq!(result.exit_code(), 130);
}

#[test]
fn mutation_lock_prevents_overlapping_actors() {
    let f = Fixture::new();
    let lock = f.engine.locations.lock().unwrap();
    assert!(f.engine.locations.lock().is_err());
    drop(lock);
    assert!(f.engine.locations.lock().is_ok());
}

#[cfg(unix)]
#[test]
fn root_replacement_never_inherits_approval() {
    let f = Fixture::new();
    f.write("AGENTS.md", "same");
    let a = f.find("AGENTS.md");
    f.approve(&a);
    fs::rename(&f.root, f.root.with_file_name("old")).unwrap();
    fs::create_dir(&f.root).unwrap();
    f.write("AGENTS.md", "same");
    let next = f.scan();
    let current = next
        .artifacts
        .iter()
        .find(|a| a.state == ApprovalState::New)
        .unwrap();
    assert_ne!(current.artifact_id, a.artifact_id);
}

#[cfg(unix)]
#[test]
fn links_are_inventory_only_and_never_escape() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = f.root.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("AGENTS.md"), "outside").unwrap();
    symlink(&outside, f.root.join("linked-directory")).unwrap();
    symlink(outside.join("AGENTS.md"), f.root.join("AGENTS.md")).unwrap();
    let result = f.scan();
    assert_eq!(result.exit_code(), 3);
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.artifacts[0].state, ApprovalState::Unreadable);
    assert_eq!(
        fs::read_to_string(outside.join("AGENTS.md")).unwrap(),
        "outside"
    );
}

#[cfg(unix)]
#[test]
fn file_quarantine_restore_preserves_bytes_modes_and_xattrs() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("AGENTS.md", "reviewed original\n");
    let source = f.root.join("AGENTS.md");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).unwrap();
    #[cfg(target_os = "macos")]
    let attribute = "com.rulescan.test";
    #[cfg(not(target_os = "macos"))]
    let attribute = "user.rulescan-test";
    xattr::set(&source, attribute, b"preserve").unwrap();
    let a = f.find("AGENTS.md");
    f.approve(&a);
    let id = f.quarantine(&a);
    assert!(!source.exists());
    let payload = actions::payload_path(&f.engine.locations, &id).unwrap();
    assert_eq!(
        fs::metadata(&payload).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        xattr::get(&payload, attribute).unwrap().unwrap(),
        b"preserve"
    );
    let plan = f.engine.restore_plan(&id, None).unwrap();
    assert!(f.engine.apply(&plan.plan_id).unwrap().success());
    assert_eq!(fs::read_to_string(&source).unwrap(), "reviewed original\n");
    assert_eq!(
        fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o751
    );
    assert_eq!(
        xattr::get(&source, attribute).unwrap().unwrap(),
        b"preserve"
    );
    assert_eq!(f.find("AGENTS.md").state, ApprovalState::New);
}

#[cfg(unix)]
#[test]
fn bundle_quarantine_restores_complete_layout_and_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("skills/test/SKILL.md", "skill");
    f.write("skills/test/scripts/run.sh", "echo test");
    fs::set_permissions(
        f.root.join("skills/test/scripts/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let a = f.find("skills/test");
    let id = f.quarantine(&a);
    assert!(!f.root.join("skills/test").exists());
    let payload = actions::payload_path(&f.engine.locations, &id).unwrap();
    assert_eq!(
        fs::metadata(payload.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    let plan = f.engine.restore_plan(&id, None).unwrap();
    assert!(f.engine.apply(&plan.plan_id).unwrap().success());
    assert_eq!(f.find("skills/test").observed_sha256, a.observed_sha256);
    assert_eq!(
        fs::metadata(f.root.join("skills/test/scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn restore_never_overwrites_even_when_destination_appears_after_plan() {
    let f = Fixture::new();
    f.write("AGENTS.md", "original");
    let a = f.find("AGENTS.md");
    let id = f.quarantine(&a);
    let plan = f.engine.restore_plan(&id, None).unwrap();
    f.write("AGENTS.md", "new occupant");
    let result = f.engine.apply(&plan.plan_id).unwrap();
    assert!(!result.success());
    assert_eq!(
        fs::read_to_string(f.root.join("AGENTS.md")).unwrap(),
        "new occupant"
    );
    assert!(
        actions::payload_path(&f.engine.locations, &id)
            .unwrap()
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn hardlinks_and_shared_settings_remain_read_only_for_quarantine() {
    let f = Fixture::new();
    f.write("AGENTS.md", "original");
    fs::hard_link(f.root.join("AGENTS.md"), f.root.join("other.txt")).unwrap();
    let a = f.find("AGENTS.md");
    let p = f
        .engine
        .plan(
            ActionKind::Quarantine,
            &[(a.artifact_id.clone(), a.observed_sha256.unwrap())],
        )
        .unwrap();
    assert!(!f.engine.apply(&p.plan_id).unwrap().success());
    assert!(f.root.join("AGENTS.md").exists());
    f.write(".claude/settings.json", "{}");
    let a = f.find(".claude/settings.json");
    assert!(
        f.engine
            .plan(
                ActionKind::Quarantine,
                &[(a.artifact_id.clone(), a.observed_sha256.unwrap())]
            )
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn recovery_reconciles_prepared_and_moved_transactions_without_deleting() {
    let f = Fixture::new();
    f.write("AGENTS.md", "original");
    let a = f.find("AGENTS.md");
    let id = new_id();
    let payload = actions::payload_path(&f.engine.locations, &id).unwrap();
    rulescan::paths::private_dir(payload.parent().unwrap()).unwrap();
    let mut q = QuarantineRecord {
        quarantine_id: id.clone(),
        artifact: a.clone(),
        payload_sha256: a.observed_sha256.clone().unwrap(),
        payload_identity: a.identity.clone().unwrap(),
        original_mode: a.identity.as_ref().unwrap().mode,
        stage: "prepared".into(),
        created_at: now(),
        restored_to: None,
    };
    f.engine.store.save_quarantine(&q).unwrap();
    let result = f.engine.recover().unwrap();
    assert_eq!(result[0].status, "completed");
    assert_eq!(f.engine.store.quarantine(&id).unwrap().stage, "aborted");
    assert!(f.root.join("AGENTS.md").exists());
    q.stage = "prepared".into();
    f.engine.store.save_quarantine(&q).unwrap();
    fs::rename(f.root.join("AGENTS.md"), &payload).unwrap();
    let result = f.engine.recover().unwrap();
    assert_eq!(result[0].status, "completed");
    assert_eq!(f.engine.store.quarantine(&id).unwrap().stage, "completed");
    assert_eq!(fs::read_to_string(payload).unwrap(), "original");
}

#[cfg(unix)]
#[test]
fn recovery_with_both_copies_keeps_both_and_requires_review() {
    let f = Fixture::new();
    f.write("AGENTS.md", "original");
    let a = f.find("AGENTS.md");
    let id = new_id();
    let payload = actions::payload_path(&f.engine.locations, &id).unwrap();
    rulescan::paths::private_dir(payload.parent().unwrap()).unwrap();
    fs::rename(f.root.join("AGENTS.md"), &payload).unwrap();
    f.write("AGENTS.md", "replacement");
    let q = QuarantineRecord {
        quarantine_id: id.clone(),
        artifact: a.clone(),
        payload_sha256: a.observed_sha256.unwrap(),
        payload_identity: a.identity.clone().unwrap(),
        original_mode: a.identity.unwrap().mode,
        stage: "prepared".into(),
        created_at: now(),
        restored_to: None,
    };
    f.engine.store.save_quarantine(&q).unwrap();
    assert_eq!(f.engine.recover().unwrap()[0].status, "failed");
    assert!(payload.exists());
    assert_eq!(
        fs::read_to_string(f.root.join("AGENTS.md")).unwrap(),
        "replacement"
    );
    assert_eq!(
        f.engine.store.quarantine(&id).unwrap().stage,
        "needs_review"
    );
}

#[cfg(unix)]
#[test]
fn terminal_quarantine_record_and_approval_revocation_commit_together() {
    let f = Fixture::new();
    f.write("AGENTS.md", "original");
    let a = f.find("AGENTS.md");
    f.approve(&a);
    f.engine.store.conn.execute_batch("CREATE TRIGGER fail_revoke BEFORE DELETE ON approvals BEGIN SELECT RAISE(FAIL,'injected storage failure'); END;").unwrap();
    let plan = f
        .engine
        .plan(
            ActionKind::Quarantine,
            &[(a.artifact_id.clone(), a.observed_sha256.clone().unwrap())],
        )
        .unwrap();
    assert!(!f.engine.apply(&plan.plan_id).unwrap().success());
    let q = f.engine.store.quarantines().unwrap().pop().unwrap();
    assert_eq!(q.stage, "moved");
    assert!(f.engine.store.approval(&a.artifact_id).unwrap().is_some());
    assert!(
        actions::payload_path(&f.engine.locations, &q.quarantine_id)
            .unwrap()
            .exists()
    );
    f.engine
        .store
        .conn
        .execute_batch("DROP TRIGGER fail_revoke;")
        .unwrap();
    assert_eq!(f.engine.recover().unwrap()[0].status, "completed");
    assert_eq!(
        f.engine.store.quarantine(&q.quarantine_id).unwrap().stage,
        "completed"
    );
    assert!(f.engine.store.approval(&a.artifact_id).unwrap().is_none());
}

#[test]
fn config_rejects_duplicate_unknown_and_weakening_fields() {
    for text in [
        "version: 1\nversion: 1\n",
        "version: 1\nunknown: true\n",
        "version: 1\nscan:\n  follow_directory_links: true\n",
        "version: 1\nwatch:\n  default_action: quarantine\n",
    ] {
        let parsed = serde_yaml_ng::from_str::<Config>(text);
        assert!(
            parsed.as_ref().map_or(true, |c| c.validate().is_err()),
            "{text}"
        );
    }
}

proptest::proptest! {
    #[test]
    fn terminal_output_has_no_control_characters(s in ".{0,1024}") {
        let escaped=escape(&s,false);proptest::prop_assert!(!escaped.chars().any(char::is_control));
    }
    #[test]
    fn native_paths_round_trip_without_case_or_unicode_normalization(s in "[a-zA-Z0-9 _.-]{1,80}") {
        let path=Path::new(&s);let encoded=NativePath::new(path);proptest::prop_assert_eq!(encoded.to_path().unwrap(),path);
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_and_hostile_filenames_have_lossless_safe_output() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let native = OsString::from_vec(b"bad-\xff-\x1b]52;secret\x07\n".to_vec());
    assert_eq!(
        NativePath::new(Path::new(&native)).to_path().unwrap(),
        Path::new(&native)
    );
    // APFS rejects invalid UTF-8; lossless serialization is still tested above.
    #[cfg(target_os = "macos")]
    let dirname = OsString::from("bad-\u{202e}-\x1b]52;secret\x07\n");
    #[cfg(not(target_os = "macos"))]
    let dirname = native;
    let f = Fixture::new();
    let dir = f.root.join(dirname);
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("AGENTS.md"), "text\x1b]52;c;secret\x07\u{202e}end").unwrap();
    let a = f.scan().artifacts.pop().unwrap();
    assert_eq!(a.path.to_path().unwrap(), dir.join("AGENTS.md"));
    assert!(!a.path.display.contains(['\x1b', '\n']));
    let data = f.engine.inspect(&a.artifact_id, true, false).unwrap();
    let content = data["content"].as_str().unwrap();
    assert!(!content.contains(['\x1b', '\u{202e}']));
}
