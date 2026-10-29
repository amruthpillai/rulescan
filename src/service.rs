//! Per-user startup integration for `rulescan watch --foreground`.
//! macOS: LaunchAgent. Linux: systemd user unit. Windows: current-user Scheduled Task.
//! Only files carrying RuleScan's marker are ever replaced or removed, and
//! uninstall never touches configuration, state, approvals, or quarantine.
use crate::{
    cli::ServiceCommand,
    engine::Engine,
    model::*,
    paths::Locations,
    watch::{self, MonitorHealth},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const LABEL: &str = "dev.rulescan.watch";
pub const UNIT: &str = "rulescan-watch.service";
pub const TASK: &str = r"\RuleScan\Watch";
const MARKER: &str = "Managed by RuleScan service install";
const MAX_OUTPUT: usize = 4096;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Launchd,
    Systemd,
    TaskScheduler,
}
impl Platform {
    fn current() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Self::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Systemd)
        } else if cfg!(windows) {
            Ok(Self::TaskScheduler)
        } else {
            bail!(
                "startup integration is unsupported on this platform; use rulescan watch --foreground"
            )
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd_user_agent",
            Self::Systemd => "systemd_user_unit",
            Self::TaskScheduler => "scheduled_task_current_user",
        }
    }
}

struct Integration {
    path: PathBuf,
    program: String,
    args: Vec<String>,
    content: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Existing {
    Absent,
    Ours(Vec<u8>),
    Foreign,
}

pub fn execute(
    engine: &Engine,
    command: &ServiceCommand,
    state_override: Option<&Path>,
) -> Result<Value> {
    let _lock = crate::paths::lock(&engine.locations.state.join("service.lock"))?;
    let platform = Platform::current()?;
    match command {
        ServiceCommand::Install { apply } => install(engine, platform, state_override, *apply),
        ServiceCommand::Start => {
            start(engine, platform)?;
            status(engine, platform, state_override)
        }
        ServiceCommand::Stop => {
            stop(engine, platform)?;
            status(engine, platform, state_override)
        }
        ServiceCommand::Status => status(engine, platform, state_override),
        ServiceCommand::Logs => logs(engine, platform),
        ServiceCommand::Uninstall => uninstall(engine, platform),
    }
}

/// Truncate the LaunchAgent log once it exceeds the bound. launchd appends, so
/// truncation in place is safe. Called from the watcher heartbeat.
pub fn trim_log(locations: &Locations) {
    let path = log_path(locations);
    if fs::symlink_metadata(&path)
        .is_ok_and(|m| m.is_file() && m.len() > watch::MAX_SERVICE_LOG_BYTES)
    {
        let mut o = fs::OpenOptions::new();
        o.write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.custom_flags(libc::O_NOFOLLOW);
        }
        if let Ok(f) = o.open(&path) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if !f.metadata().is_ok_and(|m| {
                    m.is_file() && m.nlink() == 1 && m.uid() == unsafe { libc::geteuid() }
                }) {
                    return;
                }
            }
            let _ = f.set_len(0);
        }
    }
}
fn log_path(locations: &Locations) -> PathBuf {
    locations.state.join("service").join("watch.log")
}

fn target_path(engine: &Engine, platform: Platform) -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("home directory unavailable")?;
    Ok(match platform {
        Platform::Launchd => base
            .home_dir()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
        Platform::Systemd => base.config_dir().join("systemd/user").join(UNIT),
        // Task definition source kept under private state; the task itself lives in Task Scheduler.
        Platform::TaskScheduler => engine
            .locations
            .state
            .join("service")
            .join("rulescan-watch.xml"),
    })
}

