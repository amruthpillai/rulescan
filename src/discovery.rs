use crate::{
    config::{self, CompiledRule, Config},
    model::*,
    paths::{self, Locations},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Found(Box<Artifact>),
    Progress(u64),
    Finished(Box<ScanResult>),
    Failed(String),
}

pub fn read_snapshot(path: &Path) -> Result<(Vec<u8>, FileIdentity)> {
    let mut file = paths::open_regular(path).context("cannot safely open regular file")?;
    let before = FileIdentity::from_metadata(&file.metadata()?);
    ensure!(before.size <= MAX_FILE_BYTES, "file exceeds 8 MiB budget");
    let mut bytes = Vec::with_capacity(before.size as usize);
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = FileIdentity::from_metadata(&file.metadata()?);
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES && before == after && bytes.len() as u64 == after.size,
        "file changed while hashing"
    );
    Ok((bytes, after))
}

pub fn manifest(path: &Path, cancel: &AtomicBool) -> Result<(Vec<ManifestEntry>, String)> {
    let mut entries = Vec::new();
    let mut total = 0u64;
    for item in WalkDir::new(path).follow_links(false).sort_by_file_name() {
        ensure!(!cancel.load(Ordering::Relaxed), "scan cancelled");
        let entry = item?;
        if entry.path() == path {
            continue;
        }
        ensure!(
            entries.len() < MAX_BUNDLE_ENTRIES,
            "bundle exceeds 10000 entry budget"
        );
        let m = fs::symlink_metadata(entry.path())?;
        let mut identity = FileIdentity::from_metadata(&m);
        let (kind, sha256, link) = if paths::is_link(&m) {
            (
                "symlink",
                None,
                Some(NativePath::new(&fs::read_link(entry.path())?)),
            )
        } else if m.is_file() {
            total = total.checked_add(m.len()).context("bundle size overflow")?;
            ensure!(total <= MAX_BUNDLE_BYTES, "bundle exceeds 64 MiB budget");
            let (bytes, id) = read_snapshot(entry.path())?;
            identity = id;
            ("file", Some(digest(&bytes)), None)
        } else if m.is_dir() {
            ("directory", None, None)
        } else {
            anyhow::bail!("bundle contains unsupported special entry");
        };
        entries.push(ManifestEntry {
            path: NativePath::new(entry.path().strip_prefix(path)?),
            entry_type: kind.into(),
            sha256,
            executable_mode: identity.mode & 0o111,
            mode: identity.mode,
            link_target: link,
        });
    }
    entries.sort_by(|a, b| a.path.value.cmp(&b.path.value));
    // Display strings and timestamps are deliberately absent from the canonical manifest.
    let canonical: Vec<_> = entries
        .iter()
        .map(|e| {
            (
                &e.path.encoding,
                &e.path.value,
                &e.entry_type,
                &e.sha256,
                e.executable_mode,
                e.link_target.as_ref().map(|p| (&p.encoding, &p.value)),
            )
        })
        .collect();
    let hash = digest(&serde_json::to_vec(&("bundle-manifest-1", canonical))?);
    Ok((entries, hash))
}

#[derive(Default)]
struct Match {
    category: String,
    profiles: Vec<String>,
    rules: Vec<String>,
    bundle: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileManifest {
    schema_version: u32,
    version: String,
    loading_semantics: String,
    rules: Vec<ProfileRule>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileRule {
    profile: String,
    id: String,
    #[serde(rename = "match")]
    matcher: config::MatchSpec,
    category: String,
    source: String,
}
fn builtins() -> &'static Vec<(String, CompiledRule)> {
    static RULES: std::sync::OnceLock<Vec<(String, CompiledRule)>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        let manifest: ProfileManifest =
            serde_json::from_str(include_str!("../profiles/builtin.json"))
                .expect("validated built-in profile manifest");
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.version, PROFILE_VERSION);
        assert!(!manifest.loading_semantics.is_empty());
        manifest
            .rules
            .into_iter()
            .map(|rule| {
                assert!(rule.source.starts_with("https://"));
                let compiled = CompiledRule::new(config::Rule {
                    id: rule.id,
                    matcher: rule.matcher,
                    category: rule.category,
                    on_new: "review".into(),
                    on_change: "review".into(),
                })
                .expect("validated built-in rule");
                (rule.profile, compiled)
            })
            .collect()
    })
}
fn matches(
    config: &Config,
    rules: &[CompiledRule],
    relative: &Path,
    absolute: Option<&Path>,
) -> Option<Match> {
    let mut m = Match::default();
    for (profile, rule) in builtins() {
        if config.enabled(profile) && rule.matches(absolute.unwrap_or(relative)) {
            let bundle = rule.rule.matcher.bundle_marker.is_some();
            if m.category.is_empty() || bundle {
                m.category = rule.rule.category.clone();
            }
            if !m.profiles.contains(profile) {
                m.profiles.push(profile.clone());
            }
            m.rules.push(rule.rule.id.clone());
            m.bundle |= bundle;
        }
    }
    for rule in rules {
        if rule.matches(relative) {
            let bundle = rule.rule.matcher.bundle_marker.is_some();
            if m.category.is_empty() || bundle {
                m.category = if bundle {
                    "skill".into()
                } else {
                    rule.rule.category.clone()
                };
            }
            m.rules.push(format!("user.{}", rule.rule.id));
            if !m.profiles.iter().any(|p| p == "custom") {
                m.profiles.push("custom".into());
            }
            m.bundle |= bundle;
        }
    }
    (!m.rules.is_empty()).then_some(m)
}

