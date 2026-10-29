use crate::{config::Config, engine::Engine, model::*, paths::Locations};
use anyhow::{Context, Result, ensure};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use schemars::JsonSchema;
use serde::Serialize;
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Debug, Parser)]
#[command(
    name = "rulescan",
    version,
    about = "Review and monitor local AI agent instructions",
    long_about = "RuleScan inventories local agent instructions, reviews exact-content snapshots, and detects changes. It does not prove instructions are safe or constrain an agent. Run without a subcommand for the keyboard dashboard."
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        help = "Override all private state/data locations (use a local disk)"
    )]
    pub state_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub format: Format,
    #[arg(
        long,
        global = true,
        help = "Never open a TUI or prompt; this never grants approval"
    )]
    pub no_input: bool,
    #[arg(
        long,
        global = true,
        help = "Linear accessible review when attached to a terminal"
    )]
    pub plain: bool,
    #[arg(long, global = true, help = "Disable UI colors (also honors NO_COLOR)")]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Format {
    Auto,
    Text,
    Json,
    Jsonl,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    Scan {
        #[arg(long)]
        root: Vec<PathBuf>,
    },
    Findings {
        #[command(subcommand)]
        command: FindingsCommand,
    },
    Inspect {
        artifact_id: String,
        #[arg(long)]
        content: bool,
        #[arg(long)]
        diff: bool,
        #[arg(
            long,
            requires = "content",
            help = "Preview a regular file relative to a skill bundle"
        )]
        entry: Option<PathBuf>,
    },
    Explain {
        artifact_id: String,
    },
    Verify {
        #[arg(long)]
        workspace: PathBuf,
    },
    Check {
        #[arg(long)]
        workspace: PathBuf,
    },
    Plan {
        #[command(subcommand)]
        command: PlanCommand,
    },
    Apply {
        plan_id: String,
    },
    Quarantine {
        #[command(subcommand)]
        command: QuarantineCommand,
    },
    Restore {
        quarantine_id: String,
        #[arg(long)]
        to: Option<PathBuf>,
    },
    Recover,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Patterns {
        #[command(subcommand)]
        command: PatternCommand,
    },
    Schema {
        #[arg(long, value_enum, default_value = "output")]
        kind: SchemaKind,
    },
    Completions {
        shell: clap_complete::Shell,
    },
    Watch {
        #[arg(long, required = true)]
        foreground: bool,
        #[arg(long)]
        root: Vec<PathBuf>,
        #[arg(long, help = "Reconcile once and exit")]
        once: bool,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Doctor,
    Run {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(last = true, required = true)]
        executable: Vec<std::ffi::OsString>,
    },
}
#[derive(Debug, Subcommand)]
pub enum FindingsCommand {
    List {
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
    },
}
#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    Approve {
        #[arg(long, required = true)]
        id: Vec<String>,
        #[arg(long, required = true)]
        expected_sha256: Vec<String>,
    },
    Quarantine {
        #[arg(long, required = true)]
        id: Vec<String>,
        #[arg(long, required = true)]
        expected_sha256: Vec<String>,
    },
    Show {
        plan_id: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum QuarantineCommand {
    List,
}
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Validate,
    Show,
    Init,
}
#[derive(Debug, Subcommand)]
pub enum PatternCommand {
    Test {
        #[arg(long)]
        path: PathBuf,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SchemaKind {
    Output,
    Config,
    Plan,
    Quarantine,
    Commands,
    Event,
}
#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    Install {
        #[arg(long, help = "Install the previewed user startup integration")]
        apply: bool,
    },
    Start,
    Stop,
    Status,
    Logs,
    Uninstall,
}