fn integration(
    engine: &Engine,
    platform: Platform,
    state_override: Option<&Path>,
) -> Result<Integration> {
    ensure!(
        !engine.config.scan.roots.is_empty(),
        InvocationError(format!(
            "service install requires scan.roots in {}; a background watcher never falls back to its working directory",
            engine.locations.config.display()
        ))
    );
    let exe = fs::canonicalize(std::env::current_exe()?)
        .context("cannot resolve the RuleScan executable")?;
    let program = utf8(&exe)?;
    let config = utf8(&crate::config::expand(&engine.locations.config)?)?;
    let mut args: Vec<String> = ["--no-input", "--format", "text", "--config", &config]
        .map(String::from)
        .into();
    if let Some(state) = state_override {
        args.extend(["--state-dir".into(), utf8(&crate::config::expand(state)?)?]);
    }
    args.extend(["watch".into(), "--foreground".into()]);
    let state = utf8(&engine.locations.state)?;
    let content = match platform {
        Platform::Launchd => render_launchd(
            &program,
            &args,
            &state,
            &utf8(&log_path(&engine.locations))?,
        ),
        Platform::Systemd => render_systemd(&program, &args)?,
        Platform::TaskScheduler => render_task(&program, &args, &state, &windows_user())?,
    };
    Ok(Integration {
        path: target_path(engine, platform)?,
        program,
        args,
        content,
    })
}

fn utf8(p: &Path) -> Result<String> {
    let s = p
        .to_str()
        .context("startup integration requires valid UTF-8 paths")?;
    ensure!(
        !s.chars().any(char::is_control),
        "startup integration paths must not contain control characters"
    );
    Ok(s.to_owned())
}

fn install(
    engine: &Engine,
    platform: Platform,
    state_override: Option<&Path>,
    apply: bool,
) -> Result<Value> {
    let spec = integration(engine, platform, state_override)?;
    let existing = classify(&spec.path)?;
    let bytes = encode(platform, &spec.content);
    let mut out = json!({
        "applied": false,
        "integration": {"type": platform.name(), "path": NativePath::new(&spec.path), "label": label(platform)},
        "program": spec.program, "arguments": spec.args, "content": spec.content,
        "roots": engine.config.scan.roots.iter().map(|r| NativePath::new(r)).collect::<Vec<_>>(),
        "scope": "current user only; no elevation, password, or lingering",
        "existing": match &existing { Existing::Absent => "absent", Existing::Ours(b) if *b == bytes => "identical", Existing::Ours(_) => "update_requires_reinstall", Existing::Foreign => "conflict" },
        "note": "Detects changes after they are written; this is not enforcement. Moving the executable requires reinstalling.",
    });
    if !apply {
        out["next"] =
            "rerun with `rulescan service install --apply` to write and register this integration"
                .into();
        return Ok(out);
    }
    ensure!(
        existing != Existing::Foreign,
        "conflict: {} exists and was not created by RuleScan; refusing to overwrite it",
        spec.path.display()
    );
    ensure!(
        !matches!(&existing,Existing::Ours(b) if *b!=bytes),
        "conflict: installed integration differs; explicitly uninstall then reinstall to update its executable/config path (state is retained)"
    );
    if platform == Platform::TaskScheduler {
        ensure!(
            task_query_xml().is_none_or(|xml| xml.contains(MARKER)),
            "conflict: scheduled task {TASK} exists and was not created by RuleScan"
        );
    }
    if platform == Platform::Launchd {
        crate::paths::private_dir(&engine.locations.state.join("service"))?;
    }
    if platform == Platform::TaskScheduler {
        crate::paths::private_dir(spec.path.parent().context("missing parent")?)?;
    }
    if !matches!(&existing, Existing::Ours(b) if *b == bytes) {
        write_owned(&spec.path, &bytes)?;
    }
    let path = utf8(&spec.path)?;
    let steps = match platform {
        Platform::Launchd => {
            let mut steps = vec![];
            if launchd_loaded() {
                steps.push(require(launchctl(&["bootout", &launchd_service()]))?);
            }
            steps.push(require(launchctl(&[
                "bootstrap",
                &launchd_domain(),
                &path,
            ]))?);
            steps
        }
        Platform::Systemd => vec![
            require(systemctl(&["daemon-reload"]))?,
            require(systemctl(&["enable", "--now", UNIT]))?,
        ],
        Platform::TaskScheduler => vec![
            require(schtasks(&["/Create", "/TN", TASK, "/XML", &path, "/F"]))?,
            require(schtasks(&["/Run", "/TN", TASK]))?,
        ],
    };
    out["applied"] = true.into();
    out["native"] = steps.into();
    Ok(out)
}

