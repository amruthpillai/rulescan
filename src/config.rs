use crate::model::{POLICY_VERSION, PROFILE_VERSION, digest};
use anyhow::{Context, Result, bail, ensure};
use globset::{GlobBuilder, GlobMatcher};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub scan: ScanConfig,
    pub profiles: BTreeMap<String, bool>,
    pub rules: Vec<Rule>,
    pub workspaces: Vec<Workspace>,
    pub exclusions: Vec<Exclusion>,
    pub watch: WatchConfig,
    pub analysis: AnalysisConfig,
    pub ui: UiConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            scan: ScanConfig::default(),
            profiles: BTreeMap::new(),
            rules: vec![],
            workspaces: vec![],
            exclusions: vec![],
            watch: WatchConfig::default(),
            analysis: AnalysisConfig::default(),
            ui: UiConfig::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ScanConfig {
    pub roots: Vec<PathBuf>,
    pub include_hidden: bool,
    pub respect_repository_ignores: bool,
    pub follow_directory_links: bool,
}
impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            roots: vec![],
            include_hidden: true,
            respect_repository_ignores: false,
            follow_directory_links: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub path: PathBuf,
    pub policy: WorkspacePolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePolicy {
    ApprovedContent,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    pub path: PathBuf,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AnalysisConfig {
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub color: String,
    pub keymap: BTreeMap<String, String>,
}
impl Default for UiConfig {
    fn default() -> Self {
        Self {
            color: "auto".into(),
            keymap: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WatchConfig {
    pub enabled: bool,
    pub debounce: String,
    pub reconcile_every: String,
    pub poll_every: String,
    pub default_action: String,
}
impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debounce: "500ms".into(),
            reconcile_every: "6h".into(),
            poll_every: "30s".into(),
            default_action: "notify".into(),
        }
    }
}
pub fn duration(value: &str) -> Result<Duration> {
    for (suffix, factor) in [("ms", 1), ("s", 1000), ("m", 60_000), ("h", 3_600_000)] {
        if let Some(n) = value.strip_suffix(suffix) {
            let millis = n
                .parse::<u64>()?
                .checked_mul(factor)
                .context("duration overflow")?;
            ensure!(
                (100..=7 * 24 * 3_600_000).contains(&millis),
                "duration must be between 100ms and 7d"
            );
            return Ok(Duration::from_millis(millis));
        }
    }
    bail!("duration needs ms, s, m, or h suffix")
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    #[serde(rename = "match")]
    pub matcher: MatchSpec,
    pub category: String,
    #[serde(default = "review")]
    pub on_new: String,
    #[serde(default = "review")]
    pub on_change: String,
}
fn review() -> String {
    "review".into()
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    pub basename: Option<String>,
    pub glob: Option<String>,
    pub bundle_marker: Option<String>,
    #[serde(default)]
    pub case: Case,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Case {
    #[default]
    Sensitive,
    Insensitive,
}
pub struct CompiledRule {
    pub rule: Rule,
    pub matcher: GlobMatcher,
}
impl CompiledRule {
    pub fn new(rule: Rule) -> Result<Self> {
        let spec = &rule.matcher;
        ensure!(
            [
                spec.basename.is_some(),
                spec.glob.is_some(),
                spec.bundle_marker.is_some()
            ]
            .iter()
            .filter(|x| **x)
            .count()
                == 1,
            "rule {} needs exactly one match type",
            rule.id
        );
        let pattern = spec
            .glob
            .as_ref()
            .or(spec.basename.as_ref())
            .or(spec.bundle_marker.as_ref())
            .unwrap();
        ensure!(
            !pattern.is_empty() && pattern.len() <= 4096 && !pattern.contains('\\'),
            "patterns must be 1..4096 bytes without backslashes"
        );
        if spec.glob.is_none() {
            ensure!(
                !pattern.contains('/') && !pattern.contains(['*', '?', '[', '{']),
                "basename and bundle_marker must be literal names"
            );
        }
        let matcher = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .case_insensitive(matches!(spec.case, Case::Insensitive))
            .build()?
            .compile_matcher();
        Ok(Self { rule, matcher })
    }
    pub fn matches(&self, relative: &Path) -> bool {
        if self.rule.matcher.glob.is_some() {
            self.matcher.is_match(portable(relative))
        } else {
            relative
                .file_name()
                .is_some_and(|p| self.matcher.is_match(Path::new(p)))
        }
    }
}
pub fn portable(p: &Path) -> String {
    p.components()
        .map(|s| s.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 1024 * 1024,
            "config must be a regular file under 1 MiB"
        );
        let mut bytes = Vec::new();
        // Explicit config directories may use OS aliases such as macOS /var.
        // Canonicalize only the parent; the final config entry must not be a link.
        let parent = std::fs::canonicalize(path.parent().context("configuration needs parent")?)?;
        let target = parent.join(path.file_name().context("configuration needs filename")?);
        crate::paths::open_regular(&target)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
        reject_yaml_aliases(&bytes)?;
        let config: Self =
            serde_yaml_ng::from_slice(&bytes).context("invalid YAML configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported configuration version");
        ensure!(
            self.rules.len() <= 1024
                && self.scan.roots.len() <= 256
                && self.exclusions.len() <= 4096,
            "configuration collection budget exceeded"
        );
        ensure!(
            self.scan.include_hidden
                && !self.scan.respect_repository_ignores
                && !self.scan.follow_directory_links,
            "this release requires hidden coverage, ignores disabled, and no link traversal"
        );
        ensure!(
            !self.analysis.enabled,
            "content analyzers are not enabled in this release"
        );
        ensure!(
            self.watch.default_action == "notify",
            "automatic mutation is unsupported"
        );
        ensure!(
            ["auto", "always", "never"].contains(&self.ui.color.as_str()),
            "invalid ui.color"
        );
        for key in self.profiles.keys() {
            ensure!(
                [
                    "shared_instructions",
                    "claude",
                    "codex",
                    "copilot",
                    "gemini",
                    "skills",
                    "cursor",
                    "hooks"
                ]
                .contains(&key.as_str()),
                "unknown profile: {key}"
            );
        }
        for value in [
            &self.watch.debounce,
            &self.watch.reconcile_every,
            &self.watch.poll_every,
        ] {
            duration(value)?;
        }
        let mut ids = std::collections::HashSet::new();
        for rule in &self.rules {
            ensure!(
                !rule.id.is_empty() && ids.insert(&rule.id),
                "empty or duplicate rule id"
            );
            ensure!(
                rule.on_new == "review" && rule.on_change == "review",
                "rules cannot weaken review"
            );
            ensure!(
                [
                    "instructions",
                    "agent_configuration",
                    "skill",
                    "prompt",
                    "rules"
                ]
                .contains(&rule.category.as_str()),
                "unsupported category"
            );
            CompiledRule::new(rule.clone())?;
        }
        for e in &self.exclusions {
            ensure!(
                !e.reason.is_empty() && expand(&e.path)?.is_absolute(),
                "exclusions need absolute paths and reasons"
            );
        }
        let mut keys = std::collections::HashSet::new();
        for (action, key) in &self.ui.keymap {
            ensure!(
                [
                    "approve",
                    "quarantine",
                    "undo",
                    "review",
                    "refresh",
                    "diff",
                    "help",
                    "settings",
                    "quarantine_view"
                ]
                .contains(&action.as_str()),
                "unknown key action"
            );
            ensure!(
                key.chars().count() == 1
                    && !"qjk /:frw?daxucQ".contains(key.as_str())
                    && keys.insert(key),
                "custom keys must be distinct single characters outside reserved keys"
            );
        }
        Ok(())
    }
    pub fn enabled(&self, profile: &str) -> bool {
        self.profiles.get(profile).copied().unwrap_or(true)
    }
    pub fn revision(&self) -> String {
        digest(
            &serde_json::to_vec(&(
                PROFILE_VERSION,
                POLICY_VERSION,
                digest(include_bytes!("../profiles/builtin.json")),
                &self.profiles,
                &self.rules,
            ))
            .expect("serializable configuration"),
        )
    }
    pub fn roots(&self, explicit: &[PathBuf], interactive: bool) -> Result<Vec<PathBuf>> {
        let roots = if !explicit.is_empty() {
            explicit.to_vec()
        } else if !self.scan.roots.is_empty() {
            self.scan.roots.clone()
        } else {
            vec![std::env::current_dir()?]
        };
        let mut result: Vec<PathBuf> = roots.iter().map(|r| expand(r)).collect::<Result<_>>()?;
        if interactive
            && explicit.is_empty()
            && self.scan.roots.is_empty()
            && let Some(home) = directories::BaseDirs::new()
        {
            for relative in [
                ".codex",
                ".claude",
                ".gemini",
                ".config/agents",
                ".agents/skills",
            ] {
                let p = home.home_dir().join(relative);
                if p.is_dir() && !result.contains(&p) {
                    result.push(p);
                }
            }
        }
        Ok(result)
    }
}
pub fn expand(path: &Path) -> Result<PathBuf> {
    if path == Path::new("~") || path.starts_with("~/") {
        let home = directories::BaseDirs::new().context("home directory unavailable")?;
        return Ok(home.home_dir().join(path.strip_prefix("~")?));
    }
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// Prevent YAML alias expansion before deserialization. Patterns containing `*`
/// are ordinary quoted strings; executable tags are not part of our config model.
pub fn reject_yaml_aliases(bytes: &[u8]) -> Result<()> {
    let mut quote = 0u8;
    let mut escaped = false;
    let mut comment = false;
    let mut boundary = true;
    let mut input = bytes.iter().copied().peekable();
    while let Some(b) = input.next() {
        if b == b'\n' {
            comment = false;
            if quote == 0 {
                boundary = true;
            }
            continue;
        }
        if comment {
            continue;
        }
        if quote != 0 {
            if quote == b'\'' && b == b'\'' && input.peek() == Some(&b'\'') {
                input.next();
                continue;
            }
            if quote == b'"' && b == b'\\' && !escaped {
                escaped = true;
                continue;
            }
            if b == quote && !escaped {
                quote = 0;
            }
            escaped = false;
            continue;
        }
        if b == b'#' && boundary {
            comment = true;
            continue;
        }
        if (b == b'\'' || b == b'"') && boundary {
            quote = b;
            boundary = false;
            continue;
        }
        ensure!(
            !(boundary && (b == b'&' || b == b'*')),
            "configuration YAML anchors/aliases are unsupported; quote glob patterns"
        );
        boundary = b.is_ascii_whitespace() || matches!(b, b':' | b'[' | b'{' | b',');
    }
    Ok(())
}
