# Recovery and safe file operations

There is no permanent-delete or automatic-purge command. Back up the entire
private state and data layout together before manual recovery. Do not edit SQLite
or remove `data/quarantine` simply because an action failed.

Run `rulescan doctor`, `rulescan quarantine list --format json`, and
`rulescan recover --format json`. Recovery runs under the same coordination lock
as ordinary actions. `apply` reconciles incomplete quarantine records before
accepting new file operations. An interrupted action plan cannot be replayed;
create a new plan after reviewing actual state.

| Durable stage / observed files | Reconciliation |
| --- | --- |
| prepared; verified original exists; no payload | Mark aborted; leave source untouched |
| prepared/moved; verified payload exists; no original | Verify identity and original contents, disable execution, finalize quarantine |
| source and payload both exist | Keep both; mark needs_review |
| neither verified source nor verified payload exists | Mark needs_review; no deletion |
| restoring; payload still exists | Verify it, return it to safe stored modes, make a fresh restore plan |
| restoring; no payload; reviewed destination matches bytes and inode | Finalize restored status and revoke former approval |
| payload digest/identity/content mismatch | Keep recoverable material; manual review |

Normal restore is always preview-plan then apply:

```sh
rulescan restore QUARANTINE_ID --to /existing-parent/unused-name --format json
rulescan plan show PLAN_ID --format json
rulescan apply PLAN_ID --no-input --format json
```

If another process creates the destination after preview, native no-replace rename
fails and the payload remains. An existing symlink also counts as an occupied
destination. Restore never replaces it. Cross-volume restoration is unsupported;
choose an unused path on the payload's volume or perform a separately reviewed
manual copy while retaining the original until verification.

The opaque quarantine ID maps to `data/quarantine/ID/payload`. A file is stored
without executable bits and with mode 0600. Bundles retain directory structure
inside the private 0700 hierarchy; each regular file becomes 0600, with original
file permissions retained in the durable manifest for restore. Keep this layout
outside active agent skill/config directories. Same-volume rename preserves native
metadata, including xattrs; special bits, foreign ownership, hard links, links,
special files and shared settings are refused. macOS flags / unusual ACL denial
may leave a recoverable moved entry with an incomplete record, requiring review.

For `needs_review`, first make a verified offline backup of source, payload and
state. Compare recorded hashes, native paths and actual contents without executing
them. Resolve which path is authoritative manually. RuleScan deliberately does not
guess or delete one of two competing copies. A same-user process can alter these
records, so do not treat them as tamperproof forensic evidence.

Tests exercise real temporary files, prepared/moved recovery, source/payload
ambiguity, exact snapshots, mode/xattr restoration and destination collisions.
Release testing still requires process termination at every durable boundary,
full-disk/fsync failures, cross-volume volumes, locked files/antivirus, concurrent
editor saves and native sharing semantics. These are not covered by claiming that
unit tests passed; see the release checklist.