fn start(engine: &Engine, platform: Platform) -> Result<()> {
    let path = target_path(engine, platform)?;
    ensure!(
        matches!(classify(&path)?, Existing::Ours(_)),
        "service start requires `rulescan service install --apply` first"
    );
    match platform {
        Platform::Launchd if launchd_loaded() => {
            require(launchctl(&["kickstart", &launchd_service()]))?;
        }
        Platform::Launchd => {
            require(launchctl(&["bootstrap", &launchd_domain(), &utf8(&path)?]))?;
        }
        Platform::Systemd => {
            require(systemctl(&["start", UNIT]))?;
        }
        Platform::TaskScheduler => {
            require(schtasks(&["/Run", "/TN", TASK]))?;
        }
    }
    Ok(())
}

fn stop(engine: &Engine, platform: Platform) -> Result<()> {
    ensure!(
        matches!(
            classify(&target_path(engine, platform)?)?,
            Existing::Ours(_)
        ),
        "service stop requires a RuleScan-installed integration"
    );
    match platform {
        // bootout unloads until the next login or `service start`; KeepAlive would otherwise restart it.
        Platform::Launchd => {
            if launchd_loaded() {
                require(launchctl(&["bootout", &launchd_service()]))?;
            }
        }
        Platform::Systemd => {
            require(systemctl(&["stop", UNIT]))?;
        }
        Platform::TaskScheduler => {
            let _ = schtasks(&["/End", "/TN", TASK])?;
        }
    }
    Ok(())
}

fn uninstall(engine: &Engine, platform: Platform) -> Result<Value> {
    let path = target_path(engine, platform)?;
    let existing = classify(&path)?;
    ensure!(
        existing != Existing::Foreign,
        "conflict: {} was not created by RuleScan; refusing to remove it",
        path.display()
    );
    if existing == Existing::Absent {
        return Ok(
            json!({"removed":false,"note":"No owned integration file; no native registration was changed","preserved":{"config":NativePath::new(&engine.locations.config),"state":NativePath::new(&engine.locations.state),"data_and_quarantine":NativePath::new(&engine.locations.data)}}),
        );
    }
    let mut steps = vec![];
    match platform {
        Platform::Launchd => {
            if launchd_loaded() {
                steps.push(require(launchctl(&["bootout", &launchd_service()]))?);
            }
        }
        Platform::Systemd => {
            if existing != Existing::Absent {
                steps.push(systemctl(&["disable", "--now", UNIT])?.to_json());
            }
        }
        Platform::TaskScheduler => {
            if let Some(xml) = task_query_xml() {
                ensure!(
                    xml.contains(MARKER),
                    "conflict: scheduled task {TASK} was not created by RuleScan"
                );
                let _ = schtasks(&["/End", "/TN", TASK])?;
                steps.push(require(schtasks(&["/Delete", "/TN", TASK, "/F"]))?);
            }
        }
    }
    let removed = existing != Existing::Absent;
    if removed {
        fs::remove_file(&path)?;
    }
    if platform == Platform::Systemd && removed {
        steps.push(systemctl(&["daemon-reload"])?.to_json());
    }
    Ok(json!({
        "removed": removed, "integration": {"type": platform.name(), "path": NativePath::new(&path), "label": label(platform)}, "native": steps,
        "preserved": {"config": NativePath::new(&engine.locations.config), "state": NativePath::new(&engine.locations.state), "data_and_quarantine": NativePath::new(&engine.locations.data)},
    }))
}

