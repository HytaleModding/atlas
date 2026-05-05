//! Tar+zstd packing and verification for index artifacts.
//!
//! ## Layout inside the `.tar.zst`
//!
//! ```text
//! manifest.json          // covered by manifest.json.sig
//! manifest.json.sig      // Ed25519 detached signature (empty in 3.D, real in 3.E)
//! SHA256SUMS             // digests for everything below; covered by manifest
//! tantivy/               // Tantivy segment files
//! lance/                 // Lance table dirs
//! symbols.sqlite         // symbol sidecar
//! ```
//!
//! Everything under the two content roots (`tantivy/`, `lance/`) plus
//! `symbols.sqlite` gets a `SHA256SUMS` entry.
//!
//! ## What the artifact does NOT contain
//!
//! The artifact is intentionally an **index-only** distribution. It
//! does not ship `decompile/` (the decompiled Java source tree), raw
//! chunk text, JAR contents, or anything else that would constitute
//! redistribution of Hytale source. Clients reconstruct source on
//! demand by running Vineflower locally against the JAR already
//! present on the user's machine. See
//! `docs/legal-spec/what-the-artifact-contains.md` for the full
//! policy and the audit trail behind this decision.
//!
//! ## Pack side (`atlas-build`)
//!
//! The builder walks the staging directory, streams each file into the
//! tar writer while also feeding a SHA256 hasher; at the end it writes
//! `SHA256SUMS`, stamps its SHA256 into the manifest, writes
//! `manifest.json`, and (3.E) appends `manifest.json.sig`. Output is
//! zstd-compressed at the `zstd::stream` level pinned to what the plan
//! specifies.
//!
//! ## Verify side (client)
//!
//! The client reads the manifest first, checks signature + fingerprint
//! + schema/min-client-version compatibility, then streams the rest of
//! the archive - hashing each entry as it extracts and comparing to
//! `SHA256SUMS` before committing the file to disk. Any mismatch aborts
//! the whole mount.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::manifest::{
    sha256_hex, Manifest, Sha256Sums, DIGEST_EXEMPT, MANIFEST_FILENAME, SHA256SUMS_FILENAME,
    SIGNATURE_FILENAME,
};

/// zstd compression level used when packing artifacts. Level 19 is the
/// plan's pinned value (notes) - it trades meaningful build
/// time for ~6-8× compression on decompile text.
pub const ZSTD_LEVEL: i32 = 19;

/// One file pending inclusion in an artifact.
///
/// Kept as an explicit staging list rather than a "walk a directory"
/// helper so the builder can choose exactly what to ship (skipping
/// `.DS_Store`, Lance write-ahead logs, etc.) without the layer knowing.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path inside the tar (forward slashes, no leading `/`).
    pub rel_path: String,
    /// Absolute source path on disk.
    pub abs_path: PathBuf,
}

/// Inputs needed to pack an artifact. `manifest` has its
/// `sha256sums_sha256` field overwritten by [`pack`] so callers don't
/// have to compute it ahead of time.
pub struct PackRequest<'a> {
    /// All files to include under the three content roots. Must NOT
    /// include `manifest.json` / `SHA256SUMS` / `manifest.json.sig` -
    /// those are synthesized.
    pub files: &'a [FileEntry],
    /// Manifest body - `sha256sums_sha256` is filled in by the packer.
    pub manifest: Manifest,
    /// Optional signature bytes. `None` emits an empty
    /// `manifest.json.sig` so the tarball layout stays stable across
    /// signed and unsigned builds.
    pub signature: Option<Vec<u8>>,
}

