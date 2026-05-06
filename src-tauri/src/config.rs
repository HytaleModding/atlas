//! Atlas config persistence: a single JSON file in the OS app-data dir.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fs, io};

const CONFIG_FILENAME: &str = "config.json";

/// Which Hytale install the user is currently working against. Persists in
/// config as the default; can be overridden live via the LeftNav toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    Release,
    PreRelease,
}

impl Default for Slot {
    fn default() -> Self {
        Slot::Release
    }
}

impl Slot {
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::Release => "release",
            Slot::PreRelease => "pre-release",
        }
    }
}

/// User-visible Atlas configuration. Any field that should persist across app
/// restarts lives here. Missing fields deserialize to their defaults so old
/// configs stay forward-compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AtlasConfig {
    /// Path to the Hytale release install (directory containing `Server/HytaleServer.jar`).
    pub hytale_release_path: Option<PathBuf>,
    /// Path to the Hytale pre-release install, if the user has one.
    pub hytale_prerelease_path: Option<PathBuf>,
    /// True once the user has actively chosen to skip the first-run wizard.
    pub first_run_skipped: bool,
    /// Which branch the LeftNav defaults to on app startup.
    pub active_branch: Slot,
    /// `owner/name` of the GitHub repository that hosts published reference
    /// data releases. The desktop client queries this repo's Releases API to
    /// discover and download new builds. Defaults to the dev test repo until
    /// the public hand-off.
    pub central_repo: String,
    /// build_id of the build the user has selected as the active reference
    /// data for the release patchline. Search defaults to this build when
    /// the LeftNav is on Release. `None` until the user mounts a release
    /// build for the first time.
    pub active_release_build: Option<String>,
    /// build_id of the active build for the pre-release patchline.
    pub active_pre_release_build: Option<String>,
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            hytale_release_path: None,
            hytale_prerelease_path: None,
            first_run_skipped: false,
            active_branch: Slot::default(),
            central_repo: default_central_repo(),
            active_release_build: None,
            active_pre_release_build: None,
        }
    }
}

/// Default central repository for reference-data releases. Points at the
/// HytaleModding org repo where index artifacts are published.
pub fn default_central_repo() -> String {
    "HytaleModding/atlas".to_string()
}

/// Result of validating a candidate Hytale install directory.
#[derive(Debug, Clone, Serialize)]
pub struct HytalePathCheck {
    pub path: PathBuf,
    pub valid: bool,
    pub reason: Option<String>,
}

fn config_dir() -> io::Result<PathBuf> {
    directories::ProjectDirs::from("dev", "horizon", "Atlas")
        .map(|p| p.config_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no OS config dir for Atlas"))
}

fn config_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILENAME))
}

pub fn load() -> io::Result<AtlasConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AtlasConfig::default());
    }
    let bytes = fs::read(&path)?;
    let cfg: AtlasConfig = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(cfg)
}

pub fn save(cfg: &AtlasConfig) -> io::Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(CONFIG_FILENAME);
    let json = serde_json::to_vec_pretty(cfg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(path, json)
}

/// Attempt to locate the Hytale release install in its default Windows
/// location. Returns the path only if `Server/HytaleServer.jar` exists.
pub fn detect_release_path() -> Option<PathBuf> {
    let candidate = default_release_candidate();
    let result = candidate.clone().filter(|p| is_valid_hytale_install(p));
    tracing::info!(
        target: "atlas::path_check",
        candidate = ?candidate.as_ref().map(|p| p.display().to_string()),
        appdata = ?std::env::var_os("APPDATA").map(|s| s.to_string_lossy().to_string()),
        valid = result.is_some(),
        "detect_release_path"
    );
    result
}

/// Attempt to locate the Hytale pre-release install.
pub fn detect_prerelease_path() -> Option<PathBuf> {
    default_prerelease_candidate().filter(|p| is_valid_hytale_install(p))
}

pub fn default_release_candidate() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Hytale")
            .join("install")
            .join("release")
            .join("package")
            .join("game")
            .join("latest"),
    )
}

pub fn default_prerelease_candidate() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Hytale")
            .join("install")
            .join("pre-release")
            .join("package")
            .join("game")
            .join("latest"),
    )
}

/// A Hytale install is valid if the expected server jar lives beneath it.
pub fn is_valid_hytale_install(path: &Path) -> bool {
    path.join("Server").join("HytaleServer.jar").is_file()
}

/// Absolute path the user configured (or auto-detected) for a given slot, if any.
pub fn configured_path(cfg: &AtlasConfig, slot: Slot) -> Option<PathBuf> {
    match slot {
        Slot::Release => cfg.hytale_release_path.clone(),
        Slot::PreRelease => cfg.hytale_prerelease_path.clone(),
    }
}

/// Default launcher path for a slot on the current OS.
pub fn default_candidate(slot: Slot) -> Option<PathBuf> {
    match slot {
        Slot::Release => default_release_candidate(),
        Slot::PreRelease => default_prerelease_candidate(),
    }
}

/// Run a full validation and return a structured result. Used by the front-end
/// for the first-run wizard's live validation feedback.
pub fn check_hytale_path(path: &Path) -> HytalePathCheck {
    let path_str = path.to_string_lossy();
    let path_bytes = path_str.as_bytes();
    let path_meta = std::fs::metadata(path);
    let canonical = std::fs::canonicalize(path);
    tracing::info!(
        target: "atlas::path_check",
        path = %path_str,
        path_len = path_bytes.len(),
        path_bytes_hex = %hex::encode(path_bytes),
        is_dir = path.is_dir(),
        exists = path.exists(),
        metadata_ok = path_meta.is_ok(),
        metadata_err_kind = ?path_meta.as_ref().err().map(|e| e.kind()),
        metadata_raw_os_err = ?path_meta.as_ref().err().and_then(|e| e.raw_os_error()),
        canonical_ok = canonical.is_ok(),
        canonical_err_kind = ?canonical.as_ref().err().map(|e| e.kind()),
        canonical_raw_os_err = ?canonical.as_ref().err().and_then(|e| e.raw_os_error()),
        "check_hytale_path invoked"
    );

    // Walk parents to find the first one that fails to stat.
    // Tells us if the failure is at a specific path level (e.g. AppData itself
    // is fine but Hytale isn't) vs a leaf-only failure.
    let mut cur = Some(path);
    while let Some(p) = cur {
        let m = std::fs::metadata(p);
        tracing::info!(
            target: "atlas::path_check",
            level = %p.display(),
            ok = m.is_ok(),
            raw_os_err = ?m.as_ref().err().and_then(|e| e.raw_os_error()),
            "ancestor stat"
        );
        cur = p.parent();
    }
    if !path.is_dir() {
        return HytalePathCheck {
            path: path.to_path_buf(),
            valid: false,
            reason: Some("Path does not exist or is not a directory.".to_string()),
        };
    }
    let jar = path.join("Server").join("HytaleServer.jar");
    let jar_meta = std::fs::metadata(&jar);
    tracing::info!(
        target: "atlas::path_check",
        jar = %jar.display(),
        is_file = jar.is_file(),
        metadata = ?jar_meta.as_ref().map(|m| m.is_file()).map_err(|e| e.kind()),
        "jar check"
    );
    if !jar.is_file() {
        return HytalePathCheck {
            path: path.to_path_buf(),
            valid: false,
            reason: Some(format!(
                "Missing {}. Pick the `latest` folder of a Hytale install.",
                jar.display()
            )),
        };
    }
    HytalePathCheck {
        path: path.to_path_buf(),
        valid: true,
        reason: None,
    }
}