fn status(engine: &Engine, platform: Platform, state_override: Option<&Path>) -> Result<Value> {
    let path = target_path(engine, platform)?;
    let existing = classify(&path)?;
    let installed = matches!(existing, Existing::Ours(_));
    let up_to_date = match (&existing, integration(engine, platform, state_override)) {
        (Existing::Ours(b), Ok(spec)) => Some(*b == encode(platform, &spec.content)),
        _ => None,
    };
    let (running, native) = if !installed {
        (false, Value::Null)
    } else {
        match platform {
            Platform::Launchd => match launchctl(&["print", &launchd_service()]) {
                Ok(o) => (
                    o.ok && o.stdout.lines().any(|l| l.trim() == "state = running"),
                    o.to_json(),
                ),
                Err(e) => (false, json!({"error": escape(&format!("{e:#}"), false)})),
            },
            Platform::Systemd => match systemctl(&["is-active", UNIT]) {
                Ok(o) => (o.stdout.trim() == "active", o.to_json()),
                Err(e) => (false, json!({"error": escape(&format!("{e:#}"), false)})),
            },
            // Status text is localized; English "Running" is matched until native validation.
            Platform::TaskScheduler => {
                match schtasks(&["/Query", "/TN", TASK, "/FO", "LIST", "/V"]) {
                    Ok(o) => (
                        o.ok && o
                            .stdout
                            .lines()
                            .any(|l| l.starts_with("Status:") && l.contains("Running")),
                        o.to_json(),
                    ),
                    Err(e) => (false, json!({"error": escape(&format!("{e:#}"), false)})),
                }
            }
        }
    };
    let health: Option<MonitorHealth> = engine.store.setting(watch::HEALTH_KEY)?;
    let fresh = health.as_ref().is_some_and(|h| {
        h.status != "stopped"
            && now().saturating_sub(h.heartbeat_at) <= 3 * h.heartbeat_interval_secs.max(1)
    });
    let healthy = running
        && fresh
        && health.as_ref().is_some_and(|h| {
            h.status == "watching"
                && h.last_complete_reconcile.is_some()
                && h.polling_fallbacks.is_empty()
        });
    Ok(json!({
        "integration": {"type": platform.name(), "path": NativePath::new(&path), "label": label(platform), "ownership": match existing { Existing::Absent => "absent", Existing::Ours(_) => "rulescan", Existing::Foreign => "conflict" }},
        "installed": installed, "running": running, "healthy": healthy, "heartbeat_fresh": fresh, "up_to_date": up_to_date,
        "executable": std::env::current_exe().ok().map(|p| NativePath::new(&p)), "version": env!("CARGO_PKG_VERSION"),
        "user": current_user(), "health": health, "native": native,
        "definitions": {"installed": "RuleScan-owned startup integration exists", "running": "native supervisor reports the process running", "healthy": "running, recent heartbeat, complete reconciliation, no polling fallbacks"},
    }))
}

fn logs(engine: &Engine, platform: Platform) -> Result<Value> {
    // Audit metadata only: ids, kinds, digests and times. No file content or paths.
    let mut stmt = engine.store.conn.prepare("SELECT sequence,created_at,kind,artifact_id,digest FROM events ORDER BY sequence DESC LIMIT 200")?;
    let mut events: Vec<Value> = stmt.query_map([], |r| Ok(json!({
        "sequence": r.get::<_, i64>(0)?, "created_at": r.get::<_, i64>(1)?, "kind": escape(&r.get::<_, String>(2)?, false),
        "artifact_id": r.get::<_, Option<String>>(3)?, "sha256": r.get::<_, Option<String>>(4)?,
    })))?.collect::<rusqlite::Result<_>>()?;
    events.reverse();
    let native = match platform {
        Platform::Launchd => {
            let p = log_path(&engine.locations);
            json!({"path": NativePath::new(&p), "bytes": fs::metadata(&p).map(|m| m.len()).ok(), "bound_bytes": watch::MAX_SERVICE_LOG_BYTES})
        }
        Platform::Systemd => json!({"journal": format!("journalctl --user -u {UNIT}")}),
        Platform::TaskScheduler => {
            json!({"note": "Scheduled Task output is not captured; use the audit events above"})
        }
    };
    Ok(
        json!({"events": events, "health": engine.store.setting::<MonitorHealth>(watch::HEALTH_KEY)?, "native_log": native}),
    )
}

fn label(platform: Platform) -> &'static str {
    match platform {
        Platform::Launchd => LABEL,
        Platform::Systemd => UNIT,
        Platform::TaskScheduler => TASK,
    }
}