impl Cli {
    pub fn output_format(&self) -> Format {
        if self.format == Format::Auto {
            if io::stdout().is_terminal() {
                Format::Text
            } else {
                Format::Json
            }
        } else {
            self.format
        }
    }
    fn operation(&self) -> &'static str {
        match &self.command {
            None => "dashboard",
            Some(Command::Scan { .. }) => "scan",
            Some(Command::Findings { .. }) => "findings.list",
            Some(Command::Inspect { .. }) => "inspect",
            Some(Command::Explain { .. }) => "explain",
            Some(Command::Verify { .. }) => "verify",
            Some(Command::Check { .. }) => "check",
            Some(Command::Plan { .. }) => "plan",
            Some(Command::Apply { .. }) => "apply",
            Some(Command::Quarantine { .. }) => "quarantine.list",
            Some(Command::Restore { .. }) => "restore.plan",
            Some(Command::Recover) => "recover",
            Some(Command::Config { .. }) => "config",
            Some(Command::Patterns { .. }) => "patterns.test",
            Some(Command::Schema { .. }) => "schema",
            Some(Command::Completions { .. }) => "completions",
            Some(Command::Watch { .. }) => "watch",
            Some(Command::Service { .. }) => "service",
            Some(Command::Doctor) => "doctor",
            Some(Command::Run { .. }) => "run",
        }
    }
}
pub fn run(cli: Cli) -> i32 {
    match execute(&cli) {
        Ok(code) => code,
        Err(e) => {
            let message = escape(&format!("{e:#}"), false);
            let code = if e.downcast_ref::<InvocationError>().is_some() {
                2
            } else if e.to_string().starts_with("conflict:") {
                4
            } else {
                5
            };
            let envelope: Envelope<serde_json::Value> = Envelope {
                schema_version: 1,
                request_id: new_id(),
                operation: cli.operation().into(),
                completion: "failed".into(),
                coverage: None,
                profile_version: PROFILE_VERSION.into(),
                policy_version: POLICY_VERSION.into(),
                data: None,
                errors: vec![message.clone()],
            };
            if cli.output_format() == Format::Text {
                eprintln!("RuleScan: {message}");
            } else {
                let _ = write_json(&envelope, false);
            }
            code
        }
    }
}
fn execute(cli: &Cli) -> Result<i32> {
    if let Some(Command::Completions { shell }) = cli.command.as_ref() {
        clap_complete::generate(*shell, &mut Cli::command(), "rulescan", &mut io::stdout());
        return Ok(0);
    }
    if let Some(Command::Schema { kind }) = cli.command.as_ref() {
        write_json(&schema(*kind), true)?;
        return Ok(0);
    }
    let mut locations = Locations::resolve(cli.state_dir.as_deref())?;
    if let Some(path) = &cli.config {
        locations.config = crate::config::expand(path)?;
        ensure!(
            matches!(
                cli.command,
                Some(Command::Config {
                    command: ConfigCommand::Init
                })
            ) || std::fs::symlink_metadata(&locations.config).is_ok(),
            InvocationError("explicit configuration file is missing or inaccessible".into())
        );
    }
    let config = Config::load(&locations.config)
        .map_err(|e| InvocationError(format!("configuration validation failed: {e:#}")))?;
    if let Some(Command::Config { command }) = &cli.command {
        match command {
            ConfigCommand::Validate => emit(
                cli,
                "config.validate",
                serde_json::json!({"valid":true,"path":NativePath::new(&locations.config)}),
                None,
                true,
            )?,
            ConfigCommand::Show => emit(cli, "config.show", &config, None, true)?,
            ConfigCommand::Init => {
                write_config_new(&locations.config, &config)?;
                emit(
                    cli,
                    "config.init",
                    serde_json::json!({"path":NativePath::new(&locations.config)}),
                    None,
                    true,
                )?;
            }
        }
        return Ok(0);
    }
    if let Some(Command::Patterns {
        command: PatternCommand::Test { path },
    }) = &cli.command
    {
        emit(
            cli,
            "patterns.test",
            crate::discovery::pattern_test(&config, path)
                .map_err(|e| InvocationError(e.to_string()))?,
            None,
            true,
        )?;
        return Ok(0);
    }
    let engine = Engine::open(locations, config)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.store(true, std::sync::atomic::Ordering::Relaxed))
        .context("cannot install cancellation handler")?;
    match &cli.command {
        None => {
            let interactive = !cli.no_input
                && cli.format == Format::Auto
                && io::stdin().is_terminal()
                && io::stdout().is_terminal();
            let roots = engine.config.roots(&[], interactive)?;
            if interactive && !cli.plain {
                return crate::tui::run(engine, roots, cli.no_color, cancel);
            }
            let scan = engine.scan(&roots, cancel, None)?;
            if interactive && cli.plain {
                return plain_review(&engine, scan);
            }
            emit_scan(cli, "summary", &scan)?;
            if cli.output_format() == Format::Text {
                eprintln!(
                    "Run rulescan in a terminal to review, or use plan/apply. No content has been approved."
                );
            }
            Ok(scan.exit_code())
        }
        Some(Command::Scan { root }) => {
            let scan = engine.scan(&engine.config.roots(root, false)?, cancel, None)?;
            emit_scan(cli, "scan", &scan)?;
            Ok(scan.exit_code())
        }
        Some(Command::Verify { workspace } | Command::Check { workspace }) => {
            let scan = engine.scan(std::slice::from_ref(workspace), cancel, None)?;
            emit_scan(cli, cli.operation(), &scan)?;
            Ok(scan.exit_code())
        }
        Some(Command::Findings {
            command: FindingsCommand::List { status },
        }) => {
            ensure!(
                status.iter().all(|s| [
                    "new",
                    "approved",
                    "changed",
                    "missing",
                    "quarantined",
                    "unreadable",
                    "excluded"
                ]
                .contains(&s.as_str())),
                InvocationError("invalid finding status".into())
            );
            let items: Vec<_> = engine
                .store
                .artifacts()?
                .into_iter()
                .filter(|a| status.is_empty() || status.iter().any(|s| s == a.state.label()))
                .collect();
            emit(
                cli,
                "findings.list",
                items,
                engine.store.last_coverage()?,
                true,
            )?;
            Ok(0)
        }
        Some(Command::Inspect {
            artifact_id,
            content,
            diff,
            entry,
        }) => {
            let data = if let Some(entry) = entry {
                engine.inspect_entry(artifact_id, entry)?
            } else {
                engine.inspect(artifact_id, *content, *diff)?
            };
            emit(cli, "inspect", data, None, true)?;
            Ok(0)
        }
        Some(Command::Explain { artifact_id }) => {
            let a = engine.store.artifact(artifact_id)?;
            emit(
                cli,
                "explain",
                serde_json::json!({"artifact":a,"loading":"Candidate matching only; installed agent/version-specific loading has not been verified","policy":"Exact bytes, path, root identity, executable bits, references, bundle manifest, profile and policy revision require explicit approval","analysis":"No semantic safety verdict is produced"}),
                None,
                true,
            )?;
            Ok(0)
        }
        Some(Command::Plan { command }) => {
            let plan = match command {
                PlanCommand::Show { plan_id } => engine.store.plan(plan_id)?,
                PlanCommand::Approve {
                    id,
                    expected_sha256,
                }
                | PlanCommand::Quarantine {
                    id,
                    expected_sha256,
                } => {
                    ensure!(
                        id.len() == expected_sha256.len(),
                        InvocationError("requires one --expected-sha256 for each --id".into())
                    );
                    ensure!(
                        expected_sha256
                            .iter()
                            .all(|h| h.len() == 64 && hex::decode(h).is_ok()),
                        InvocationError("invalid SHA-256 digest".into())
                    );
                    let kind = if matches!(command, PlanCommand::Approve { .. }) {
                        ActionKind::Approve
                    } else {
                        ActionKind::Quarantine
                    };
                    engine.plan(
                        kind,
                        &id.iter()
                            .cloned()
                            .zip(expected_sha256.iter().cloned())
                            .collect::<Vec<_>>(),
                    )?
                }
            };
            emit(cli, "plan", plan, None, true)?;
            Ok(0)
        }
        Some(Command::Apply { plan_id }) => {
            let result = engine.apply(plan_id)?;
            let code = if result.success() {
                0
            } else if result.outcomes.iter().any(|o| o.status == "conflict") {
                4
            } else {
                5
            };
            emit(cli, "apply", &result, None, result.success())?;
            Ok(code)
        }
        Some(Command::Quarantine { .. }) => {
            emit(
                cli,
                "quarantine.list",
                engine.store.quarantines()?,
                None,
                true,
            )?;
            Ok(0)
        }
        Some(Command::Restore { quarantine_id, to }) => {
            emit(
                cli,
                "restore.plan",
                engine.restore_plan(quarantine_id, to.as_deref())?,
                None,
                true,
            )?;
            Ok(0)
        }
        Some(Command::Recover) => {
            let results = engine.recover()?;
            let ok = results.iter().all(|r| r.status == "completed");
            emit(cli, "recover", results, None, ok)?;
            Ok(if ok { 0 } else { 5 })
        }
        Some(Command::Doctor) => {
            let data = engine.doctor()?;
            let ok = data["quarantine_integrity"]
                .as_array()
                .is_some_and(|items| items.iter().all(|i| i["valid"] == true))
                && data["database"] == "ok"
                && data["incomplete_operations"]
                    .as_array()
                    .is_some_and(|a| a.is_empty());
            emit(cli, "doctor", data, None, ok)?;
            Ok(if ok { 0 } else { 5 })
        }
        Some(Command::Watch { root, once, .. }) => crate::watch::run(
            &engine,
            &engine.config.roots(root, false)?,
            cancel,
            *once,
            cli.output_format(),
        ),
        Some(Command::Service { command }) => {
            let value = crate::service::execute(&engine, command, cli.state_dir.as_deref())?;
            emit(cli, "service", value, None, true)?;
            Ok(0)
        }
        Some(Command::Run {
            workspace,
            executable,
        }) => {
            let scan = engine.scan(std::slice::from_ref(workspace), cancel, None)?;
            if scan.exit_code() != 0 {
                eprintln!(
                    "RuleScan preflight failed (exit 125): {} actionable items, {} coverage gaps. Review with rulescan.",
                    scan.artifacts
                        .iter()
                        .filter(|a| a.state.actionable())
                        .count(),
                    scan.coverage.gaps.len()
                );
                return Ok(125);
            }
            let program = resolve_executable(&executable[0])?;
            ensure!(
                std::fs::canonicalize(&program)?
                    != std::fs::canonicalize(std::env::current_exe()?)?,
                "run wrapper must not recurse into RuleScan"
            );
            let mut command = std::process::Command::new(program);
            command
                .args(&executable[1..])
                .current_dir(crate::config::expand(workspace)?);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                Err(command.exec().into())
            }
            #[cfg(windows)]
            {
                let status = command.status()?;
                Ok(status.code().unwrap_or(130))
            }
        }
        _ => unreachable!("early commands handled before opening state"),
    }
}
pub fn write_config_new(path: &Path, config: &Config) -> Result<()> {
    let parent = path.parent().context("configuration needs a parent")?;
    crate::paths::private_dir(parent)?;
    let parent = std::fs::canonicalize(parent)?;
    let target = parent.join(path.file_name().context("configuration needs filename")?);
    let mut f = crate::paths::private_file(&target, true)
        .context("configuration exists; refusing to overwrite it")?;
    f.write_all(serde_yaml_ng::to_string(config)?.as_bytes())?;
    f.sync_all()?;
    crate::paths::sync_dir(&parent)
}
fn resolve_executable(name: &std::ffi::OsStr) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 || path.is_absolute() {
        return Ok(path.to_owned());
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = candidate.with_extension("exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!("executable not found in PATH")
}
pub fn emit<T: Serialize + JsonSchema>(
    cli: &Cli,
    operation: &str,
    data: T,
    coverage: Option<Coverage>,
    complete: bool,
) -> Result<()> {
    let e = Envelope::new(operation, data, coverage, complete);
    write_json(&e, cli.output_format() == Format::Text)
}
fn emit_scan(cli: &Cli, operation: &str, result: &ScanResult) -> Result<()> {
    if cli.output_format() == Format::Text {
        let mut out = io::stdout().lock();
        writeln!(
            out,
            "RuleScan · {} artifacts · {} entries · {} coverage gaps",
            result.artifacts.len(),
            result.coverage.entries_visited,
            result.coverage.gaps.len()
        )?;
        for a in &result.artifacts {
            writeln!(
                out,
                "{:<12} {:<20} {}",
                a.state.label(),
                a.category,
                a.path.display
            )?;
        }
        for gap in &result.coverage.gaps {
            writeln!(out, "coverage gap: {} — {}", gap.path.display, gap.reason)?;
        }
        for e in &result.coverage.exclusions {
            writeln!(
                out,
                "excluded: {} — {}",
                e.path.display,
                escape(&e.reason, false)
            )?;
        }
        writeln!(
            out,
            "Candidate discovery is not a safety verdict. Review with rulescan inspect <id>."
        )?;
        Ok(())
    } else {
        emit(
            cli,
            operation,
            &result.artifacts,
            Some(result.coverage.clone()),
            result.coverage.complete(),
        )
    }
}
fn write_json<T: Serialize>(value: &T, pretty: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if pretty {
        serde_json::to_writer_pretty(&mut out, value)?;
    } else {
        serde_json::to_writer(&mut out, value)?;
    }
    writeln!(out)?;
    Ok(())
}
pub fn schema(kind: SchemaKind) -> serde_json::Value {
    match kind {
        SchemaKind::Config => serde_json::to_value(schemars::schema_for!(Config)).unwrap(),
        SchemaKind::Output => {
            serde_json::to_value(schemars::schema_for!(Envelope<Vec<Artifact>>)).unwrap()
        }
        SchemaKind::Plan => {
            serde_json::to_value(schemars::schema_for!(Envelope<ActionPlan>)).unwrap()
        }
        SchemaKind::Quarantine => {
            serde_json::to_value(schemars::schema_for!(Envelope<Vec<QuarantineRecord>>)).unwrap()
        }
        SchemaKind::Commands => command_metadata(&Cli::command()),
        SchemaKind::Event => {
            serde_json::to_value(schemars::schema_for!(crate::watch::WatchEvent)).unwrap()
        }
    }
}
fn command_metadata(command: &clap::Command) -> serde_json::Value {
    serde_json::json!({"name":command.get_name(),"about":command.get_about().map(|s|s.to_string()),"arguments":command.get_arguments().map(|a|serde_json::json!({"id":a.get_id().as_str(),"long":a.get_long(),"required":a.is_required_set()})).collect::<Vec<_>>(),"subcommands":command.get_subcommands().map(command_metadata).collect::<Vec<_>>()})
}
fn plain_review(engine: &Engine, scan: ScanResult) -> Result<i32> {
    println!(
        "RuleScan · {} artifacts · {} coverage gaps",
        scan.artifacts.len(),
        scan.coverage.gaps.len()
    );
    for root in &scan.coverage.roots {
        println!("Root: {}", root.display);
    }
    for gap in &scan.coverage.gaps {
        println!("Coverage gap: {} — {}", gap.path.display, gap.reason);
    }
    let mut staged = Vec::new();
    for a in scan.artifacts.iter().filter(|a| {
        a.state.actionable()
            && a.allowed_next_actions
                .iter()
                .any(|s| s == "approve" || s == "quarantine")
    }) {
        let inspected = engine.inspect(&a.artifact_id, true, true)?;
        if let Some(entrypoint) = inspected["entrypoint_content"].as_str() {
            println!("Skill instructions (untrusted):\n{entrypoint}");
        }
        println!(
            "\n{} [{}]\n{}",
            a.path.display,
            a.state.label(),
            inspected["content"].as_str().unwrap_or("")
        );
        print!("[a] stage approve · [x] stage quarantine · [Enter] skip · [q] finish: ");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Ok(scan.exit_code());
        }
        let kind = match line.trim() {
            "a" => ActionKind::Approve,
            "x" => ActionKind::Quarantine,
            "q" => break,
            _ => continue,
        };
        let action = if kind == ActionKind::Approve {
            "approve"
        } else {
            "quarantine"
        };
        if !a.allowed_next_actions.iter().any(|s| s == action) {
            println!("This action is read-only for this artifact.");
            continue;
        }
        staged.push(PlanItem {
            kind,
            artifact: a.clone(),
            expected_sha256: a.observed_sha256.clone().unwrap(),
            quarantine_id: None,
            destination: None,
            destination_parent_identity: None,
        });
    }
    if staged.is_empty() {
        return Ok(scan.exit_code());
    }
    let plan = engine.mixed_plan(staged)?;
    println!("\nReview plan {}:", plan.plan_id);
    for i in &plan.items {
        println!(
            "{:?} {} SHA-256 {}",
            i.kind, i.artifact.path.display, i.expected_sha256
        );
    }
    print!(
        "Type apply to apply these {} decisions, or Enter to leave them pending: ",
        plan.items.len()
    );
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if line.trim() == "apply" {
        let result = engine.apply(&plan.plan_id)?;
        write_json(&result, true)?;
        return Ok(if result.success() { 0 } else { 4 });
    }
    Ok(scan.exit_code())
}
