//! Detect installed IDEs so the patcher card can offer "Open in VS Code",
//! "Open in IntelliJ IDEA", etc.
//!
//! Windows-first detection heuristics - no registry yet:
//!   * `code.exe` / `code` on PATH
//!   * Well-known install dirs under `%LOCALAPPDATA%\Programs\` and
//!     `%PROGRAMFILES%\`.
//!   * JetBrains Toolbox installs under
//!     `%LOCALAPPDATA%\JetBrains\Toolbox\apps\IDEA-U\` and `IDEA-C\`.
//!
//! `detect_ides` is cheap enough to call once at startup.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdeId {
    Explorer,
    VsCode,
    VsCodeInsiders,
    IntelliJCommunity,
    IntelliJUltimate,
}

impl IdeId {
    pub fn display_name(self) -> &'static str {
        match self {
            IdeId::Explorer => "File Explorer",
            IdeId::VsCode => "VS Code",
            IdeId::VsCodeInsiders => "VS Code Insiders",
            IdeId::IntelliJCommunity => "IntelliJ IDEA CE",
            IdeId::IntelliJUltimate => "IntelliJ IDEA",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "explorer" => Some(IdeId::Explorer),
            "vs-code" => Some(IdeId::VsCode),
            "vs-code-insiders" => Some(IdeId::VsCodeInsiders),
            "intellij-community" => Some(IdeId::IntelliJCommunity),
            "intellij-ultimate" => Some(IdeId::IntelliJUltimate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectedIde {
    pub id: IdeId,
    pub display_name: String,
    pub executable: PathBuf,
}

/// Scan for IDEs. Explorer is always included on Windows.
pub fn detect_ides() -> Vec<DetectedIde> {
    let mut found = Vec::new();

    #[cfg(windows)]
    {
        // Explorer is always present on Windows.
        found.push(DetectedIde {
            id: IdeId::Explorer,
            display_name: IdeId::Explorer.display_name().to_string(),
            executable: PathBuf::from("explorer.exe"),
        });

        if let Some(code) = detect_vs_code(false) {
            found.push(code);
        }
        if let Some(code) = detect_vs_code(true) {
            found.push(code);
        }
        for idea in detect_intellij() {
            found.push(idea);
        }
    }

    #[cfg(not(windows))]
    {
        // Minimal non-Windows support: rely on `code` / `idea` on PATH.
        if let Some(p) = which("code") {
            found.push(DetectedIde {
                id: IdeId::VsCode,
                display_name: IdeId::VsCode.display_name().to_string(),
                executable: p,
            });
        }
        if let Some(p) = which("idea") {
            found.push(DetectedIde {
                id: IdeId::IntelliJUltimate,
                display_name: IdeId::IntelliJUltimate.display_name().to_string(),
                executable: p,
            });
        }
    }

    found
}

#[cfg(windows)]
fn detect_vs_code(insiders: bool) -> Option<DetectedIde> {
    let (cli, exe, dir, id) = if insiders {
        (
            "code-insiders",
            "Code - Insiders.exe",
            "Microsoft VS Code Insiders",
            IdeId::VsCodeInsiders,
        )
    } else {
        (
            "code",
            "Code.exe",
            "Microsoft VS Code",
            IdeId::VsCode,
        )
    };

    // 1) CLI on PATH (note: on Windows the `code` shim is a .cmd).
    let cli_names = [format!("{cli}.cmd"), format!("{cli}.exe")];
    for name in &cli_names {
        if let Some(p) = which(name) {
            return Some(DetectedIde {
                id,
                display_name: id.display_name().to_string(),
                executable: p,
            });
        }
    }

    // 2) Per-user install under %LOCALAPPDATA%\Programs\
    if let Some(la) = std::env::var_os("LOCALAPPDATA") {
        let p = Path::new(&la).join("Programs").join(dir).join(exe);
        if p.is_file() {
            return Some(DetectedIde {
                id,
                display_name: id.display_name().to_string(),
                executable: p,
            });
        }
    }

    // 3) System install under %PROGRAMFILES%\
    if let Some(pf) = std::env::var_os("PROGRAMFILES") {
        let p = Path::new(&pf).join(dir).join(exe);
        if p.is_file() {
            return Some(DetectedIde {
                id,
                display_name: id.display_name().to_string(),
                executable: p,
            });
        }
    }

    None
}

#[cfg(windows)]
fn detect_intellij() -> Vec<DetectedIde> {
    let mut out = Vec::new();

    let toolbox_roots = std::env::var_os("LOCALAPPDATA")
        .map(|la| {
            vec![
                (
                    Path::new(&la)
                        .join("JetBrains")
                        .join("Toolbox")
                        .join("apps")
                        .join("IDEA-U"),
                    IdeId::IntelliJUltimate,
                ),
                (
                    Path::new(&la)
                        .join("JetBrains")
                        .join("Toolbox")
                        .join("apps")
                        .join("IDEA-C"),
                    IdeId::IntelliJCommunity,
                ),
            ]
        })
        .unwrap_or_default();

    for (root, id) in toolbox_roots {
        if let Some(exe) = newest_intellij_exe(&root) {
            out.push(DetectedIde {
                id,
                display_name: id.display_name().to_string(),
                executable: exe,
            });
        }
    }

    // Program Files install (non-Toolbox).
    if let Some(pf) = std::env::var_os("PROGRAMFILES") {
        for (pattern, id) in [
            ("IntelliJ IDEA Community*", IdeId::IntelliJCommunity),
            ("IntelliJ IDEA*", IdeId::IntelliJUltimate),
        ] {
            if let Some(dir) = find_first_matching(Path::new(&pf), pattern) {
                let exe = dir.join("bin").join("idea64.exe");
                if exe.is_file() && !out.iter().any(|i| i.id == id) {
                    out.push(DetectedIde {
                        id,
                        display_name: id.display_name().to_string(),
                        executable: exe,
                    });
                }
            }
        }
    }

    out
}

/// For a Toolbox IDE root like `.../apps/IDEA-U`, find the most recent
/// `<version>/bin/idea64.exe`. Toolbox layout changed a few times so we
/// look both at the root-level and inside a `ch-0/` subdirectory.
#[cfg(windows)]
fn newest_intellij_exe(root: &Path) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }

    // Collect candidate dirs: root + `root/ch-0/` (legacy Toolbox layout).
    let mut search = vec![root.to_path_buf()];
    let ch0 = root.join("ch-0");
    if ch0.is_dir() {
        search.push(ch0);
    }

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in search {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let exe = p.join("bin").join("idea64.exe");
            if !exe.is_file() {
                continue;
            }
            let mtime = std::fs::metadata(&exe)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, exe));
            }
        }
    }

    best.map(|(_, p)| p)
}

#[cfg(windows)]
fn find_first_matching(base: &Path, pattern: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(base).ok()?;
    let lower_pattern = pattern.to_lowercase();
    let prefix = lower_pattern.trim_end_matches('*').to_string();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_lowercase();
        if lower.starts_with(&prefix) {
            return Some(entry.path());
        }
    }
    None
}

fn which(exe: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Launch a detached instance of the given IDE, pointed at `target`.
/// On Windows we use `cmd /C start ""` for `.cmd` shims so a console
/// window doesn't linger.
pub fn open_with(ide: &DetectedIde, target: &Path) -> std::io::Result<()> {
    let exe = ide.executable.as_path();

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;

        if exe
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("cmd"))
            .unwrap_or(false)
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(exe)
                .arg(target)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()?;
            return Ok(());
        }

        std::process::Command::new(exe)
            .arg(target)
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn()?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        std::process::Command::new(exe).arg(target).spawn()?;
        Ok(())
    }
}
