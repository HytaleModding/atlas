//! Ed25519 signing + verification for artifact manifests.
//!
//! ## Trust model
//!
//! The Hytale Modding signing keypair is generated out-of-band. The
//! **private key** lives only in HM's CI secrets (consumed by
//! `atlas-build` when packaging artifacts). The **public key** is
//! embedded in every Atlas client binary via
//! `src-tauri/signing/atlas-pubkey.hex` - compiled in at build time so
//! it can't be swapped out of a shipped installer.
//!
//! Signatures are Ed25519 detached sigs over the exact bytes of
//! `manifest.json`. The client:
//!
//! 1. Reads `manifest.json` bytes from the tarball.
//! 2. Reads `manifest.json.sig` bytes from the tarball.
//! 3. Calls [`verify_manifest`] with the embedded pubkey.
//! 4. Only if that succeeds does it trust anything else in the archive
//!    (SHA256SUMS, per-file digests, metadata).
//!
//! ## Key rotation
//!
//! To rotate: ship a new Atlas client with the new pubkey embedded.
//! Older clients refuse to mount artifacts signed by the new key -
//! which is a feature, not a bug: it creates natural upgrade pressure
//! without silently trusting a rotated identity.

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{
    ed25519::signature::SignerMut, Signature, SigningKey, Verifier, VerifyingKey,
    SECRET_KEY_LENGTH, SIGNATURE_LENGTH,
};

/// Length in bytes of a hex-encoded SHA256 fingerprint-of-pubkey prefix.
/// We expose 16 bytes (32 hex chars) of the raw 32-byte pubkey as the
/// manifest's `signing_pubkey_fingerprint` field - enough to diagnose
/// "wrong key" errors without leaking the whole key material into logs
/// or UIs on every mount.
pub const PUBKEY_FINGERPRINT_HEX_LEN: usize = 32; // 16 bytes -> 32 hex chars

/// Generate a fresh keypair. Used by key-setup tooling, not on a hot
/// path. Returns a tuple of (signing, verifying) keys in raw byte form
/// so callers can serialize them however they like (pkcs8 for the
/// private key, hex for the pubkey).
pub fn generate_keypair() -> (SigningKey, VerifyingKey) {
    use rand::rngs::OsRng;
    let signing = SigningKey::generate(&mut OsRng);
    let verifying = signing.verifying_key();
    (signing, verifying)
}

/// Sign `manifest_bytes` (the raw JSON payload) with `signing_key`.
pub fn sign_manifest(signing_key: &mut SigningKey, manifest_bytes: &[u8]) -> Vec<u8> {
    let sig: Signature = signing_key.sign(manifest_bytes);
    sig.to_bytes().to_vec()
}

/// Verify a detached signature over `manifest_bytes` against
/// `pubkey_bytes`.
///
/// Errors on:
///   - Pubkey wrong length (must be 32 bytes).
///   - Signature wrong length (must be 64 bytes).
///   - Any Ed25519 verification failure.
///
/// A returned `Ok(())` is a hard cryptographic assertion: the manifest
/// bytes were signed by the holder of the private key corresponding to
/// `pubkey_bytes`.
pub fn verify_manifest(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    pubkey_bytes: &[u8],
) -> Result<()> {
    if pubkey_bytes.len() != ed25519_dalek::PUBLIC_KEY_LENGTH {
        bail!(
            "pubkey must be {} bytes, got {}",
            ed25519_dalek::PUBLIC_KEY_LENGTH,
            pubkey_bytes.len()
        );
    }
    if signature_bytes.len() != SIGNATURE_LENGTH {
        bail!(
            "signature must be {} bytes, got {}",
            SIGNATURE_LENGTH,
            signature_bytes.len()
        );
    }
    let pubkey_arr: [u8; ed25519_dalek::PUBLIC_KEY_LENGTH] = pubkey_bytes
        .try_into()
        .map_err(|_| anyhow!("pubkey length check passed but conversion failed"))?;
    let verifying = VerifyingKey::from_bytes(&pubkey_arr)
        .context("loading Ed25519 verifying key")?;
    let sig_arr: [u8; SIGNATURE_LENGTH] = signature_bytes
        .try_into()
        .map_err(|_| anyhow!("signature length check passed but conversion failed"))?;
    let signature = Signature::from_bytes(&sig_arr);
    verifying
        .verify(manifest_bytes, &signature)
        .context("Ed25519 manifest signature verification failed")
}

/// First 16 bytes of the pubkey, lowercase-hex-encoded. Stored in the
/// manifest's `signing_pubkey_fingerprint` field so mount-time errors
/// can distinguish "signature invalid" from "signed by the wrong key".
pub fn fingerprint(pubkey_bytes: &[u8]) -> Result<String> {
    if pubkey_bytes.len() < 16 {
        bail!("pubkey too short to fingerprint");
    }
    Ok(hex::encode(&pubkey_bytes[..16]))
}