fn classify(path: &Path) -> Result<Existing> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Absent),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() || crate::paths::is_link(&meta) || meta.len() > 1024 * 1024 {
        return Ok(Existing::Foreign);
    }
    let bytes = fs::read(path)?;
    Ok(if has_marker(&bytes) {
        Existing::Ours(bytes)
    } else {
        Existing::Foreign
    })
}

fn has_marker(bytes: &[u8]) -> bool {
    if String::from_utf8_lossy(bytes).contains(MARKER) {
        return true;
    }
    let words: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&words).contains(MARKER)
}

/// Task Scheduler expects UTF-16 XML; the other formats are UTF-8.
fn encode(platform: Platform, content: &str) -> Vec<u8> {
    if platform != Platform::TaskScheduler {
        return content.as_bytes().to_vec();
    }
    [0xFF, 0xFE]
        .into_iter()
        .chain(content.encode_utf16().flat_map(u16::to_le_bytes))
        .collect()
}

fn write_owned(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("integration path needs a parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.rulescan-{}.tmp",
        path.file_name()
            .context("missing file name")?
            .to_string_lossy(),
        std::process::id()
    ));
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o644).custom_flags(libc::O_NOFOLLOW);
    }
    let written = (|| -> Result<()> {
        let mut f = o.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    {
        let directory = crate::paths::open_dir(&fs::canonicalize(parent)?)?;
        if let Err(error) = crate::actions::rename_exclusive(
            &directory,
            tmp.file_name().unwrap(),
            &directory,
            path.file_name().unwrap(),
        ) {
            let _ = fs::remove_file(&tmp);
            return Err(error.context(
                "conflict: service definition appeared during install; nothing replaced",
            ));
        }
    }
    #[cfg(windows)]
    {
        if let Err(error) = fs::hard_link(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(error.into());
        }
        fs::remove_file(&tmp)?;
    }
    let _ = crate::paths::sync_dir(parent);
    Ok(())
}

// ---- templates ----

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_launchd(program: &str, args: &[String], state: &str, log: &str) -> String {
    let argv: String = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- {MARKER}. Remove with `rulescan service uninstall`. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{argv}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>30</integer>
	<key>ProcessType</key>
	<string>Background</string>
	<key>LowPriorityIO</key>
	<true/>
	<key>Nice</key>
	<integer>10</integer>
	<key>WorkingDirectory</key>
	<string>{state}</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        state = xml_escape(state),
        log = xml_escape(log)
    )
}

/// Quote one ExecStart word: systemd expands `%` specifiers and `$` variables
/// and honors C-style backslash escapes inside double quotes.
pub fn systemd_quote(s: &str) -> Result<String> {
    ensure!(
        !s.chars().any(char::is_control),
        "systemd arguments must not contain control characters"
    );
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

fn render_systemd(program: &str, args: &[String]) -> Result<String> {
    let exec = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(systemd_quote)
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "# {MARKER}. Remove with `rulescan service uninstall`.
[Unit]
Description=RuleScan agent instruction change monitor
StartLimitIntervalSec=600
StartLimitBurst=5

[Service]
Type=simple
ExecStart={exec}
WorkingDirectory=~
Restart=on-failure
RestartSec=30
StandardOutput=journal
StandardError=journal
Nice=10
IOSchedulingClass=idle
NoNewPrivileges=yes

[Install]
WantedBy=default.target
"
    ))
}

/// Quote one argument for CommandLineToArgvW / the MSVC runtime parser.
pub fn windows_quote(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '"']) {
        return s.into();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in s.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

fn render_task(program: &str, args: &[String], workdir: &str, user: &str) -> Result<String> {
    ensure!(
        !user.is_empty() && !user.chars().any(char::is_control),
        "current Windows user is unknown"
    );
    let arguments = args
        .iter()
        .map(|a| windows_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{MARKER}. RuleScan change monitor; remove with rulescan service uninstall.</Description>
    <URI>{task}</URI>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>5</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{program}</Command>
      <Arguments>{arguments}</Arguments>
      <WorkingDirectory>{workdir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        task = xml_escape(TASK),
        user = xml_escape(user),
        program = xml_escape(program),
        arguments = xml_escape(&arguments),
        workdir = xml_escape(workdir)
    ))
}

fn windows_user() -> String {
    let name = std::env::var("USERNAME").unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() && !name.is_empty() => format!("{d}\\{name}"),
        _ => name,
    }
}

