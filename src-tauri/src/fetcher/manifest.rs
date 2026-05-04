//! Artifact manifest format.
//!
//! Every `.tar.zst` artifact ships two anchor files at its root:
//!
//!   - `manifest.json`  - human-readable, machine-parseable: the compound
//!     version key, file list, and SHA256 of the `SHA256SUMS` digest
//!     file. This is what Ed25519 signing covers.
//!   - `SHA256SUMS`     - one hex-digest+path line per file in the
//!     tarball (other than `manifest.json` / `SHA256SUMS` / `manifest
//!     .json.sig` themselves). Verified per-file during streaming
//!     extract.
//!
//! The signature, when present, lives at `manifest.json.sig` and covers
//! the bytes of `manifest.json` exactly.
//!
//! # Trust chain
//!
//! 1. Client loads `manifest.json` and `manifest.json.sig`.
//! 2. Ed25519-verify the signature against the embedded pubkey (3.E).
//! 3. Hash `SHA256SUMS` on disk; compare to `manifest.sha256sums_sha256`.
//! 4. Extract each file; check its running SHA256 against the digest
//!    line in `SHA256SUMS`.
//!
//! If any step fails, mount aborts and nothing is written to the live
//! indexes directory (extraction happens to a `.tmp/` staging path).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Filename of the manifest inside the tarball.
pub const MANIFEST_FILENAME: &str = "manifest.json";
/// Filename of the Ed25519 detached signature over `manifest.json`.
pub const SIGNATURE_FILENAME: &str = "manifest.json.sig";
/// Filename of the per-file digest listing.
pub const SHA256SUMS_FILENAME: &str = "SHA256SUMS";

/// Files that live at the tarball root and are NOT themselves listed in
/// `SHA256SUMS` (they sit outside the per-file digest loop because the
/// manifest covers `SHA256SUMS` and the signature covers the manifest).
pub const DIGEST_EXEMPT: &[&str] = &[
    MANIFEST_FILENAME,
    SIGNATURE_FILENAME,
    SHA256SUMS_FILENAME,
];

/// Artifact manifest written at the root of every `.tar.zst` blob.
///
/// Every field is explicit - nothing is silently defaulted on write.
/// Reads still tolerate older manifests by letting serde default missing
/// fields; this keeps the client forward-compatible with signing
/// additions in without a format bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Compound identifier for the artifact, e.g.
    /// `release-89796e57b` or `pre-release-8a1c2d4e7`.
    pub build_id: String,

    /// Hytale `Implementation-Version`, e.g. `2026.03.26-89796e57b`.
    pub hytale_impl_version: String,

    /// Hytale `Implementation-Patchline` - `release` or `pre-release`.
    pub hytale_patchline: Option<String>,

    /// Vineflower version used to produce the shipped decompile.
    pub vineflower_version: String,

    /// Chunker logic version - bumped independently of Vineflower.
    pub chunker_version: String,

    /// Artifact format version. Client refuses to mount if this is
    /// newer than the client knows about.
    pub schema_version: u32,

    /// Embedder model identifier (e.g. `bge-small-en-v1.5`).
    pub embedder_id: String,

    /// Embedding dimensionality. Client checks this matches its own
    /// runtime embedder before the vector store is usable.
    pub embedder_dim: u32,

    /// Earliest Atlas client version that can mount this artifact.
    pub min_client_version: String,

    /// Hex-encoded first 16 bytes of the Ed25519 signing pubkey used
    /// to sign this artifact. Empty string means "unsigned" - the
    /// client refuses to mount an unsigned artifact in production.
    pub signing_pubkey_fingerprint: String,

    /// ISO-8601 timestamp the builder emitted this artifact.
    pub created_at: String,

    /// Lowercase hex SHA256 of `SHA256SUMS`. This is how the manifest
    /// ties itself to the per-file digests: if anyone rewrites
    /// `SHA256SUMS`, this hash diverges and mount fails.
    pub sha256sums_sha256: String,
}

