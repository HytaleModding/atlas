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

/// Default central repository for reference-data releases. Test repo while the
/// pipeline beds in; flip to `HytaleModding/atlas` (or wherever HM hosts) for
/// the public release.
pub fn default_central_repo() -> String {
    "Vibe-Theory/atlastest".to_string()
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
    default_release_candidate().filter(|p| is_valid_hytale_install(p))
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
    if !path.is_dir() {
        return HytalePathCheck {
            path: path.to_path_buf(),
            valid: false,
            reason: Some("Path does not exist or is not a directory.".to_string()),
        };
    }
    let jar = path.join("Server").join("HytaleServer.jar");
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