fn current_user() -> Value {
    #[cfg(unix)]
    {
        json!({"uid": unsafe { libc::getuid() }})
    }
    #[cfg(not(unix))]
    {
        json!({"name": escape(&windows_user(), false)})
    }
}

// ---- native commands: argument vectors only, never a shell ----

struct Output {
    program: String,
    args: Vec<String>,
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}
impl Output {
    fn to_json(&self) -> Value {
        json!({"command": self.program, "arguments": self.args, "ok": self.ok, "code": self.code, "stdout": self.stdout, "stderr": self.stderr})
    }
}

fn require(result: Result<Output>) -> Result<Value> {
    let o = result?;
    ensure!(
        o.ok,
        "native service command failed: {} {} (exit {:?}): {}",
        o.program,
        o.args.join(" "),
        o.code,
        o.stderr.trim()
    );
    Ok(o.to_json())
}

fn run(program: &str, args: &[&str]) -> Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = pipe {
                let _ = p.take(MAX_OUTPUT as u64 * 4).read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if started.elapsed() > COMMAND_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let clean = |b: Vec<u8>| {
        let s = escape(&String::from_utf8_lossy(&b), true);
        s.chars().take(MAX_OUTPUT).collect::<String>()
    };
    Ok(Output {
        program: program.into(),
        args: args.iter().map(|a| escape(a, false)).collect(),
        ok: status.is_some_and(|s| s.success()),
        code: status.and_then(|s| s.code()),
        stdout: clean(out.join().unwrap_or_default()),
        stderr: if status.is_none() {
            "timed out".into()
        } else {
            clean(err.join().unwrap_or_default())
        },
    })
}

