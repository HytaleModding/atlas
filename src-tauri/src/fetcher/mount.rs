//! Verify a downloaded artifact, extract it to a staging dir, then
//! atomic-rename into `<indexes>/<build_id>/` and write the `.ok`
//! marker that `SearchCatalog` requires before it'll open the index
//!.
//!
//! Flow:
//!   1. `artifact::verify` streams the `.tar.zst` once to confirm
//!      `manifest.json` parses, `SHA256SUMS` matches the manifest's
//!      digest, and every payload file's streaming SHA256 matches.
//!   2. `signing::verify_manifest` Ed25519-verifies `manifest.json.sig`
//!      against the embedded pubkey. If the pubkey fingerprint in the
//!      manifest doesn't match our compiled-in fingerprint, we reject.
//!   3. A schema/min-client version compatibility check runs against
//!      constants defined in `indexer::metadata`. Older clients mounting
//!      newer artifacts is a hard NO.
//!   4. Stream-extract the archive into `<indexes>/.tmp/<build_id>/`.
//!   5. Atomic `rename(.tmp/<build_id> → <build_id>)`. On filesystems
//!      that won't rename-over-non-empty-dir, we delete the target
//!      first; loss of that target is safe because `.ok` is absent and
//!      `SearchCatalog` won't have mounted it yet.
//!   6. Write `.ok`. Only then does the index become mountable.
//!
//! This keeps the "partially-extracted" state invisible to
//! `SearchCatalog` - the only directory under `<indexes>/` that matters
//! is one with a `.ok` marker.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::artifact::{self, VerifiedArtifact};
use super::manifest::Manifest;
use super::signing::{self, embedded_pubkey, fingerprint};
use crate::config::Slot;
use crate::indexer::metadata::{IndexMetadata, MIN_CLIENT_VERSION, SCHEMA_VERSION};

/// Filename written inside a mounted index dir once extraction
/// completes. `SearchCatalog` uses this as a readiness marker.
pub const MOUNT_OK_MARKER: &str = ".ok";

/// Progress sink for the extraction pass. Kept sync + dyn-free so the
/// blocking extractor on a tokio worker thread can call it cheaply.
pub trait ExtractProgress {
    fn report(&self, current: usize, total: usize);
}

/// No-op progress - useful for tests and internal callers that don't
/// care about events.
pub struct NoProgress;
impl ExtractProgress for NoProgress {
    fn report(&self, _: usize, _: usize) {}
}

/// Verify + mount an already-downloaded `.tar.zst` into
/// `<indexes_root>/<build_id>/`. Returns the mounted path on success.
///
/// This is sync because tar + zstd extraction is blocking - callers
/// should run it on `spawn_blocking`.
pub fn verify_and_mount(
    artifact_path: &Path,
    indexes_root: &Path,
    progress: &dyn ExtractProgress,
) -> Result<MountedArtifact> {
    let pubkey = embedded_pubkey().context("loading embedded signing pubkey")?;
    verify_and_mount_with_pubkey(artifact_path, indexes_root, &pubkey, progress)
}