impl Manifest {
    /// Deserialize from an in-memory byte buffer. Kept separate from
    /// [`Self::read`] so the fetcher can hash the exact bytes before
    /// parsing (signature verification runs against the raw bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("parsing manifest.json")
    }

    /// Serialize to pretty JSON. Pretty so the manifest reads cleanly
    /// in a browser when fetched straight off GH Releases.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).context("serializing manifest.json")
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading manifest at {}", path.display()))?;
        Self::from_bytes(&bytes)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let bytes = self.to_bytes()?;
        std::fs::write(path, bytes)
            .with_context(|| format!("writing manifest to {}", path.display()))?;
        Ok(())
    }
}

/// Per-file SHA256 digest line, as it appears in `SHA256SUMS`.
///
/// Wire format: `<hex64>  <relpath>\n` - two spaces between digest and
/// path, matching the output of `sha256sum` so the file stays
/// inspectable with standard tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestEntry {
    pub sha256_hex: String,
    pub rel_path: String,
}

/// The full `SHA256SUMS` listing. Stored sorted by path so the digest of
/// the listing itself is reproducible.
#[derive(Debug, Clone, Default)]
pub struct Sha256Sums {
    entries: Vec<DigestEntry>,
}

impl Sha256Sums {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) the digest for `rel_path`. The final write sorts
    /// entries by path, so insertion order is irrelevant.
    pub fn insert(&mut self, rel_path: impl Into<String>, sha256_hex: impl Into<String>) {
        let rel_path = rel_path.into();
        let sha256_hex = sha256_hex.into();
        if let Some(existing) = self.entries.iter_mut().find(|e| e.rel_path == rel_path) {
            existing.sha256_hex = sha256_hex;
            return;
        }
        self.entries.push(DigestEntry {
            rel_path,
            sha256_hex,
        });
    }

    pub fn entries(&self) -> &[DigestEntry] {
        &self.entries
    }

    /// Build a lookup map for verification. Sorted into a BTreeMap so
    /// verify-loop ordering is deterministic.
    pub fn as_map(&self) -> BTreeMap<&str, &str> {
        self.entries
            .iter()
            .map(|e| (e.rel_path.as_str(), e.sha256_hex.as_str()))
            .collect()
    }

    /// Render to the on-disk wire format: sorted lines, LF terminators.
    /// Matching `sha256sum`'s "text-mode" output means external tools
    /// can sanity-check the file without special handling.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut sorted = self.entries.clone();
        sorted.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        let mut out = Vec::new();
        for entry in &sorted {
            writeln!(&mut out, "{}  {}", entry.sha256_hex, entry.rel_path)
                .expect("writing to Vec<u8> is infallible");
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut entries = Vec::new();
        for (lineno, line) in BufReader::new(bytes).lines().enumerate() {
            let line = line.with_context(|| format!("reading SHA256SUMS line {lineno}"))?;
            if line.trim().is_empty() {
                continue;
            }
            // sha256sum format: "<hex64>  <path>" (two spaces).
            let (digest, rest) = line
                .split_once("  ")
                .ok_or_else(|| anyhow!("SHA256SUMS line {lineno}: missing two-space separator"))?;
            if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(anyhow!(
                    "SHA256SUMS line {lineno}: digest must be 64 hex chars, got `{digest}`"
                ));
            }
            entries.push(DigestEntry {
                sha256_hex: digest.to_ascii_lowercase(),
                rel_path: rest.to_string(),
            });
        }
        Ok(Self { entries })
    }

    /// Compute the SHA256 of this listing's on-disk bytes. This is what
    /// the manifest's `sha256sums_sha256` field stores, so the manifest
    /// binds to the full digest listing.
    pub fn self_sha256_hex(&self) -> String {
        sha256_hex(&self.to_bytes())
    }
}

