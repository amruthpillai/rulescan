use crate::{
    discovery::{manifest, read_snapshot},
    model::*,
    paths::{self, Locations},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub fn payload_path(locations: &Locations, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).context("invalid quarantine id")?;
    Ok(locations.data.join("quarantine").join(id).join("payload"))
}

pub fn quarantine(
    store: &Store,
    locations: &Locations,
    artifact: &Artifact,
) -> Result<Option<String>> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (store, locations, artifact);
        bail!("quarantine is read-only on this platform pending native filesystem validation");
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let source = artifact.path.to_path()?;
        ensure!(
            source != artifact.root.to_path()?,
            "cannot quarantine an entire scan root"
        );
        ensure!(
            artifact.category != "agent_configuration",
            "shared settings are review-only"
        );
        validate_root(artifact)?;
        validate_tree(&source)?;
        flush_tree(&source)?;
        let (hash, identity) = snapshot(&source, artifact.category == "skill")?;
        ensure!(
            Some(&hash) == artifact.observed_sha256.as_ref()
                && Some(&identity) == artifact.identity.as_ref(),
            "conflict: source changed before quarantine"
        );
        let id = new_id();
        let payload = payload_path(locations, &id)?;
        paths::private_dir(payload.parent().unwrap())?;
        let dst_parent = paths::open_dir(payload.parent().unwrap())?;
        let src_parent = paths::open_dir(source.parent().unwrap())?;
        let dst_id = FileIdentity::from_metadata(&dst_parent.metadata()?);
        ensure!(
            identity.volume == dst_id.volume,
            "cross-volume quarantine is read-only; no source was removed"
        );
        let mut q = QuarantineRecord {
            quarantine_id: id.clone(),
            artifact: artifact.clone(),
            payload_sha256: hash.clone(),
            payload_identity: identity.clone(),
            original_mode: identity.mode,
            stage: "prepared".into(),
            created_at: now(),
            restored_to: None,
        };
        store.save_quarantine(&q)?;
        paths::sync_dir(payload.parent().unwrap().parent().unwrap())?;
        validate_root(artifact)?;
        rename_exclusive(
            &src_parent,
            source.file_name().unwrap(),
            &dst_parent,
            std::ffi::OsStr::new("payload"),
        )?;
        src_parent.sync_all()?;
        dst_parent.sync_all()?;
        // Rename preserves the sole recoverable copy, xattrs, ACLs and streams. A
        // concurrent name replacement is detected here and retained for recovery.
        let observed = snapshot(&payload, artifact.category == "skill");
        match observed {
            Ok((moved_hash, moved_id)) if moved_hash == hash && moved_id == identity => {
                q.stage = "moved".into();
                store.save_quarantine(&q)?;
                secure_payload(&payload, artifact)?;
                q.payload_sha256 = snapshot(&payload, artifact.category == "skill")?.0;
                q.stage = "completed".into();
                finalize_quarantine(store, &q)?;
                Ok(Some(id))
            }
            outcome => {
                q.stage = "needs_review".into();
                if let Ok((h, i)) = outcome {
                    q.payload_sha256 = h;
                    q.payload_identity = i;
                }
                store.save_quarantine(&q)?;
                bail!(
                    "conflict: moved entry differs from reviewed snapshot; recoverable payload retained as {id}"
                )
            }
        }
    }
}