fn uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}
fn launchd_domain() -> String {
    format!("gui/{}", uid())
}
fn launchd_service() -> String {
    format!("gui/{}/{LABEL}", uid())
}
fn launchctl(args: &[&str]) -> Result<Output> {
    run("/bin/launchctl", args)
}
fn launchd_loaded() -> bool {
    launchctl(&["print", &launchd_service()]).is_ok_and(|o| o.ok)
}
fn systemctl(args: &[&str]) -> Result<Output> {
    let program = ["/usr/bin/systemctl", "/bin/systemctl"].into_iter().find(|p| Path::new(p).is_file())
        .context("systemd user manager unavailable; run rulescan watch --foreground or schedule rulescan scan")?;
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    run(program, &full)
}
fn schtasks(args: &[&str]) -> Result<Output> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    run(&utf8(&root.join("System32").join("schtasks.exe"))?, args)
}
fn task_query_xml() -> Option<String> {
    schtasks(&["/Query", "/TN", TASK, "/XML"])
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn args() -> Vec<String> {
        [
            "--no-input",
            "--config",
            "/Users/a b/c&d.yaml",
            "watch",
            "--foreground",
        ]
        .map(String::from)
        .into()
    }

    #[test]
    fn escaping() {
        assert_eq!(xml_escape(r#"<a&"b'>"#), "&lt;a&amp;&quot;b&apos;&gt;");
        assert_eq!(
            systemd_quote(r#"/p a/$HOME%h\"q"#).unwrap(),
            r#""/p a/$$HOME%%h\\\"q""#
        );
        assert!(systemd_quote("a\nExecStartPre=/bin/evil").is_err());
        assert_eq!(windows_quote("plain"), "plain");
        assert_eq!(
            windows_quote(r"C:\Program Files\x\"),
            r#""C:\Program Files\x\\""#
        );
        assert_eq!(windows_quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(windows_quote(""), "\"\"");
        assert!(utf8(Path::new("/tmp/a\nb")).is_err());
    }

    #[test]
    fn launchd_template() {
        let p = render_launchd("/opt/rule scan", &args(), "/s<t>", "/s/log");
        for want in [
            "<string>/opt/rule scan</string>",
            "<string>/Users/a b/c&amp;d.yaml</string>",
            "<key>RunAtLoad</key>\n\t<true/>",
            "<key>ThrottleInterval</key>",
            "<string>/s&lt;t&gt;</string>",
            MARKER,
            LABEL,
        ] {
            assert!(p.contains(want), "missing {want}");
        }
        assert!(!p.contains("/bin/sh") && !p.contains("<key>Program</key>"));
    }

    #[test]
    fn systemd_template() {
        let u = render_systemd("/opt/rulescan", &args()).unwrap();
        assert!(u.contains(r#"ExecStart="/opt/rulescan" "--no-input" "--config" "/Users/a b/c&d.yaml" "watch" "--foreground""#));
        for want in [
            "Restart=on-failure",
            "StartLimitBurst=5",
            "StandardOutput=journal",
            "WantedBy=default.target",
            MARKER,
        ] {
            assert!(u.contains(want));
        }
        assert!(!u.to_lowercase().contains("linger"));
    }

    #[test]
    fn task_template() {
        let t = render_task(
            r"C:\Program Files\RuleScan\rulescan.exe",
            &args(),
            r"C:\s",
            r"DOM\u&ser",
        )
        .unwrap();
        for want in [
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<RunLevel>LeastPrivilege</RunLevel>",
            "<LogonType>InteractiveToken</LogonType>",
            r"<UserId>DOM\u&amp;ser</UserId>",
            "&quot;/Users/a b/c&amp;d.yaml&quot;",
            MARKER,
        ] {
            assert!(t.contains(want), "missing {want}");
        }
        assert!(!t.contains("<Password>") && !t.contains("HighestAvailable"));
        assert!(has_marker(&encode(Platform::TaskScheduler, &t)));
    }

    #[test]
    fn ownership_classification() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.plist");
        assert_eq!(classify(&p).unwrap(), Existing::Absent);
        fs::write(&p, "someone else's agent").unwrap();
        assert_eq!(classify(&p).unwrap(), Existing::Foreign);
        fs::write(&p, format!("<!-- {MARKER} -->")).unwrap();
        assert!(matches!(classify(&p).unwrap(), Existing::Ours(_)));
        #[cfg(unix)]
        {
            let l = tmp.path().join("link.plist");
            std::os::unix::fs::symlink(&p, &l).unwrap();
            assert_eq!(
                classify(&l).unwrap(),
                Existing::Foreign,
                "links are never followed or replaced"
            );
        }
        let original = fs::read(&p).unwrap();
        assert!(write_owned(&p, b"new").is_err());
        assert_eq!(
            fs::read(&p).unwrap(),
            original,
            "concurrent/existing definitions are never overwritten"
        );
        fs::remove_file(&p).unwrap();
        write_owned(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }

    fn engine(dir: &Path, roots: Vec<PathBuf>) -> Engine {
        let mut config = Config::default();
        config.scan.roots = roots;
        Engine::open(Locations::resolve(Some(&dir.join("rs"))).unwrap(), config).unwrap()
    }

    #[test]
    fn install_requires_configured_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path(), vec![]);
        let err = execute(&e, &ServiceCommand::Install { apply: false }, None).unwrap_err();
        assert!(format!("{err:#}").contains("requires scan.roots"));
    }

    #[test]
    fn install_without_apply_is_preview_only() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine(tmp.path(), vec![tmp.path().to_owned()]);
        let platform = Platform::current().unwrap();
        let target = target_path(&e, platform).unwrap();
        let before = fs::read(&target).ok();
        let state_dir = tmp.path().join("rs");
        let v = execute(
            &e,
            &ServiceCommand::Install { apply: false },
            Some(&state_dir),
        )
        .unwrap();
        assert_eq!(v["applied"], false);
        assert!(v["content"].as_str().unwrap().contains(MARKER));
        let argv: Vec<&str> = v["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(argv.ends_with(&["watch", "--foreground"]) && argv.contains(&"--state-dir"));
        assert!(Path::new(v["program"].as_str().unwrap()).is_absolute());
        assert_eq!(
            fs::read(&target).ok(),
            before,
            "preview never writes the integration"
        );
        assert!(!e.locations.state.join("service").exists());
    }
}