/// SHA256 a file on disk, streaming so we don't peak on large
/// `.tar.zst` payloads.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading {} for hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Streaming SHA256: update the hasher as bytes flow through the
/// extractor, then call [`finish`] at EOF.
pub struct StreamingHasher {
    hasher: Sha256,
}

impl StreamingHasher {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    pub fn finish(self) -> String {
        hex::encode(self.hasher.finalize())
    }
}

impl Default for StreamingHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for StreamingHasher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            signing_pubkey_fingerprint: "deadbeefdeadbeef".into(),
            created_at: "2026-04-22T12:00:00.000Z".into(),
            sha256sums_sha256: "0".repeat(64),
        }
    }

    #[test]
    fn manifest_roundtrips() {
        let m = sample_manifest();
        let bytes = m.to_bytes().unwrap();
        let parsed = Manifest::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.build_id, m.build_id);
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.hytale_patchline.as_deref(), Some("release"));
    }

    #[test]
    fn manifest_tolerates_missing_optional_fields() {
        // An older/forward-emitted manifest might omit patchline. Serde
        // Option<String> handles this natively - this test locks that in.
        let minimal = serde_json::json!({
            "build_id": "x",
            "hytale_impl_version": "v",
            "vineflower_version": "1.11.2",
            "chunker_version": "1.0.0",
            "schema_version": 1,
            "embedder_id": "bge-small-en-v1.5",
            "embedder_dim": 384,
            "min_client_version": "0.1.0",
            "signing_pubkey_fingerprint": "",
            "created_at": "2026-04-22T00:00:00.000Z",
            "sha256sums_sha256": "0".repeat(64),
        });
        let bytes = serde_json::to_vec(&minimal).unwrap();
        let m = Manifest::from_bytes(&bytes).unwrap();
        assert_eq!(m.hytale_patchline, None);
    }

    #[test]
    fn sha256sums_roundtrips_and_sorts() {
        let mut sums = Sha256Sums::new();
        sums.insert("zzz.txt", "a".repeat(64));
        sums.insert("aaa.txt", "b".repeat(64));
        sums.insert("mmm.txt", "c".repeat(64));
        let wire = sums.to_bytes();
        let text = String::from_utf8(wire.clone()).unwrap();
        // Sorted output: aaa, mmm, zzz.
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with("aaa.txt"));
        assert!(lines[1].ends_with("mmm.txt"));
        assert!(lines[2].ends_with("zzz.txt"));

        let parsed = Sha256Sums::from_bytes(&wire).unwrap();
        assert_eq!(parsed.entries().len(), 3);
    }

    #[test]
    fn sha256sums_rejects_malformed() {
        // Missing two-space separator.
        let bad = b"abcd file.txt\n".to_vec();
        assert!(Sha256Sums::from_bytes(&bad).is_err());

        // Bad digest length.
        let bad2 = b"abc  file.txt\n".to_vec();
        assert!(Sha256Sums::from_bytes(&bad2).is_err());
    }

    #[test]
    fn sha256sums_self_hash_stable() {
        // The listing's own hash must be path-order-independent, so two
        // callers building SHA256SUMS in different insertion orders get
        // the same manifest.sha256sums_sha256.
        let mut a = Sha256Sums::new();
        a.insert("b.txt", "0".repeat(64));
        a.insert("a.txt", "1".repeat(64));
        let mut b = Sha256Sums::new();
        b.insert("a.txt", "1".repeat(64));
        b.insert("b.txt", "0".repeat(64));
        assert_eq!(a.self_sha256_hex(), b.self_sha256_hex());
    }

    #[test]
    fn streaming_hasher_matches_one_shot() {
        let data = b"atlas-artifact-bytes";
        let one_shot = sha256_hex(data);
        let mut streaming = StreamingHasher::new();
        streaming.write_all(data).unwrap();
        assert_eq!(one_shot, streaming.finish());
    }
}
