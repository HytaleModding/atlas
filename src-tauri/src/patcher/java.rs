//! Detect a usable Java runtime.
//!
//! Hytale's server + Vineflower both need Java 17+. We locate `java` on PATH,
//! run `java -version`, parse the version string, and reject anything older.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

pub const MIN_JAVA_MAJOR: u32 = 17;

/// Find `java` on PATH and verify its major version is >= 17.
/// Returns the absolute path to the executable.
pub async fn ensure_java() -> Result<PathBuf> {
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    let java_path = which(exe).context("`java` not found on PATH")?;

    let output = Command::new(&java_path)
        .arg("-version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("running {} -version", java_path.display()))?;

    if !output.status.success() {
        return Err(anyhow!(
            "java -version exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // `java -version` prints to stderr. Example lines:
    //   openjdk version "17.0.9" 2023-10-17
    //   openjdk version "21.0.1" 2023-10-17
    //   java version "1.8.0_341"
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let major = parse_major_version(&text)
        .ok_or_else(|| anyhow!("could not parse Java version from output:\n{text}"))?;

    if major < MIN_JAVA_MAJOR {
        return Err(anyhow!(
            "Java {MIN_JAVA_MAJOR}+ is required, found Java {major}. \
             Install a JDK from https://adoptium.net/ and ensure `java` is on PATH."
        ));
    }

    tracing::info!("detected Java {major} at {}", java_path.display());
    Ok(java_path)
}

fn parse_major_version(text: &str) -> Option<u32> {
    // Look for the first `"..."` after the word `version`.
    let start = text.find('"')?;
    let rest = &text[start + 1..];
    let end = rest.find('"')?;
    let ver = &rest[..end];

    // Java 1.8.0_x -> major 8; 17.0.9 -> 17; 21 -> 21.
    let first_part = ver.split('.').next()?;
    let first: u32 = first_part.parse().ok()?;
    if first == 1 {
        ver.split('.').nth(1)?.parse().ok()
    } else {
        Some(first)
    }
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

#[cfg(test)]
mod tests {
    use super::parse_major_version;

    #[test]
    fn parses_modern_jdk() {
        let text = r#"openjdk version "17.0.9" 2023-10-17"#;
        assert_eq!(parse_major_version(text), Some(17));
    }

    #[test]
    fn parses_jdk_21() {
        let text = r#"openjdk version "21" 2023-09-19"#;
        assert_eq!(parse_major_version(text), Some(21));
    }

    #[test]
    fn parses_legacy_1_8() {
        let text = r#"java version "1.8.0_341""#;
        assert_eq!(parse_major_version(text), Some(8));
    }
}