/// One required entry an artifact's staging directory must contain
/// before pack is allowed to proceed. Used by [`validate_staging`] to
/// enforce that every artifact ships a complete payload.
///
/// The variants matter because some staging entries are directories
/// whose internal layout we don't want to police here (Tantivy and
/// Lance change segment files between versions), while others are
/// single files where mere existence is the only sane check (e.g.
/// `symbols.sqlite`). Non-empty is the universal "did the upstream
/// step actually run?" signal.
#[derive(Debug)]
pub enum RequiredEntry {
    /// File at this staging-relative path must exist and be non-empty.
    File(&'static str),
    /// Directory at this staging-relative path must exist and contain
    /// at least one regular file (recursively).
    NonEmptyDir(&'static str),
}

/// Declarative spec for what an artifact's staging directory MUST
/// contain. Closes the producer-side completeness gap that previously
/// let `atlas-build pack` ship index artifacts with `symbols.sqlite`
/// missing - the old code walked whatever was in the staging tree and
/// trusted the upstream pipeline to have produced everything. Now the
/// spec is the single source of truth.
#[derive(Debug)]
pub struct ArtifactSpec {
    pub required: Vec<RequiredEntry>,
}

impl ArtifactSpec {
    /// Spec for an `atlas-build` index artifact. Captures the three
    /// payload roots the search/diff client expects: a Tantivy index,
    /// a Lance vector store, and the symbols sidecar that powers
    /// `find_symbol` and the diff tracker. Optional content (HM docs,
    /// Hypixel javadocs) is intentionally NOT in here - those are
    /// gated by their own pipeline flags and missing them is not an
    /// error condition.
    pub fn index_default() -> Self {
        Self {
            required: vec![
                RequiredEntry::NonEmptyDir("tantivy"),
                RequiredEntry::NonEmptyDir("lance"),
                RequiredEntry::File("tantivy/symbols.sqlite"),
            ],
        }
    }
}

/// Verify every required entry in `spec` is present under `staging`.
/// Reports all missing entries in a single error - users running
/// pipelines should see the full list of what to fix, not chase one
/// at a time. Called from [`crate::bin`]'s pack command before [`pack`]
/// so an incomplete staging dir aborts before any `.tar.zst` is written.
pub fn validate_staging(spec: &ArtifactSpec, staging: &Path) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();
    for entry in &spec.required {
        match entry {
            RequiredEntry::File(rel) => {
                let p = staging.join(rel);
                match std::fs::metadata(&p) {
                    Ok(m) if m.is_file() && m.len() > 0 => {}
                    Ok(m) if m.is_file() => missing.push(format!("{rel} (empty)")),
                    Ok(_) => missing.push(format!("{rel} (not a file)")),
                    Err(_) => missing.push(rel.to_string()),
                }
            }
            RequiredEntry::NonEmptyDir(rel) => {
                let p = staging.join(rel);
                match std::fs::metadata(&p) {
                    Ok(m) if m.is_dir() => {
                        if !dir_has_any_file(&p)? {
                            missing.push(format!("{rel}/ (empty)"));
                        }
                    }
                    Ok(_) => missing.push(format!("{rel}/ (not a directory)")),
                    Err(_) => missing.push(format!("{rel}/")),
                }
            }
        }
    }
    if !missing.is_empty() {
        bail!(
            "staging dir {} is missing required artifact contents: {}",
            staging.display(),
            missing.join(", ")
        );
    }
    Ok(())
}

/// Recursively check whether `dir` contains at least one regular file.
/// Used by [`validate_staging`] to enforce non-empty directories
/// without policing their internal layout.
fn dir_has_any_file(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("scanning {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry under {}", dir.display()))?;
        let ft = entry
            .file_type()
            .with_context(|| format!("file_type for {}", entry.path().display()))?;
        if ft.is_file() {
            return Ok(true);
        }
        if ft.is_dir() && dir_has_any_file(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pack an artifact to `out_path`.
///
/// Returns the finalized [`Manifest`] with `sha256sums_sha256` computed
/// so the caller (atlas-build) can log / attach it to the GH Release.
///
/// The function is fully in-memory for the `SHA256SUMS` + manifest
/// bytes; content files stream through a buffered reader so the peak
/// memory stays bounded by file size.
pub fn pack(req: PackRequest<'_>, out_path: &Path) -> Result<Manifest> {
    // Sanity: refuse to let the caller shadow one of the synthesized
    // files. A silent path clash here would corrupt verification.
    for f in req.files {
        if DIGEST_EXEMPT.contains(&f.rel_path.as_str()) {
            bail!(
                "file entry collides with reserved artifact name: {}",
                f.rel_path
            );
        }
    }

    // First pass: hash every payload file into SHA256SUMS. We can't
    // stream-write the tar yet because SHA256SUMS content isn't known
    // until all files are hashed, and tar doesn't support back-patching.
    let mut sums = Sha256Sums::new();
    for f in req.files {
        let digest = super::manifest::sha256_file(&f.abs_path)?;
        sums.insert(f.rel_path.clone(), digest);
    }
    let sha256sums_bytes = sums.to_bytes();
    let sha256sums_sha = sha256_hex(&sha256sums_bytes);

    // Finalize the manifest by binding it to SHA256SUMS.
    let mut manifest = req.manifest;
    manifest.sha256sums_sha256 = sha256sums_sha;
    let manifest_bytes = manifest.to_bytes()?;

    // Signature: empty `Vec` if the caller didn't provide one (3.D
    // default). Still written so the tarball shape is stable.
    let signature_bytes = req.signature.unwrap_or_default();

    // Second pass: stream everything into a zstd-wrapped tar writer.
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating artifact parent dir {}", parent.display()))?;
    }
    let out_file = File::create(out_path)
        .with_context(|| format!("creating artifact at {}", out_path.display()))?;
    let encoder = zstd::Encoder::new(out_file, ZSTD_LEVEL)
        .context("creating zstd encoder")?
        .auto_finish();
    let mut tar_builder = tar::Builder::new(encoder);

    // manifest.json + SHA256SUMS + manifest.json.sig first so a
    // streaming verifier can short-circuit before touching payload.
    append_bytes(&mut tar_builder, MANIFEST_FILENAME, &manifest_bytes)?;
    append_bytes(&mut tar_builder, SIGNATURE_FILENAME, &signature_bytes)?;
    append_bytes(&mut tar_builder, SHA256SUMS_FILENAME, &sha256sums_bytes)?;

    // Then payload files, sorted by rel_path for reproducibility.
    let mut sorted_files: Vec<&FileEntry> = req.files.iter().collect();
    sorted_files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    for f in sorted_files {
        tar_builder
            .append_path_with_name(&f.abs_path, &f.rel_path)
            .with_context(|| format!("appending {} to tar", f.rel_path))?;
    }

    tar_builder.finish().context("finalizing tar stream")?;
    Ok(manifest)
}

fn append_bytes<W: Write>(tar: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_path(name)?;
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, Cursor::new(bytes))
        .with_context(|| format!("appending {name} to tar"))?;
    Ok(())
}