pub fn pattern_test(config: &Config, path: &Path) -> Result<serde_json::Value> {
    ensure!(
        !path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "test path must be relative without .."
    );
    let rules = config
        .rules
        .iter()
        .cloned()
        .map(CompiledRule::new)
        .collect::<Result<Vec<_>>>()?;
    let m = matches(config, &rules, path, None);
    Ok(
        serde_json::json!({"path": NativePath::new(path), "rule_ids": m.as_ref().map(|x| &x.rules), "category": m.as_ref().map(|x| &x.category), "bundle": m.as_ref().is_some_and(|x| x.bundle), "loading": "candidate only; agent-specific loading is not verified"}),
    )
}

pub fn scan(
    config: &Config,
    locations: &Locations,
    roots: &[PathBuf],
    cancel: Arc<AtomicBool>,
    events: Option<&SyncSender<ScanEvent>>,
) -> Result<ScanResult> {
    config.validate()?;
    let compiled = config
        .rules
        .iter()
        .cloned()
        .map(CompiledRule::new)
        .collect::<Result<Vec<_>>>()?;
    let exclusions: Vec<_> = config
        .exclusions
        .iter()
        .map(|e| Ok((config::expand(&e.path)?, e.reason.clone())))
        .collect::<Result<_>>()?;
    let mut coverage = Coverage::default();
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for requested in roots {
        if cancel.load(Ordering::Relaxed) {
            coverage.cancelled = true;
            break;
        }
        let expanded = config::expand(requested)?;
        let root = match fs::canonicalize(&expanded) {
            Ok(p) => p,
            Err(e) => {
                gap(&mut coverage, &expanded, &e.to_string());
                continue;
            }
        };
        if coverage.roots.iter().any(|p| p == &NativePath::new(&root)) {
            continue;
        }
        coverage.roots.push(NativePath::new(&root));
        let meta = match fs::symlink_metadata(&expanded) {
            Ok(m) => m,
            Err(e) => {
                gap(&mut coverage, &expanded, &e.to_string());
                continue;
            }
        };
        if paths::is_link(&meta) {
            gap(
                &mut coverage,
                &expanded,
                "root is a link; choose its resolved location explicitly",
            );
            continue;
        }
        if !meta.is_dir() {
            gap(&mut coverage, &root, "scan root is not a directory");
            continue;
        }
        if is_virtual(&root) {
            gap(
                &mut coverage,
                &root,
                "virtual filesystem root is unsupported",
            );
            continue;
        }
        let root_identity = FileIdentity::from_metadata(&fs::metadata(&root)?);
        let root_id = digest(&serde_json::to_vec(&(
            &NativePath::new(&root).value,
            root_identity.volume,
            root_identity.file,
        ))?);
        let iter = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                !locations.excluded(e.path())
                    && !exclusions.iter().any(|(p, _)| e.path().starts_with(p))
                    && !is_virtual(e.path())
            });
        for (p, reason) in &exclusions {
            if p.starts_with(&root) || root.starts_with(p) {
                coverage.exclusions.push(CoverageGap {
                    path: NativePath::new(p),
                    reason: reason.clone(),
                });
            }
        }
        for p in [&locations.state, &locations.data, &locations.cache] {
            if p.starts_with(&root) {
                coverage.exclusions.push(CoverageGap {
                    path: NativePath::new(p),
                    reason: "RuleScan private state".into(),
                });
            }
        }
        #[cfg(target_os = "linux")]
        for p in ["/proc", "/sys", "/dev"] {
            if Path::new(p).starts_with(&root) {
                coverage.exclusions.push(CoverageGap {
                    path: NativePath::new(Path::new(p)),
                    reason: "virtual filesystem".into(),
                });
            }
        }
        if locations.excluded(&root) {
            coverage.exclusions.push(CoverageGap {
                path: NativePath::new(&root),
                reason: "RuleScan private state".into(),
            });
        }
        for entry in iter {
            if cancel.load(Ordering::Relaxed) {
                coverage.cancelled = true;
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    gap(&mut coverage, e.path().unwrap_or(&root), &e.to_string());
                    continue;
                }
            };
            coverage.entries_visited += 1;
            if coverage.entries_visited > 1_000_000 {
                gap(
                    &mut coverage,
                    &root,
                    "scan exceeds 1000000 entry budget; narrow the roots",
                );
                break;
            }
            if coverage.entries_visited.is_multiple_of(256)
                && let Some(tx) = events
            {
                let _ = tx.try_send(ScanEvent::Progress(coverage.entries_visited));
            }
            let relative = entry.path().strip_prefix(&root)?;
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(m) => m,
                Err(e) => {
                    gap(&mut coverage, entry.path(), &e.to_string());
                    continue;
                }
            };
            if paths::is_link(&metadata) {
                gap(
                    &mut coverage,
                    entry.path(),
                    "link/reparse target not traversed; coverage does not include its target",
                );
            }
            if metadata.is_dir() {
                continue;
            }
            let Some(m) = matches(config, &compiled, relative, Some(entry.path())) else {
                continue;
            };
            let target = if m.bundle {
                entry.path().parent().context("bundle has no parent")?
            } else {
                entry.path()
            };
            // A SKILL.md in the scan root does not make the entire selected workspace a mutation unit.
            let blocked = m.bundle
                && (exclusions.iter().any(|(p, _)| p.starts_with(target))
                    || [&locations.state, &locations.data, &locations.cache]
                        .iter()
                        .any(|p| p.starts_with(target)));
            let mut artifact = observe(config, &root, &root_identity, &root_id, target, m, &cancel, &mut coverage,blocked.then_some("bundle intersects an excluded/private scope; narrow the roots or relocate the skill"))?;
            if !seen.insert(artifact.artifact_id.clone()) {
                continue;
            }
            if target == root && artifact.category == "skill" {
                artifact
                    .reasons
                    .push("bundle is the scan root; quarantine is disabled".into());
                artifact.allowed_next_actions.retain(|a| a != "quarantine");
            }
            if let Some(tx) = events {
                let _ = tx.try_send(ScanEvent::Found(Box::new(artifact.clone())));
            }
            artifacts.push(artifact);
        }
    }
    // Resolve only bounded local @path directives in Claude/Gemini memory. References
    // remain explicit inventory nodes; no remote content, settings, or hooks execute.
    let mut index: HashMap<String, usize> = artifacts
        .iter()
        .enumerate()
        .map(|(i, a)| (a.path.value.clone(), i))
        .collect();
    let mut pending: std::collections::VecDeque<_> = artifacts
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            a.category == "instructions"
                && a.profiles.iter().any(|p| p == "claude" || p == "gemini")
        })
        .map(|(i, _)| (i, 0usize))
        .collect();
    let mut parsed = HashSet::new();
    let mut import_bytes = 0usize;
    while let Some((cursor, depth)) = pending.pop_front() {
        if !parsed.insert(cursor) {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            coverage.cancelled = true;
            break;
        }
        let parent = artifacts[cursor].clone();
        let path = parent.path.to_path()?;
        let bytes = match read_snapshot(&path) {
            Ok((b, id))
                if Some(digest(&b)) == parent.observed_sha256
                    && Some(&id) == parent.identity.as_ref() =>
            {
                b
            }
            _ => {
                gap(
                    &mut coverage,
                    &path,
                    "instruction changed or became unreadable during reference resolution",
                );
                artifacts[cursor].approval_digest = None;
                continue;
            }
        };
        import_bytes += bytes.len();
        if import_bytes > MAX_BUNDLE_BYTES as usize || depth >= 16 {
            gap(
                &mut coverage,
                &path,
                "import depth or total-byte budget exceeded",
            );
            artifacts[cursor].approval_digest = None;
            continue;
        }
        let mut references = Vec::new();
        let mut seen_refs = HashSet::new();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Some(rest) = line.trim().strip_prefix('@') else {
                continue;
            };
            let token = rest.trim().trim_matches('"');
            if token.is_empty() {
                continue;
            }
            if references.len() >= 128 {
                gap(&mut coverage, &path, "import count budget exceeded");
                artifacts[cursor].approval_digest = None;
                break;
            }
            let proposed = if token.starts_with("~/") {
                config::expand(Path::new(token))?
            } else {
                path.parent().unwrap().join(token)
            };
            if token.contains("://") || token.starts_with("www.") {
                references.push(Reference {
                    path: NativePath::new(Path::new(token)),
                    artifact_id: None,
                    reason: "remote reference not fetched".into(),
                });
                gap(
                    &mut coverage,
                    &path,
                    "remote import is outside offline coverage",
                );
                continue;
            }
            let resolved = match fs::canonicalize(&proposed) {
                Ok(p) => p,
                Err(_) => {
                    gap(&mut coverage, &proposed, "unresolved import");
                    references.push(Reference {
                        path: NativePath::new(&proposed),
                        artifact_id: None,
                        reason: "unresolved import".into(),
                    });
                    continue;
                }
            };
            if !seen_refs.insert(resolved.clone()) {
                continue;
            }
            let root = parent.root.to_path()?;
            if !resolved.starts_with(&root)
                || locations.excluded(&resolved)
                || exclusions.iter().any(|(p, _)| resolved.starts_with(p))
            {
                gap(
                    &mut coverage,
                    &resolved,
                    "import outside declared root or inside excluded scope; not followed",
                );
                references.push(Reference {
                    path: NativePath::new(&resolved),
                    artifact_id: None,
                    reason: "outside covered scope".into(),
                });
                continue;
            }
            if !matches!(fs::symlink_metadata(&proposed),Ok(m) if !paths::is_link(&m))
                || paths::open_regular(&proposed).is_err()
            {
                gap(
                    &mut coverage,
                    &proposed,
                    "import link, unstable or unsupported entry",
                );
                references.push(Reference {
                    path: NativePath::new(&proposed),
                    artifact_id: None,
                    reason: "import not safely readable".into(),
                });
                artifacts[cursor].approval_digest = None;
                continue;
            }
            let key = NativePath::new(&resolved).value;
            let child_index = if let Some(i) = index.get(&key) {
                *i
            } else {
                if artifacts.len() >= 10_000 {
                    gap(
                        &mut coverage,
                        &path,
                        "import graph exceeds 10000 artifact budget",
                    );
                    references.push(Reference {
                        path: NativePath::new(&resolved),
                        artifact_id: None,
                        reason: "import graph budget".into(),
                    });
                    break;
                }
                let m = Match {
                    category: "instructions".into(),
                    profiles: parent.profiles.clone(),
                    rules: vec!["reference.local-at-path".into()],
                    bundle: false,
                };
                let child = observe(
                    config,
                    &root,
                    &parent.root_identity,
                    &parent.root_id,
                    &resolved,
                    m,
                    &cancel,
                    &mut coverage,
                    None,
                )?;
                let i = artifacts.len();
                index.insert(key, i);
                artifacts.push(child);
                i
            };
            pending.push_back((child_index, depth + 1));
            references.push(Reference {
                path: NativePath::new(&resolved),
                artifact_id: Some(artifacts[child_index].artifact_id.clone()),
                reason: "local @path directive; candidate loading only".into(),
            });
        }
        artifacts[cursor].references = references;
    }
    let by_id: HashMap<String, usize> = artifacts
        .iter()
        .enumerate()
        .map(|(i, a)| (a.artifact_id.clone(), i))
        .collect();
    let mut computed = HashMap::new();
    for i in 0..artifacts.len() {
        let hash = dependency_digest(i, &artifacts, &by_id, &mut HashSet::new(), &mut computed);
        if hash.is_none() && !artifacts[i].references.is_empty() {
            gap(
                &mut coverage,
                &artifacts[i].path.to_path()?,
                "incomplete or cyclic import graph; approval unavailable",
            );
        }
        artifacts[i].approval_digest = hash;
        if artifacts[i].approval_digest.is_none() {
            artifacts[i].allowed_next_actions.clear();
        }
    }
    Ok(ScanResult {
        artifacts,
        coverage,
    })
}