pub fn restore(store: &Store, locations: &Locations, item: &PlanItem) -> Result<Option<String>> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (store, locations, item);
        bail!("restore is read-only on this platform pending native validation");
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let id = item
            .quarantine_id
            .as_deref()
            .context("missing quarantine id")?;
        let mut q = store.quarantine(id)?;
        ensure!(
            q.stage == "completed" && q.payload_sha256 == item.expected_sha256,
            "conflict: quarantine changed or already restored"
        );
        let payload = payload_path(locations, id)?;
        verify_payload(&payload, &q)?;
        let destination = item
            .destination
            .as_ref()
            .context("missing destination")?
            .to_path()?;
        ensure!(
            !locations.excluded(&destination),
            "cannot restore into RuleScan state"
        );
        let dst_parent = paths::open_dir(destination.parent().context("missing parent")?)?;
        let dst_id = FileIdentity::from_metadata(&dst_parent.metadata()?);
        ensure!(
            item.destination_parent_identity
                .as_ref()
                .is_some_and(|i| i.same_object(&dst_id)),
            "conflict: destination parent identity changed"
        );
        ensure!(
            q.payload_identity.volume == dst_id.volume,
            "cross-volume restore is read-only; payload retained"
        );
        let src_parent = paths::open_dir(payload.parent().unwrap())?;
        q.stage = "restoring".into();
        q.restored_to = Some(NativePath::new(&destination));
        store.save_quarantine(&q)?;
        restore_modes(&payload, &q)?;
        match rename_exclusive(
            &src_parent,
            std::ffi::OsStr::new("payload"),
            &dst_parent,
            destination.file_name().context("missing filename")?,
        ) {
            Ok(()) => {
                src_parent.sync_all()?;
                dst_parent.sync_all()?;
                let (hash, identity) = snapshot(&destination, q.artifact.category == "skill")?;
                ensure!(
                    Some(hash) == q.artifact.observed_sha256
                        && identity.same_object(&q.payload_identity),
                    "conflict: restored entry drifted; preserve it and review recovery"
                );
                q.stage = "restored".into();
                finalize_restore(store, &q)?;
                Ok(Some(id.into()))
            }
            Err(e) => {
                secure_payload(&payload, &q.artifact)?;
                q.payload_sha256 = snapshot(&payload, q.artifact.category == "skill")?.0;
                q.stage = "completed".into();
                q.restored_to = None;
                store.save_quarantine(&q)?;
                Err(e
                    .context("conflict: restore did not replace the destination; payload retained"))
            }
        }
    }
}