/// Outcome of a verify-only read of an artifact.
#[derive(Debug)]
pub struct VerifiedArtifact {
    pub manifest: Manifest,
    /// Raw bytes of `manifest.json` - Ed25519 verify runs against these.
    pub manifest_bytes: Vec<u8>,
    /// Raw bytes of `manifest.json.sig`. Empty for unsigned artifacts.
    pub signature_bytes: Vec<u8>,
    /// Parsed SHA256SUMS content.
    pub sums: Sha256Sums,
    /// Number of non-exempt payload files whose digest was confirmed.
    pub verified_files: usize,
}

/// Verify a `.tar.zst` in place on disk without extracting payload.
///
/// Streams the archive once, checking that:
///   - `manifest.json` parses.
///   - `SHA256SUMS` hashes to `manifest.sha256sums_sha256`.
///   - Every payload file's streaming SHA256 matches its digest line.
///   - Every digest line corresponds to a file actually present.
///
/// Signature verification is the caller's concern - this
/// module just hands back the raw bytes of `manifest.json` +
/// `manifest.json.sig` so the caller can Ed25519-verify them before
/// trusting anything else in the return.
pub fn verify(archive_path: &Path) -> Result<VerifiedArtifact> {
    let file = File::open(archive_path)
        .with_context(|| format!("opening artifact at {}", archive_path.display()))?;
    let decoder = zstd::Decoder::new(file).context("initializing zstd decoder")?;
    let mut archive = tar::Archive::new(decoder);

    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut signature_bytes: Option<Vec<u8>> = None;
    let mut sha256sums_bytes: Option<Vec<u8>> = None;
    let mut observed: Vec<(String, String)> = Vec::new();

    for entry in archive.entries().context("reading tar entries")? {
        let mut entry = entry.context("iterating tar entry")?;
        let path = entry.path().context("reading entry path")?.into_owned();
        let rel_path = path.to_string_lossy().replace('\\', "/");

        // Directory entries have size 0; skip them - we don't digest
        // empty dirs, only files.
        if entry.header().entry_type().is_dir() {
            continue;
        }

        match rel_path.as_str() {
            MANIFEST_FILENAME => {
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .context("reading manifest.json")?;
                manifest_bytes = Some(buf);
            }
            SIGNATURE_FILENAME => {
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .context("reading manifest.json.sig")?;
                signature_bytes = Some(buf);
            }
            SHA256SUMS_FILENAME => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).context("reading SHA256SUMS")?;
                sha256sums_bytes = Some(buf);
            }
            _ => {
                // Streaming SHA256 - avoid holding the file body in RAM.
                let mut hasher = super::manifest::StreamingHasher::new();
                std::io::copy(&mut entry, &mut hasher)
                    .with_context(|| format!("hashing entry {rel_path}"))?;
                observed.push((rel_path, hasher.finish()));
            }
        }
    }

    let manifest_bytes =
        manifest_bytes.ok_or_else(|| anyhow!("manifest.json missing from artifact"))?;
    let sha256sums_bytes =
        sha256sums_bytes.ok_or_else(|| anyhow!("SHA256SUMS missing from artifact"))?;
    let signature_bytes = signature_bytes.unwrap_or_default();

    let manifest = Manifest::from_bytes(&manifest_bytes)?;

    let observed_sha = sha256_hex(&sha256sums_bytes);
    if observed_sha != manifest.sha256sums_sha256 {
        bail!(
            "SHA256SUMS hash mismatch: manifest claims {}, computed {}",
            manifest.sha256sums_sha256,
            observed_sha
        );
    }

    let sums = Sha256Sums::from_bytes(&sha256sums_bytes)?;
    let expected = sums.as_map();

    // Cross-check: every observed payload file matches its expected
    // digest, and every digest line is backed by an observed file.
    for (rel_path, digest) in &observed {
        let expected_digest = expected.get(rel_path.as_str()).ok_or_else(|| {
            anyhow!("file `{rel_path}` present in tar but missing from SHA256SUMS")
        })?;
        if digest != expected_digest {
            bail!("digest mismatch for `{rel_path}`: expected {expected_digest}, got {digest}");
        }
    }
    let observed_set: std::collections::HashSet<&str> =
        observed.iter().map(|(p, _)| p.as_str()).collect();
    for (rel_path, _) in expected.iter() {
        if !observed_set.contains(rel_path) {
            bail!("`{rel_path}` listed in SHA256SUMS but missing from tar");
        }
    }

    Ok(VerifiedArtifact {
        manifest,
        manifest_bytes,
        signature_bytes,
        sums,
        verified_files: observed.len(),
    })
}

