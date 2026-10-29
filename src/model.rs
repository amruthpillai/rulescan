use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub const SCHEMA_VERSION: u32 = 1;
pub const PROFILE_VERSION: &str = "builtin-1";
pub const POLICY_VERSION: &str = "approved-content-1";
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 64 * 1024;
pub const MAX_BUNDLE_ENTRIES: usize = 10_000;
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct InvocationError(pub String);
impl std::fmt::Display for InvocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InvocationError {}

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// All text entering a terminal passes through this function. Newlines are allowed
/// only in content, never in paths or diagnostics.
pub fn escape(text: &str, multiline: bool) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c == '\n' && multiline {
            out.push(c);
        } else if c.is_control()
            || (c.is_whitespace() && c != ' ')
            || matches!(c, '\u{00ad}' | '\u{034f}' | '\u{061c}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}' | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}' | '\u{ffa0}' | '\u{fff0}'..='\u{fffb}' | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}')
        {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativePath {
    pub encoding: String,
    pub value: String,
    pub display: String,
}
impl NativePath {
    pub fn new(path: &Path) -> Self {
        #[cfg(unix)]
        let (encoding, value) = {
            use std::os::unix::ffi::OsStrExt;
            ("unix_bytes_hex", hex::encode(path.as_os_str().as_bytes()))
        };
        #[cfg(windows)]
        let (encoding, value) = {
            use std::os::windows::ffi::OsStrExt;
            let bytes: Vec<u8> = path
                .as_os_str()
                .encode_wide()
                .flat_map(u16::to_le_bytes)
                .collect();
            ("windows_utf16le_hex", hex::encode(bytes))
        };
        Self {
            encoding: encoding.into(),
            value,
            display: escape(&path.to_string_lossy(), false),
        }
    }
    pub fn to_path(&self) -> anyhow::Result<std::path::PathBuf> {
        let bytes = hex::decode(&self.value)?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            anyhow::ensure!(self.encoding == "unix_bytes_hex", "foreign path encoding");
            Ok(std::ffi::OsString::from_vec(bytes).into())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            anyhow::ensure!(
                self.encoding == "windows_utf16le_hex" && bytes.len() % 2 == 0,
                "foreign path encoding"
            );
            let words: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Ok(std::ffi::OsString::from_wide(&words).into())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume: u64,
    pub file: u64,
    pub links: u64,
    pub mode: u32,
    pub size: u64,
    pub modified_ns: u128,
}
impl FileIdentity {
    pub fn from_metadata(m: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (volume, file, links, mode) = {
            use std::os::unix::fs::MetadataExt;
            (m.dev(), m.ino(), m.nlink(), m.mode() & 0o7777)
        };
        // Windows mutations are deliberately read-only until native handle and ADS tests pass.
        #[cfg(windows)]
        let (volume, file, links, mode) = (0, 0, 1, u32::from(m.permissions().readonly()));
        Self {
            volume,
            file,
            links,
            mode,
            size: m.len(),
            modified_ns: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos()),
        }
    }
    pub fn same_object(&self, other: &Self) -> bool {
        self.volume == other.volume && self.file == other.file
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    New,
    Approved,
    Changed,
    Missing,
    Quarantined,
    Unreadable,
    Excluded,
}
impl ApprovalState {
    pub fn actionable(self) -> bool {
        !matches!(self, Self::Approved | Self::Quarantined | Self::Excluded)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Approved => "approved",
            Self::Changed => "changed",
            Self::Missing => "missing",
            Self::Quarantined => "quarantined",
            Self::Unreadable => "unreadable",
            Self::Excluded => "excluded",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Reference {
    pub path: NativePath,
    pub artifact_id: Option<String>,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ManifestEntry {
    pub path: NativePath,
    pub entry_type: String,
    pub sha256: Option<String>,
    pub executable_mode: u32,
    pub mode: u32,
    pub link_target: Option<NativePath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Artifact {
    pub artifact_id: String,
    pub root_id: String,
    pub root: NativePath,
    pub root_identity: FileIdentity,
    pub path: NativePath,
    pub relative_path: NativePath,
    pub category: String,
    pub profiles: Vec<String>,
    pub matching_rule_ids: Vec<String>,
    pub reasons: Vec<String>,
    pub observed_sha256: Option<String>,
    pub approval_digest: Option<String>,
    pub identity: Option<FileIdentity>,
    pub bundle_manifest: Vec<ManifestEntry>,
    pub references: Vec<Reference>,
    pub state: ApprovalState,
    pub profile_version: String,
    pub policy_version: String,
    pub allowed_next_actions: Vec<String>,
    pub observed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct Coverage {
    pub roots: Vec<NativePath>,
    pub entries_visited: u64,
    pub gaps: Vec<CoverageGap>,
    pub exclusions: Vec<CoverageGap>,
    pub cancelled: bool,
}
impl Coverage {
    pub fn complete(&self) -> bool {
        self.gaps.is_empty() && !self.cancelled
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CoverageGap {
    pub path: NativePath,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScanResult {
    pub artifacts: Vec<Artifact>,
    pub coverage: Coverage,
}
impl ScanResult {
    pub fn exit_code(&self) -> i32 {
        if self.coverage.cancelled {
            130
        } else if !self.coverage.complete() {
            3
        } else if self.artifacts.iter().any(|a| a.state.actionable()) {
            1
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Approve,
    Quarantine,
    Restore,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanItem {
    pub kind: ActionKind,
    pub artifact: Artifact,
    pub expected_sha256: String,
    pub quarantine_id: Option<String>,
    pub destination: Option<NativePath>,
    pub destination_parent_identity: Option<FileIdentity>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionPlan {
    pub plan_id: String,
    pub created_at: u64,
    pub items: Vec<PlanItem>,
    pub status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ItemOutcome {
    pub artifact_id: String,
    pub status: String,
    pub detail: String,
    pub quarantine_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApplyResult {
    pub plan_id: String,
    pub outcomes: Vec<ItemOutcome>,
}
impl ApplyResult {
    pub fn success(&self) -> bool {
        self.outcomes.iter().all(|o| o.status == "completed")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuarantineRecord {
    pub quarantine_id: String,
    pub artifact: Artifact,
    pub payload_sha256: String,
    pub payload_identity: FileIdentity,
    pub original_mode: u32,
    pub stage: String,
    pub created_at: u64,
    pub restored_to: Option<NativePath>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Envelope<T: JsonSchema + Serialize> {
    pub schema_version: u32,
    pub request_id: String,
    pub operation: String,
    pub completion: String,
    pub coverage: Option<Coverage>,
    pub profile_version: String,
    pub policy_version: String,
    pub data: Option<T>,
    pub errors: Vec<String>,
}
impl<T: JsonSchema + Serialize> Envelope<T> {
    pub fn new(operation: &str, data: T, coverage: Option<Coverage>, complete: bool) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            request_id: new_id(),
            operation: operation.into(),
            completion: if complete { "completed" } else { "incomplete" }.into(),
            coverage,
            profile_version: PROFILE_VERSION.into(),
            policy_version: POLICY_VERSION.into(),
            data: Some(data),
            errors: vec![],
        }
    }
}