/// Variant of [`verify_and_mount`] that takes an explicit trust root.
/// Used by integration tests where the embedded all-zeros pubkey
/// can't validate real signatures. Production callers should use
/// [`verify_and_mount`].
pub fn verify_and_mount_with_pubkey(
    artifact_path: &Path,
    indexes_root: &Path,
    trusted_pubkey: &[u8; 32],
    progress: &dyn ExtractProgress,
) -> Result<MountedArtifact> {
    // 1. Whole-archive verify: manifest, SHA256SUMS, per-file digests.
    let verified: VerifiedArtifact = artifact::verify(artifact_path)?;

    // 2. Ed25519 signature verify against the supplied pubkey.
    if verified.signature_bytes.is_empty() {
        bail!("artifact is unsigned; atlas client refuses to mount unsigned indexes");
    }
    signing::verify_manifest(
        &verified.manifest_bytes,
        &verified.signature_bytes,
        trusted_pubkey,
    )?;
    let expected_fp = fingerprint(trusted_pubkey)?;
    if verified.manifest.signing_pubkey_fingerprint != expected_fp {
        bail!(
            "manifest signed by fingerprint {} but this client trusts {}",
            verified.manifest.signing_pubkey_fingerprint,
            expected_fp
        );
    }

    // 3. Schema / client compatibility.
    check_compatibility(&verified.manifest)?;

    // 4. Extract to the staging directory.
    //
    // build_id becomes a directory name under `indexes_root`, so it must
    // be path-traversal-safe. Hytale `Implementation-Version` strings
    // legitimately contain dots (e.g. `release-2026.03.26-89796e57b`),
    // so we can't ban `.` outright; instead reject the components that
    // make traversal possible: empty, `..` substring, leading `.`,
    // slashes, NUL, or any other control byte.
    let build_id = verified.manifest.build_id.clone();
    let bad = build_id.is_empty()
        || build_id.starts_with('.')
        || build_id.contains("..")
        || build_id.contains(['/', '\\', '\0'])
        || build_id.chars().any(|c| c.is_control());
    if bad {
        bail!("manifest has unsafe build_id {build_id:?}");
    }
    let staging = indexes_root.join(".tmp").join(&build_id);
    let final_dir = indexes_root.join(&build_id);

    // Clean any leftover staging from a previous crashed attempt.
    if staging.exists() {
        std::fs::remove_dir_all(&staging).with_context(|| {
            format!("clearing stale staging dir {}", staging.display())
        })?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("creating staging dir {}", staging.display()))?;

    extract_all(artifact_path, &staging, progress)?;

    // 5. Atomic rename into place. If the final dir exists, delete it
    // first - it might be a half-mounted dir from a prior crash (if
    // it had `.ok`, SearchCatalog would already be serving it and the
    // fetcher would have errored earlier in the flow).
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir).with_context(|| {
            format!("removing stale final dir {}", final_dir.display())
        })?;
    }
    if let Some(parent) = final_dir.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::rename(&staging, &final_dir).with_context(|| {
        format!(
            "renaming {} → {}",
            staging.display(),
            final_dir.display()
        )
    })?;

    // 6. Write the `.ok` marker last. Its presence means "this dir is
    // fully extracted + hash-verified + signature-verified".
    std::fs::write(final_dir.join(MOUNT_OK_MARKER), b"ok\n")
        .with_context(|| format!("writing {}", MOUNT_OK_MARKER))?;

    Ok(MountedArtifact {
        build_id,
        mounted_at: final_dir,
        manifest: verified.manifest,
    })
}

/// What `verify_and_mount` hands back to the orchestrator.
#[derive(Debug, Clone)]
pub struct MountedArtifact {
    pub build_id: String,
    pub mounted_at: PathBuf,
    pub manifest: Manifest,
}

/// Refuse to mount artifacts from a newer Atlas format, or that target
/// a client newer than this one. `min_client_version` is the hard
/// floor; `schema_version` is an integer Atlas controls.
fn check_compatibility(manifest: &Manifest) -> Result<()> {
    if manifest.schema_version > SCHEMA_VERSION {
        bail!(
            "artifact schema_version {} is newer than this client supports ({}). Update Atlas.",
            manifest.schema_version,
            SCHEMA_VERSION
        );
    }
    // Client-version gate: compare our crate version to the manifest's
    // `min_client_version`. Pragmatic string compare is fine here
    // because we emit semver and the format is fixed.
    let ours = env!("CARGO_PKG_VERSION");
    if !version_at_least(ours, &manifest.min_client_version) {
        bail!(
            "artifact requires Atlas >= {} but this client is {}. Update Atlas.",
            manifest.min_client_version,
            ours
        );
    }
    // Chunker / embedder drift is an audit, not a hard stop - log it
    // but don't refuse. Runtime will catch a real mismatch (e.g.,
    // embedder dim) well before a search returns wrong results.
    if manifest.min_client_version != MIN_CLIENT_VERSION {
        tracing::debug!(
            "artifact min_client_version={} vs local pin={}",
            manifest.min_client_version,
            MIN_CLIENT_VERSION
        );
    }
    Ok(())
}