fn dependency_digest(
    i: usize,
    artifacts: &[Artifact],
    by_id: &HashMap<String, usize>,
    visiting: &mut HashSet<usize>,
    cache: &mut HashMap<usize, Option<String>>,
) -> Option<String> {
    if let Some(value) = cache.get(&i) {
        return value.clone();
    }
    if visiting.len() >= 16 {
        return None;
    }
    if !visiting.insert(i) {
        return None;
    }
    let a = &artifacts[i];
    let mut children = Vec::new();
    for r in &a.references {
        let Some(child) = r.artifact_id.as_ref().and_then(|id| by_id.get(id)) else {
            visiting.remove(&i);
            cache.insert(i, None);
            return None;
        };
        let Some(hash) = dependency_digest(*child, artifacts, by_id, visiting, cache) else {
            visiting.remove(&i);
            cache.insert(i, None);
            return None;
        };
        children.push((r.path.value.clone(), hash));
    }
    visiting.remove(&i);
    let hash = a.approval_digest.as_ref().map(|base| {
        if children.is_empty() {
            base.clone()
        } else {
            digest(&serde_json::to_vec(&(base, children)).unwrap())
        }
    });
    cache.insert(i, hash.clone());
    hash
}

#[allow(clippy::too_many_arguments)]
fn observe(
    config: &Config,
    root: &Path,
    root_identity: &FileIdentity,
    root_id: &str,
    path: &Path,
    m: Match,
    cancel: &AtomicBool,
    coverage: &mut Coverage,
    blocked: Option<&str>,
) -> Result<Artifact> {
    let relative = path.strip_prefix(root)?;
    let artifact_id = digest(&serde_json::to_vec(&(
        root_id,
        NativePath::new(relative).value,
        &m.category,
    ))?);
    let mut reasons =
        vec!["Filename/profile match is a candidate, not proof of agent loading or safety".into()];
    let snapshot = if let Some(reason) = blocked {
        Err(anyhow::anyhow!("{reason}"))
    } else if m.bundle {
        manifest(path, cancel).and_then(|(entries, hash)| {
            let meta = fs::symlink_metadata(path)?;
            ensure!(
                meta.is_dir() && !paths::is_link(&meta),
                "bundle root must be a real directory"
            );
            Ok((hash, FileIdentity::from_metadata(&meta), entries))
        })
    } else {
        read_snapshot(path).map(|(bytes, id)| (digest(&bytes), id, vec![]))
    };
    let (hash, identity, entries) = match snapshot {
        Ok((h, id, e)) => (Some(h), Some(id), e),
        Err(e) => {
            gap(coverage, path, &e.to_string());
            reasons.push(e.to_string());
            (None, None, vec![])
        }
    };
    if entries.iter().any(|e| e.entry_type == "symlink") {
        gap(
            coverage,
            path,
            "bundle contains links; targets are outside manifest coverage",
        );
    }
    let approval_digest = if entries.iter().any(|e| e.entry_type == "symlink") {
        None
    } else {
        hash.as_ref().map(|h| {
            digest(
                &serde_json::to_vec(&(
                    root_id,
                    NativePath::new(relative).value,
                    &m.category,
                    h,
                    identity.as_ref().map(|i| i.mode & 0o111),
                    config.revision(),
                ))
                .unwrap(),
            )
        })
    };
    let mut allowed = vec!["inspect".into(), "explain".into()];
    if hash.is_some() {
        allowed.push("approve".into());
        if m.category != "agent_configuration" && !entries.iter().any(|e| e.entry_type == "symlink")
        {
            allowed.push("quarantine".into());
        }
    }
    #[cfg(windows)]
    {
        allowed.retain(|s| s != "approve" && s != "quarantine");
        reasons.push("Windows inventory is read-only pending native root identity and private ACL qualification; no baseline can be approved".into());
    }
    Ok(Artifact {
        artifact_id,
        root_id: root_id.into(),
        root: NativePath::new(root),
        root_identity: root_identity.clone(),
        path: NativePath::new(path),
        relative_path: NativePath::new(relative),
        category: m.category,
        profiles: m.profiles,
        matching_rule_ids: m.rules,
        reasons,
        observed_sha256: hash.clone(),
        approval_digest,
        identity,
        bundle_manifest: entries,
        references: vec![],
        state: if hash.is_some() {
            ApprovalState::New
        } else {
            ApprovalState::Unreadable
        },
        profile_version: format!("{PROFILE_VERSION}:{}", config.revision()),
        policy_version: POLICY_VERSION.into(),
        allowed_next_actions: allowed,
        observed_at: now(),
    })
}
fn gap(coverage: &mut Coverage, path: &Path, reason: &str) {
    if coverage.gaps.len() < 1000 {
        coverage.gaps.push(CoverageGap {
            path: NativePath::new(path),
            reason: escape(reason, false),
        });
    }
}
fn is_virtual(p: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        ["/proc", "/sys", "/dev"].iter().any(|x| p.starts_with(x))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = p;
        false
    }
}