/// Rewind-style variant: consume an in-memory `.tar.zst` buffer. Used by
/// tests; production code should stream from disk via [`verify`].
#[cfg(test)]
pub fn verify_bytes(bytes: &[u8]) -> Result<VerifiedArtifact> {
    let temp = std::io::Cursor::new(bytes);
    let decoder = zstd::Decoder::new(temp).context("zstd decoder")?;
    let mut archive = tar::Archive::new(decoder);

    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut signature_bytes: Option<Vec<u8>> = None;
    let mut sha256sums_bytes: Option<Vec<u8>> = None;
    let mut observed: Vec<(String, String)> = Vec::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let rel_path = path.to_string_lossy().replace('\\', "/");
        if entry.header().entry_type().is_dir() {
            continue;
        }
        match rel_path.as_str() {
            MANIFEST_FILENAME => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                manifest_bytes = Some(buf);
            }
            SIGNATURE_FILENAME => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                signature_bytes = Some(buf);
            }
            SHA256SUMS_FILENAME => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                sha256sums_bytes = Some(buf);
            }
            _ => {
                let mut hasher = super::manifest::StreamingHasher::new();
                std::io::copy(&mut entry, &mut hasher)?;
                observed.push((rel_path, hasher.finish()));
            }
        }
    }

    let manifest_bytes = manifest_bytes.ok_or_else(|| anyhow!("manifest.json missing"))?;
    let sha256sums_bytes = sha256sums_bytes.ok_or_else(|| anyhow!("SHA256SUMS missing"))?;
    let manifest = Manifest::from_bytes(&manifest_bytes)?;
    if sha256_hex(&sha256sums_bytes) != manifest.sha256sums_sha256 {
        bail!("SHA256SUMS hash mismatch");
    }
    let sums = Sha256Sums::from_bytes(&sha256sums_bytes)?;
    let expected = sums.as_map();
    for (rel, digest) in &observed {
        let exp = expected
            .get(rel.as_str())
            .ok_or_else(|| anyhow!("file `{rel}` missing from SHA256SUMS"))?;
        if digest != exp {
            bail!("digest mismatch for `{rel}`");
        }
    }
    Ok(VerifiedArtifact {
        manifest,
        manifest_bytes,
        signature_bytes: signature_bytes.unwrap_or_default(),
        sums,
        verified_files: observed.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_manifest() -> Manifest {
        Manifest {
            build_id: "release-89796e57b".into(),
            hytale_impl_version: "2026.03.26-89796e57b".into(),
            hytale_patchline: Some("release".into()),
            vineflower_version: "1.11.2".into(),
            chunker_version: "1.0.0".into(),
            schema_version: 1,
            embedder_id: "bge-small-en-v1.5".into(),
            embedder_dim: 384,
            min_client_version: "0.1.0".into(),
            signing_pubkey_fingerprint: String::new(),
            created_at: "2026-04-22T00:00:00.000Z".into(),
            // pack() overwrites this before writing, but keep it a
            // placeholder so the struct is valid before the call.
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

    #[test]
    fn pack_then_verify_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let files = vec![
            write_file(&staging, "tantivy/seg_0.dat", b"tantivy seg bytes"),
            write_file(&staging, "tantivy/nested/seg_1.dat", b"tantivy seg bytes 2"),
            write_file(&staging, "tantivy/meta.json", b"{\"segments\":[]}"),
            write_file(&staging, "symbols.sqlite", b"sqlite\0\0\0"),
        ];
        let out = tmp.path().join("artifact.tar.zst");
        let manifest = pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(),
                signature: None,
            },
            &out,
        )
        .unwrap();

        assert!(out.exists(), "artifact file was not written");
        assert_ne!(
            manifest.sha256sums_sha256,
            "0".repeat(64),
            "pack() must overwrite the placeholder sha256sums_sha256"
        );

        let verified = verify(&out).unwrap();
        assert_eq!(verified.verified_files, 4);
        assert_eq!(
            verified.manifest.sha256sums_sha256,
            manifest.sha256sums_sha256
        );
        assert!(verified.signature_bytes.is_empty());
    }

    /// Build a hand-crafted archive using explicit zstd finish to make
    /// the frame-termination ordering unambiguous across platforms. The
    /// outer `auto_finish()` in production code works fine because the
    /// Builder drops at a predictable spot; in these tests we need the
    /// file closed *before* `verify` runs, so we finish explicitly.
    fn build_raw_archive(out_path: &Path, parts: &[(&str, &[u8])], payload: &FileEntry) {
        let f = File::create(out_path).unwrap();
        let enc = zstd::Encoder::new(f, ZSTD_LEVEL).unwrap();
        let mut tb = tar::Builder::new(enc);
        for (name, bytes) in parts {
            append_bytes(&mut tb, name, bytes).unwrap();
        }
        tb.append_path_with_name(&payload.abs_path, &payload.rel_path)
            .unwrap();
        // `into_inner` returns the zstd Encoder; `.finish()` writes the
        // zstd footer and returns the underlying File, which is then
        // dropped and flushed.
        let enc = tb.into_inner().unwrap();
        enc.finish().unwrap();
    }

    #[test]
    fn verify_rejects_sha256sums_tampering() {
        // Hand-build an archive where SHA256SUMS content doesn't match
        // the manifest's sha256sums_sha256. This is the exact failure
        // mode verify() must refuse to mount.
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let payload = write_file(&staging, "tantivy/seg_0.dat", b"orig\n");

        let mut sums = Sha256Sums::new();
        sums.insert(
            payload.rel_path.clone(),
            super::super::manifest::sha256_file(&payload.abs_path).unwrap(),
        );
        let sums_bytes = sums.to_bytes();

        // Manifest claims a *different* sha256sums_sha256 than what the
        // tarball actually contains.
        let mut manifest = sample_manifest();
        manifest.sha256sums_sha256 = "f".repeat(64);
        let manifest_bytes = manifest.to_bytes().unwrap();

        let bad_out = tmp.path().join("bad.tar.zst");
        build_raw_archive(
            &bad_out,
            &[
                (MANIFEST_FILENAME, &manifest_bytes),
                (SIGNATURE_FILENAME, &[]),
                (SHA256SUMS_FILENAME, &sums_bytes),
            ],
            &payload,
        );

        let err = verify(&bad_out).unwrap_err();
        assert!(
            format!("{err:#}").contains("SHA256SUMS hash mismatch"),
            "expected SHA256SUMS hash mismatch, got: {err:#}"
        );
    }

    #[test]
    fn verify_rejects_per_file_digest_tampering() {
        // Archive where SHA256SUMS self-hashes correctly but one of its
        // digest lines is wrong for the actual file bytes.
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let payload = write_file(&staging, "tantivy/seg_0.dat", b"orig\n");

        let mut sums = Sha256Sums::new();
        sums.insert(payload.rel_path.clone(), "a".repeat(64));
        let sums_bytes = sums.to_bytes();

        // Manifest binds correctly to this (lying) SHA256SUMS.
        let mut manifest = sample_manifest();
        manifest.sha256sums_sha256 = super::super::manifest::sha256_hex(&sums_bytes);
        let manifest_bytes = manifest.to_bytes().unwrap();

        let bad_out = tmp.path().join("bad.tar.zst");
        build_raw_archive(
            &bad_out,
            &[
                (MANIFEST_FILENAME, &manifest_bytes),
                (SIGNATURE_FILENAME, &[]),
                (SHA256SUMS_FILENAME, &sums_bytes),
            ],
            &payload,
        );

        let err = verify(&bad_out).unwrap_err();
        assert!(
            format!("{err:#}").contains("digest mismatch"),
            "expected per-file digest mismatch, got: {err:#}"
        );
    }

    #[test]
    fn pack_refuses_to_shadow_reserved_names() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let bad = write_file(&staging, "manifest.json", b"{}");
        let out = tmp.path().join("art.tar.zst");
        let err = pack(
            PackRequest {
                files: &[bad],
                manifest: sample_manifest(),
                signature: None,
            },
            &out,
        )
        .unwrap_err();
        assert!(err.to_string().contains("reserved"));
    }

    #[test]
    fn pack_carries_signature_bytes_through() {
        let tmp = TempDir::new().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let files = vec![write_file(&staging, "tantivy/meta.json", b"{}")];
        let out = tmp.path().join("art.tar.zst");
        let sig = b"fake-sig-bytes".to_vec();
        pack(
            PackRequest {
                files: &files,
                manifest: sample_manifest(),
                signature: Some(sig.clone()),
            },
            &out,
        )
        .unwrap();
        let verified = verify(&out).unwrap();
        assert_eq!(verified.signature_bytes, sig);
    }
}
