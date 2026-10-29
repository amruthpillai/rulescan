use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(state: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rulescan"))
        .current_dir(cwd)
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}
fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[cfg(unix)]
#[test]
fn headless_full_approval_flow_has_stable_envelopes_and_no_prompts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let state = temp.path().join("private");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("AGENTS.md"), "harmless").unwrap();
    let scan = run(
        &state,
        &root,
        &["--format", "json", "scan", "--root", ".", "--no-input"],
    );
    assert_eq!(scan.status.code(), Some(1));
    let scan = json(&scan);
    assert_eq!(scan["schema_version"], 1);
    assert_eq!(scan["completion"], "completed");
    let a = &scan["data"][0];
    let id = a["artifact_id"].as_str().unwrap();
    let hash = a["observed_sha256"].as_str().unwrap();
    assert!(a.get("content").is_none());
    let plan = run(
        &state,
        &root,
        &[
            "plan",
            "approve",
            "--id",
            id,
            "--expected-sha256",
            hash,
            "--no-input",
        ],
    );
    assert!(plan.status.success());
    let plan = json(&plan);
    let plan_id = plan["data"]["plan_id"].as_str().unwrap();
    let check = run(&state, &root, &["check", "--workspace", ".", "--no-input"]);
    assert_eq!(check.status.code(), Some(1), "planning must not approve");
    let apply = run(&state, &root, &["apply", plan_id, "--no-input"]);
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stdout)
    );
    let check = run(&state, &root, &["check", "--workspace", ".", "--no-input"]);
    assert_eq!(check.status.code(), Some(0));
    assert_eq!(json(&check)["data"][0]["state"], "approved");
    fs::write(root.join("AGENTS.md"), "changed").unwrap();
    let check = run(&state, &root, &["verify", "--workspace", ".", "--no-input"]);
    assert_eq!(check.status.code(), Some(1));
    assert_eq!(json(&check)["data"][0]["state"], "changed");
}

#[test]
fn base_command_in_a_pipe_scans_only_current_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let state = temp.path().join("private");
    let result = run(&state, &root, &[]);
    assert!(result.status.success());
    let data = json(&result);
    assert_eq!(data["operation"], "summary");
    assert_eq!(data["coverage"]["roots"].as_array().unwrap().len(), 1);
    assert!(!result.stdout.contains(&27));
}

#[test]
fn coverage_failure_takes_precedence_over_findings() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    fs::write(temp.path().join("AGENTS.md"), "harmless").unwrap();
    let result = run(
        &state,
        temp.path(),
        &[
            "scan", "--root", ".", "--root", "missing", "--format", "json",
        ],
    );
    assert_eq!(result.status.code(), Some(3));
    let data = json(&result);
    assert_eq!(data["completion"], "incomplete");
    assert!(!data["coverage"]["gaps"].as_array().unwrap().is_empty());
}

#[test]
fn invalid_config_reports_machine_error_without_scanning() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("private");
    let config = temp.path().join("config.yaml");
    fs::write(&config, "version: 1\nprofiles:\n  made_up: true\n").unwrap();
    let output = run(
        &state,
        temp.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "--format",
            "json",
            "scan",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json(&output)["completion"], "failed");
    assert!(!state.exists());
}

#[test]
fn explicit_configuration_loads_from_native_temporary_paths_and_missing_paths_fail() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let config = temp.path().join("config.yaml");
    fs::write(
        &config,
        format!(
            "version: 1\nscan:\n  roots: [{}]\n",
            serde_json::to_string(&root).unwrap()
        ),
    )
    .unwrap();
    let output = run(
        &temp.path().join("private"),
        temp.path(),
        &["--config", config.to_str().unwrap(), "scan"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        json(&output)["coverage"]["roots"].as_array().unwrap().len(),
        1
    );
    let output = run(
        &temp.path().join("other-private"),
        temp.path(),
        &["--config", "missing.yaml", "scan"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!temp.path().join("other-private").exists());
    let fresh = temp.path().join("config-private/fresh.yaml");
    let output = run(
        &temp.path().join("no-state"),
        temp.path(),
        &["--config", fresh.to_str().unwrap(), "config", "init"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(fresh.is_file());
    assert!(!temp.path().join("no-state").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let public = temp.path().join("public-config");
        fs::create_dir(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
        let denied = public.join("config.yaml");
        let output = run(
            &temp.path().join("no-state"),
            temp.path(),
            &["--config", denied.to_str().unwrap(), "config", "init"],
        );
        assert_eq!(output.status.code(), Some(5));
        assert!(!denied.exists());
    }
}

#[test]
fn schema_and_completion_commands_do_not_create_state() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("private");
    let output = run(&state, temp.path(), &["schema", "--kind", "config"]);
    assert!(output.status.success());
    assert!(json(&output)["properties"]["version"].is_object());
    assert!(!state.exists());
    let output = run(&state, temp.path(), &["completions", "bash"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("rulescan"));
    assert!(!state.exists());
}

#[test]
fn invalid_arguments_produce_a_json_invocation_error() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(
        &temp.path().join("private"),
        temp.path(),
        &["scan", "--unknown-flag", "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(2));
    let value = json(&output);
    assert_eq!(value["operation"], "invocation");
    assert_eq!(value["completion"], "failed");
}

#[test]
fn identifiers_cannot_change_error_codes_through_their_text() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(
        &temp.path().join("private"),
        temp.path(),
        &["inspect", "conflict: invalid config"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json(&output)["completion"], "failed");
}

#[cfg(unix)]
#[test]
fn run_preflight_blocks_new_files_and_passes_literal_arguments_and_exit_status() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let state = temp.path().join("private");
    fs::write(root.join("AGENTS.md"), "harmless").unwrap();
    let blocked = run(
        &state,
        &root,
        &[
            "run",
            "--workspace",
            ".",
            "--",
            "/bin/echo",
            "should not run",
        ],
    );
    assert_eq!(blocked.status.code(), Some(125));
    assert!(blocked.stdout.is_empty());
    fs::remove_file(root.join("AGENTS.md")).unwrap();
    // A missing unapproved candidate is retained for review; use an independent empty workspace.
    let clean = temp.path().join("clean");
    fs::create_dir(&clean).unwrap();
    let result = run(
        &state,
        &clean,
        &[
            "run",
            "--workspace",
            ".",
            "--",
            "/bin/echo",
            "$(not-executed)",
            "a;b",
        ],
    );
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8_lossy(&result.stdout),
        "$(not-executed) a;b\n"
    );
    let result = run(
        &state,
        &clean,
        &["run", "--workspace", ".", "--", "/usr/bin/false"],
    );
    assert_eq!(result.status.code(), Some(1));
}