/// Naive semver-prefix compare: parses dot-separated numeric prefix of
/// each version and compares lexicographically. Ignores pre-release
/// suffixes because Atlas doesn't ship them (yet).
fn version_at_least(candidate: &str, minimum: &str) -> bool {
    fn parts(v: &str) -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<u32>().ok())
            .collect()
    }
    let a = parts(candidate);
    let b = parts(minimum);
    for (x, y) in a.iter().zip(b.iter()) {
        if x > y {
            return true;
        }
        if x < y {
            return false;
        }
    }
    a.len() >= b.len()
}

/// Stream-extract `.tar.zst` into `dest`. Reports progress by entry
/// count. Directory entries count too - they're cheap and keep the
/// progress bar advancing during the dense `tantivy/**` portion.
fn extract_all(archive: &Path, dest: &Path, progress: &dyn ExtractProgress) -> Result<()> {
    // First pass: count entries so we can emit a meaningful total.
    let total_entries = {
        let file = File::open(archive)
            .with_context(|| format!("opening archive {}", archive.display()))?;
        let decoder = zstd::Decoder::new(file).context("zstd decoder for count pass")?;
        let mut ar = tar::Archive::new(decoder);
        let mut n = 0;
        for entry in ar
            .entries()
            .context("reading tar entries for count pass")?
        {
            entry.context("iterating tar entry in count pass")?;
            n += 1;
        }
        n
    };

    // Second pass: extract.
    let file = File::open(archive)
        .with_context(|| format!("opening archive {}", archive.display()))?;
    let decoder = zstd::Decoder::new(file).context("zstd decoder for extract pass")?;
    let mut ar = tar::Archive::new(decoder);
    ar.set_preserve_permissions(false);
    ar.set_overwrite(true);

    let mut current = 0usize;
    for entry in ar
        .entries()
        .context("reading tar entries for extract pass")?
    {
        let mut entry = entry.context("iterating tar entry")?;
        let rel = entry
            .path()
            .context("reading tar entry path")?
            .into_owned();

        // Defensive: reject path-escape. tar-rs already does this, but
        // we bail clearly so log messages explain what happened.
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir))
        {
            bail!("tar entry escapes staging dir: {}", rel.display());
        }

        let out_path = dest.join(&rel);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out_path).with_context(|| {
                format!("creating dir {}", out_path.display())
            })?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent {}", parent.display()))?;
            }
            entry
                .unpack(&out_path)
                .with_context(|| format!("unpacking {}", out_path.display()))?;
        }
        current += 1;
        // Progress throttling: every 32 entries, plus the final one.
        if current % 32 == 0 || current == total_entries {
            progress.report(current, total_entries);
        }
    }
    progress.report(current, total_entries);
    Ok(())
}

/// Directory names that live directly under `<indexes_root>/` but are
/// NOT mounted-artifact dirs and so must not be reaped for missing the
/// `.ok` marker. The `tantivy/` and `lance/` containers hold the legacy
/// per-slot subdirs (`tantivy/release/`, `lance/release/`, …) that
/// `SearchCatalog` reads from after [`wire_legacy_slot`] runs.
const RESERVED_NON_ARTIFACT_DIRS: &[&str] = &["tantivy", "lance"];