pub fn recover(store: &Store, locations: &Locations) -> Result<Vec<ItemOutcome>> {
    let mut outcomes = Vec::new();
    for mut q in store.quarantines()? {
        if ["completed", "restored", "aborted"].contains(&q.stage.as_str()) {
            continue;
        }
        let payload = payload_path(locations, &q.quarantine_id)?;
        let source = q.artifact.path.to_path()?;
        let recovered = (|| -> Result<()> {
            if q.stage == "needs_review" {
                bail!("payload requires manual identity review; nothing removed");
            }
            if q.stage == "restoring" {
                let destination = q
                    .restored_to
                    .as_ref()
                    .context("missing recovery destination")?
                    .to_path()?;
                if fs::symlink_metadata(&payload)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    let (hash, id) = snapshot(&destination, q.artifact.category == "skill")?;
                    ensure!(
                        Some(hash) == q.artifact.observed_sha256
                            && id.same_object(&q.payload_identity),
                        "restored destination identity/content is uncertain"
                    );
                    q.stage = "restored".into();
                } else {
                    verify_original_content(&payload, &q)?;
                    secure_payload(&payload, &q.artifact)?;
                    q.payload_sha256 = snapshot(&payload, q.artifact.category == "skill")?.0;
                    q.stage = "completed".into();
                    q.restored_to = None;
                }
            } else if fs::symlink_metadata(&payload).is_ok() {
                verify_original_content(&payload, &q)?;
                ensure!(
                    fs::symlink_metadata(&source)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                    "both source and payload exist; manual review required"
                );
                secure_payload(&payload, &q.artifact)?;
                q.payload_sha256 = snapshot(&payload, q.artifact.category == "skill")?.0;
                q.stage = "completed".into();
            } else {
                let (hash, id) = snapshot(&source, q.artifact.category == "skill")?;
                ensure!(
                    Some(hash) == q.artifact.observed_sha256 && id.same_object(&q.payload_identity),
                    "neither verified source nor verified payload found"
                );
                q.stage = "aborted".into();
            }
            Ok(())
        })();
        match recovered {
            Ok(()) => {
                if q.stage == "completed" {
                    finalize_quarantine(store, &q)?;
                } else if q.stage == "restored" {
                    finalize_restore(store, &q)?;
                } else {
                    store.save_quarantine(&q)?;
                }
                outcomes.push(ItemOutcome {
                    artifact_id: q.artifact.artifact_id,
                    status: "completed".into(),
                    detail: format!("recovered as {}", q.stage),
                    quarantine_id: Some(q.quarantine_id),
                });
            }
            Err(e) => {
                q.stage = "needs_review".into();
                store.save_quarantine(&q)?;
                outcomes.push(ItemOutcome {
                    artifact_id: q.artifact.artifact_id,
                    status: "failed".into(),
                    detail: escape(&format!("{e:#}"), false),
                    quarantine_id: Some(q.quarantine_id),
                });
            }
        }
    }
    Ok(outcomes)
}
fn complete_quarantine(store: &Store, q: &QuarantineRecord) -> Result<()> {
    let mut artifact = q.artifact.clone();
    artifact.state = ApprovalState::Quarantined;
    artifact.allowed_next_actions = vec!["inspect".into(), "explain".into()];
    store.save_artifact(&artifact)?;
    revoke_approval(store, &artifact.artifact_id)?;
    store.event(
        "quarantined",
        Some(&artifact.artifact_id),
        Some(&q.payload_sha256),
    )?;
    Ok(())
}
fn finalize_quarantine(store: &Store, q: &QuarantineRecord) -> Result<()> {
    let transaction = store.conn.unchecked_transaction()?;
    store.save_quarantine(q)?;
    complete_quarantine(store, q)?;
    transaction.commit()?;
    Ok(())
}
fn finalize_restore(store: &Store, q: &QuarantineRecord) -> Result<()> {
    let transaction = store.conn.unchecked_transaction()?;
    store.save_quarantine(q)?;
    revoke_approval(store, &q.artifact.artifact_id)?;
    let mut a = q.artifact.clone();
    a.state = if q.restored_to.as_ref() == Some(&a.path) {
        ApprovalState::New
    } else {
        ApprovalState::Missing
    };
    a.allowed_next_actions = vec!["inspect".into(), "explain".into()];
    store.save_artifact(&a)?;
    store.event("restored", Some(&a.artifact_id), Some(&q.payload_sha256))?;
    transaction.commit()?;
    Ok(())
}
fn revoke_approval(store: &Store, id: &str) -> Result<()> {
    store
        .conn
        .execute("DELETE FROM approvals WHERE artifact_id=?1", [id])?;
    Ok(())
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_root(a: &Artifact) -> Result<()> {
    let root = a.root.to_path()?;
    #[cfg(unix)]
    let meta = paths::open_dir(&root)?.metadata()?;
    #[cfg(windows)]
    let meta = fs::symlink_metadata(&root)?;
    ensure!(
        FileIdentity::from_metadata(&meta).same_object(&a.root_identity),
        "conflict: workspace root identity changed"
    );
    Ok(())
}
fn snapshot(p: &Path, bundle: bool) -> Result<(String, FileIdentity)> {
    if bundle {
        #[cfg(unix)]
        let metadata = paths::open_dir(p)?.metadata()?;
        #[cfg(windows)]
        let metadata = fs::symlink_metadata(p)?;
        let (_, hash) = manifest(p, &AtomicBool::new(false))?;
        Ok((hash, FileIdentity::from_metadata(&metadata)))
    } else {
        let (b, id) = read_snapshot(p)?;
        Ok((digest(&b), id))
    }
}
fn validate_tree(p: &Path) -> Result<()> {
    for (count, entry) in walkdir::WalkDir::new(p)
        .follow_links(false)
        .into_iter()
        .enumerate()
    {
        ensure!(
            count <= MAX_BUNDLE_ENTRIES,
            "mutation entry budget exceeded"
        );
        let entry = entry?;
        let m = fs::symlink_metadata(entry.path())?;
        ensure!(
            !paths::is_link(&m) && (m.is_file() || m.is_dir()),
            "links and special entries are read-only"
        );
        let id = FileIdentity::from_metadata(&m);
        ensure!(
            !m.is_file() || id.links == 1,
            "hard-linked files are read-only"
        );
        ensure!(
            id.mode & 0o7000 == 0,
            "special permission bits require manual recovery"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                m.uid() == unsafe { libc::geteuid() },
                "entries owned by another user are read-only"
            );
        }
    }
    Ok(())
}
pub fn verify_payload(p: &Path, q: &QuarantineRecord) -> Result<()> {
    let (hash, id) = snapshot(p, q.artifact.category == "skill")?;
    ensure!(
        hash == q.payload_sha256 && id.same_object(&q.payload_identity),
        "conflict: quarantine payload changed"
    );
    verify_original_content(p, q)
}
fn verify_original_content(p: &Path, q: &QuarantineRecord) -> Result<()> {
    validate_tree(p)?;
    if q.artifact.category == "skill" {
        let (entries, _) = manifest(p, &AtomicBool::new(false))?;
        let content = |list: &[ManifestEntry]| {
            list.iter()
                .map(|e| {
                    (
                        &e.path.encoding,
                        &e.path.value,
                        &e.entry_type,
                        &e.sha256,
                        &e.link_target,
                    )
                })
                .map(|e| serde_json::to_string(&e).unwrap())
                .collect::<Vec<_>>()
        };
        ensure!(
            content(&entries) == content(&q.artifact.bundle_manifest),
            "conflict: bundle contents changed"
        );
    } else {
        ensure!(
            Some(digest(&read_snapshot(p)?.0)) == q.artifact.observed_sha256,
            "conflict: payload bytes changed"
        );
    }
    let id = FileIdentity::from_metadata(&fs::symlink_metadata(p)?);
    ensure!(
        id.same_object(&q.payload_identity),
        "conflict: payload identity changed"
    );
    Ok(())
}
fn secure_payload(p: &Path, a: &Artifact) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if a.category == "skill" {
            for entry in &a.bundle_manifest {
                if entry.entry_type == "file" {
                    let file = paths::open_regular(&p.join(entry.path.to_path()?))?;
                    file.set_permissions(fs::Permissions::from_mode(0o600))?;
                    file.sync_all()?;
                }
            }
        } else {
            let file = paths::open_regular(p)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.sync_all()?;
        }
        paths::sync_dir(p.parent().unwrap())?;
    }
    #[cfg(windows)]
    {
        let _ = (p, a);
    }
    Ok(())
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn restore_modes(p: &Path, q: &QuarantineRecord) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if q.artifact.category == "skill" {
            for entry in &q.artifact.bundle_manifest {
                if entry.entry_type == "file" {
                    let file = paths::open_regular(&p.join(entry.path.to_path()?))?;
                    file.set_permissions(fs::Permissions::from_mode(entry.mode))?;
                    file.sync_all()?;
                }
            }
        } else {
            let file = paths::open_regular(p)?;
            file.set_permissions(fs::Permissions::from_mode(q.original_mode))?;
            file.sync_all()?;
        }
    }
    #[cfg(windows)]
    {
        let _ = (p, q);
    }
    Ok(())
}

#[cfg(unix)]
pub fn rename_exclusive(
    src: &std::fs::File,
    name: &std::ffi::OsStr,
    dst: &std::fs::File,
    to: &std::ffi::OsStr,
) -> Result<()> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    let name = CString::new(name.as_bytes())?;
    let to = CString::new(to.as_bytes())?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            src.as_raw_fd(),
            name.as_ptr(),
            dst.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            src.as_raw_fd(),
            name.as_ptr(),
            dst.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = (src, dst, name, to);
        bail!("exclusive native rename is unsupported");
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn flush_tree(path: &Path) -> Result<()> {
    let mut directories = Vec::new();
    for (count, entry) in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .enumerate()
    {
        ensure!(count <= MAX_BUNDLE_ENTRIES, "flush entry budget exceeded");
        let entry = entry?;
        if entry.file_type().is_file() {
            paths::open_regular(entry.path())?.sync_all()?;
        } else if entry.file_type().is_dir() {
            directories.push(entry.path().to_owned());
        } else {
            bail!("unsupported entry while flushing source");
        }
    }
    for p in directories.iter().rev() {
        paths::sync_dir(p)?;
    }
    Ok(())
}