/// Parse the hex-encoded pubkey bytes shipped at
/// `src-tauri/signing/atlas-pubkey.hex`. The file contains a single
/// 64-char hex line (64 chars = 32 bytes = one Ed25519 pubkey).
/// Whitespace and comments (`# ...`) are stripped.
pub fn parse_pubkey_hex(text: &str) -> Result<[u8; ed25519_dalek::PUBLIC_KEY_LENGTH]> {
    let mut cleaned = String::new();
    for line in text.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        cleaned.push_str(trimmed);
    }
    let bytes =
        hex::decode(&cleaned).context("decoding atlas-pubkey.hex (expected hex-encoded bytes)")?;
    if bytes.len() != ed25519_dalek::PUBLIC_KEY_LENGTH {
        bail!(
            "atlas-pubkey.hex must decode to {} bytes, got {}",
            ed25519_dalek::PUBLIC_KEY_LENGTH,
            bytes.len()
        );
    }
    let mut out = [0u8; ed25519_dalek::PUBLIC_KEY_LENGTH];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// The embedded signing pubkey, parsed at first access.
///
/// Compile-time embed: `signing/atlas-pubkey.hex` is baked into the
/// binary via `include_str!`. This makes the pubkey tamper-evident in
/// shipped installers - an attacker who wants to mount a forged
/// artifact can't just swap a file on disk.
pub fn embedded_pubkey() -> Result<[u8; ed25519_dalek::PUBLIC_KEY_LENGTH]> {
    const RAW: &str = include_str!("../../signing/atlas-pubkey.hex");
    parse_pubkey_hex(RAW)
}

// `SECRET_KEY_LENGTH` is re-exported for callers that read a raw seed
// out of an env var during `atlas-build`. Kept pub(crate) to avoid
// leaking dalek internals out of the fetcher module.
#[allow(dead_code)]
pub(crate) const SECRET_KEY_LEN: usize = SECRET_KEY_LENGTH;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sign_and_verify() {
        let (mut signing, verifying) = generate_keypair();
        let manifest = b"{\"build_id\":\"release-abcd1234\"}";
        let sig = sign_manifest(&mut signing, manifest);
        verify_manifest(manifest, &sig, verifying.as_bytes()).unwrap();
    }

    #[test]
    fn verify_rejects_tampered_manifest() {
        let (mut signing, verifying) = generate_keypair();
        let manifest = b"{\"build_id\":\"release-abcd1234\"}";
        let sig = sign_manifest(&mut signing, manifest);
        let tampered = b"{\"build_id\":\"release-00000000\"}";
        let err = verify_manifest(tampered, &sig, verifying.as_bytes()).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("verification"));
    }

    #[test]
    fn verify_rejects_wrong_pubkey() {
        let (mut signing, _) = generate_keypair();
        let (_, other_verifying) = generate_keypair();
        let manifest = b"payload";
        let sig = sign_manifest(&mut signing, manifest);
        let err = verify_manifest(manifest, &sig, other_verifying.as_bytes()).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("verification"));
    }

    #[test]
    fn verify_rejects_bad_lengths() {
        let manifest = b"x";
        let bad_sig = vec![0u8; 10];
        let bad_pk = vec![0u8; 10];
        assert!(verify_manifest(manifest, &bad_sig, &[0u8; 32]).is_err());
        assert!(verify_manifest(manifest, &[0u8; SIGNATURE_LENGTH], &bad_pk).is_err());
    }

    #[test]
    fn fingerprint_is_first_16_bytes_hex() {
        let mut pubkey = [0u8; 32];
        for (i, b) in pubkey.iter_mut().enumerate() {
            *b = i as u8;
        }
        let fp = fingerprint(&pubkey).unwrap();
        assert_eq!(fp.len(), PUBKEY_FINGERPRINT_HEX_LEN);
        assert_eq!(fp, "000102030405060708090a0b0c0d0e0f");
    }

    #[test]
    fn parse_pubkey_hex_ignores_whitespace_and_comments() {
        let text = "# Atlas signing pubkey\n\
                    00112233445566778899aabbccddeeff\n\
                    00112233445566778899aabbccddeeff\n";
        let pk = parse_pubkey_hex(text).unwrap();
        assert_eq!(pk.len(), 32);
        assert_eq!(pk[0], 0x00);
        assert_eq!(pk[31], 0xff);
    }

    #[test]
    fn parse_pubkey_hex_rejects_wrong_length() {
        let short = "00112233";
        assert!(parse_pubkey_hex(short).is_err());
    }
}