/// Scan `<indexes_root>/` for directories lacking the `.ok` marker and
/// remove them. Called at startup so a mid-extract crash doesn't leave
/// half-written index dirs lying around.
///
/// Also removes stray `.partial` files under `<indexes_root>/.tmp/`.
pub fn reap_stale(indexes_root: &Path) -> Result<()> {
    if !indexes_root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(indexes_root)
        .with_context(|| format!("listing {}", indexes_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // `.tmp/` holds in-flight extractions + partial downloads.
        // Everything in it is safe to delete - if the fetcher were
        // still running against it, it wouldn't have gotten past a
        // cold start.
        if name == ".tmp" {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        // Skip the legacy slot containers - they hold per-slot index
        // dirs populated by `wire_legacy_slot`, not mounted artifacts.
        if RESERVED_NON_ARTIFACT_DIRS.contains(&name) {
            continue;
        }
        if path.is_dir() && !path.join(MOUNT_OK_MARKER).exists() {
            tracing::warn!(
                "reaping stale index dir without .ok marker: {}",
                path.display()
            );
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    Ok(())
}

/// Map a manifest's `hytale_patchline` to a [`Slot`]. Defaults to
/// [`Slot::Release`] when the field is missing or unrecognized - the
/// failure mode (rendering as "release" in the UI) is the safer default
/// than crashing the mount because of a free-form manifest field.
fn slot_for_patchline(patchline: Option<&str>) -> Slot {
    match patchline {
        Some("pre-release") => Slot::PreRelease,
        _ => Slot::Release,
    }
}

/// Bridge a freshly-mounted artifact into the legacy slot layout that
/// `SearchCatalog::search` (`commands::search`) currently reads.
///
/// `verify_and_mount` lays the artifact down at
/// `<indexes_root>/<build_id>/{tantivy,lance,symbols.sqlite,…}/`. The desktop
/// search path still reads from `<indexes_root>/tantivy/<slot>/` and
/// `<indexes_root>/lance/<slot>/`. Until the catalog/search code is
/// upgraded to address indexes by `build_id` (wiring), this
/// shim moves `tantivy/` and `lance/` from the build dir into the legacy
/// slot paths and writes the `atlas-meta.json` readiness file the
/// indexer's `summarize_slot` + `SearchCatalog::ensure` both look for.
///
/// Move (not copy) is intentional - copying would double the on-disk
/// footprint of every fetch. The `.ok` marker stays on the build_id
/// dir so [`reap_stale`] still recognises it as a completed mount.
pub fn wire_legacy_slot(mounted: &MountedArtifact, indexes_root: &Path) -> Result<()> {
    let slot = slot_for_patchline(mounted.manifest.hytale_patchline.as_deref());

    // Source: <indexes_root>/<build_id>/{tantivy,lance,javadocs}/
    let src_tantivy = mounted.mounted_at.join("tantivy");
    let src_lance = mounted.mounted_at.join("lance");
    let src_javadocs = mounted.mounted_at.join("javadocs");

    // Destination: <indexes_root>/{tantivy,lance,javadocs}/<slot>/
    let dst_tantivy = indexes_root.join("tantivy").join(slot.as_str());
    let dst_lance = indexes_root.join("lance").join(slot.as_str());
    let dst_javadocs = indexes_root.join("javadocs").join(slot.as_str());

    move_dir_replacing(&src_tantivy, &dst_tantivy)
        .with_context(|| format!("wiring tantivy → {}", dst_tantivy.display()))?;
    move_dir_replacing(&src_lance, &dst_lance)
        .with_context(|| format!("wiring lance → {}", dst_lance.display()))?;
    // Javadocs are optional - older artifacts (pre default-on Hypixel
    // docs) won't have a `javadocs/` payload. Skip silently when absent
    // so legacy artifacts still mount.
    if src_javadocs.is_dir() {
        move_dir_replacing(&src_javadocs, &dst_javadocs)
            .with_context(|| format!("wiring javadocs → {}", dst_javadocs.display()))?;
    }

    // Write atlas-meta.json so SearchCatalog::ensure_id and
    // indexer::summarize_slot both see the slot as ready.
    //
    // The indexer wrote a real atlas-meta.json into `tantivy/` at build
    // time with the actual `docs` count, `indexed_at`, and
    // `decompile_mtime`. Those moved into `dst_tantivy` along with the
    // rest of the directory above, so prefer them over the manifest's
    // build-time values - the manifest doesn't carry a doc count and
    // hardcoding zero made the BranchCard show "0 files" forever.
    //
    // Manifest-sourced fields (signing fingerprint, schema_version,
    // patchline, etc.) still win because they're the artifact's actual
    // identity, not whatever the indexer's local snapshot recorded.
    let m = &mounted.manifest;
    let existing = IndexMetadata::read(&dst_tantivy);
    let (docs, indexed_at, decompile_mtime) = match &existing {
        Some(prev) => (prev.docs, prev.indexed_at.clone(), prev.decompile_mtime.clone()),
        None => (0, m.created_at.clone(), m.created_at.clone()),
    };
    let meta = IndexMetadata {
        indexed_at,
        docs,
        decompile_mtime,
        hytale_impl_version: m.hytale_impl_version.clone(),
        hytale_patchline: m.hytale_patchline.clone(),
        vineflower_version: m.vineflower_version.clone(),
        chunker_version: m.chunker_version.clone(),
        embedder_id: m.embedder_id.clone(),
        embedder_dim: m.embedder_dim,
        schema_version: m.schema_version,
        min_client_version: m.min_client_version.clone(),
        created_at: m.created_at.clone(),
        signing_pubkey_fingerprint: m.signing_pubkey_fingerprint.clone(),
    };
    meta.write(&dst_tantivy)
        .with_context(|| format!("writing atlas-meta.json under {}", dst_tantivy.display()))?;

    Ok(())
}

/// `rename` `src` → `dst`, blowing away `dst` first if it exists. Falls
/// back to a recursive copy + delete when rename crosses devices (the
/// `EXDEV` case Windows raises as `ERROR_NOT_SAME_DEVICE`). Both source
/// and destination live under the same `<indexes_root>` in practice, so
/// the fallback is unlikely but cheap insurance.
fn move_dir_replacing(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        bail!("source directory missing: {}", src.display());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if dst.exists() {
        std::fs::remove_dir_all(dst)
            .with_context(|| format!("removing existing {}", dst.display()))?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    // Cross-device or other rename failure: copy then delete.
    copy_dir_recursive(src, dst)?;
    std::fs::remove_dir_all(src)
        .with_context(|| format!("removing source after copy: {}", src.display()))?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src)
        .with_context(|| format!("reading {}", src.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying {} → {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Returns true if the dir is a fully-extracted, hash-verified mount.
#[allow(dead_code)]
pub fn is_mounted(index_dir: &Path) -> bool {
    index_dir.is_dir() && index_dir.join(MOUNT_OK_MARKER).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetcher::artifact::{pack, FileEntry, PackRequest};
    use crate::fetcher::manifest::Manifest;
    use crate::fetcher::signing::{generate_keypair, sign_manifest};
    use tempfile::TempDir;

    fn sample_manifest(fp: String) -> Manifest {
        Manifest {
            build_id: "release-89796e57b".into(),
            hytale_impl_version: "2026.03.26-89796e57b".into(),
            hytale_patchline: Some("release".into()),
            vineflower_version: "1.11.2".into(),
            chunker_version: "1.0.0".into(),
            schema_version: SCHEMA_VERSION,
            embedder_id: "bge-small-en-v1.5".into(),
            embedder_dim: 384,
            min_client_version: "0.0.1".into(),
            signing_pubkey_fingerprint: fp,
            created_at: "2026-04-22T00:00:00.000Z".into(),
            sha256sums_sha256: "0".repeat(64),
        }
    }

    fn write_file(dir: &Path, rel: &str, bytes: &[u8]) -> FileEntry {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&abs, bytes).unwrap();
        FileEntry {
            rel_path: rel.to_string(),
            abs_path: abs,
        }
    }

    /// Build a real signed artifact using a fresh keypair, then mount
    /// it. Exercises the complete pipeline (verify digests, verify
    /// signature, extract, atomic rename, `.ok` marker).
    #[test]
    fn signed_artifact_roundtrips_through_mount() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let files = vec![
            write_file(&staging, "tantivy/meta.json", b"{\"segments\":[]}"),
            write_file(&staging, "lance/_versions/1.manifest", b"lance manifest"),
            write_file(&staging, "symbols.sqlite", b"sqlite\0\0"),
        ];

        let (mut signing_key, verifying_key) = generate_keypair();
        let pubkey_bytes = *verifying_key.as_bytes();
        let fp = fingerprint(&pubkey_bytes).unwrap();

        // Two-pass sign, mirroring atlas-build:
        let unsigned_path = tmp.path().join("unsigned.tar.zst");
        let finalized = pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(fp.clone()),
                signature: None,
            },
            &unsigned_path,
        )
        .unwrap();
        let manifest_bytes = finalized.to_bytes().unwrap();
        let sig = sign_manifest(&mut signing_key, &manifest_bytes);

        let signed_path = tmp.path().join("signed.tar.zst");
        pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(fp.clone()),
                signature: Some(sig),
            },
            &signed_path,
        )
        .unwrap();

        let indexes_root = tmp.path().join("indexes");
        let mounted = verify_and_mount_with_pubkey(
            &signed_path,
            &indexes_root,
            &pubkey_bytes,
            &NoProgress,
        )
        .expect("verify_and_mount_with_pubkey");

        assert_eq!(mounted.build_id, "release-89796e57b");
        assert!(mounted.mounted_at.ends_with("release-89796e57b"));
        assert!(
            mounted.mounted_at.join(MOUNT_OK_MARKER).is_file(),
            ".ok marker must exist after a successful mount"
        );
        assert!(
            mounted.mounted_at.join("symbols.sqlite").is_file(),
            "payload file must land at the expected path"
        );
        assert!(is_mounted(&mounted.mounted_at));
    }

    #[test]
    fn mount_rejects_wrong_pubkey() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let files = vec![write_file(&staging, "tantivy/meta.json", b"{}")];

        let (mut signer, verifier) = generate_keypair();
        let fp = fingerprint(verifier.as_bytes()).unwrap();

        let unsigned_path = tmp.path().join("u.tar.zst");
        let finalized = pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(fp.clone()),
                signature: None,
            },
            &unsigned_path,
        )
        .unwrap();
        let manifest_bytes = finalized.to_bytes().unwrap();
        let sig = sign_manifest(&mut signer, &manifest_bytes);

        let signed_path = tmp.path().join("s.tar.zst");
        pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(fp),
                signature: Some(sig),
            },
            &signed_path,
        )
        .unwrap();

        // Different keypair: signature verification must fail.
        let (_, other_verifier) = generate_keypair();
        let other_pubkey = *other_verifier.as_bytes();
        let err = verify_and_mount_with_pubkey(
            &signed_path,
            &tmp.path().join("indexes"),
            &other_pubkey,
            &NoProgress,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("signature verification failed")
                || msg.contains("fingerprint"),
            "expected signature/fingerprint error, got: {msg}"
        );
    }

    #[test]
    fn mount_rejects_unsigned_artifact() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let files = vec![write_file(&staging, "tantivy/meta.json", b"{}")];
        let out = tmp.path().join("u.tar.zst");
        pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(String::new()),
                signature: None,
            },
            &out,
        )
        .unwrap();

        let (_, verifier) = generate_keypair();
        let err = verify_and_mount_with_pubkey(
            &out,
            &tmp.path().join("indexes"),
            verifier.as_bytes(),
            &NoProgress,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unsigned"));
    }

    #[test]
    fn reap_stale_removes_dirs_without_ok_marker() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("indexes");
        std::fs::create_dir_all(root.join("good")).unwrap();
        std::fs::write(root.join("good").join(MOUNT_OK_MARKER), b"ok\n").unwrap();
        std::fs::create_dir_all(root.join("stale")).unwrap();
        std::fs::write(root.join("stale/data.bin"), b"junk").unwrap();
        std::fs::create_dir_all(root.join(".tmp/in-flight")).unwrap();
        std::fs::write(root.join(".tmp/in-flight/data"), b"junk").unwrap();

        reap_stale(&root).unwrap();

        assert!(root.join("good").is_dir(), "mounted dir must survive");
        assert!(!root.join("stale").exists(), "stale dir must be removed");
        assert!(
            !root.join(".tmp").exists(),
            ".tmp must be wiped on startup"
        );
    }

    #[test]
    fn version_at_least_monotonic() {
        assert!(version_at_least("0.1.0", "0.0.9"));
        assert!(version_at_least("1.0.0", "0.9.9"));
        assert!(version_at_least("0.1.0", "0.1.0"));
        assert!(!version_at_least("0.0.9", "0.1.0"));
        assert!(!version_at_least("1.0.0", "1.0.1"));
    }
}
