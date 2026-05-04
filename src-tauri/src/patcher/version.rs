//! Extract the Hytale version string from a server JAR.
//!
//! `META-INF/MANIFEST.MF` inside `HytaleServer.jar` carries e.g.
//! ```text
//! Implementation-Version: 2026.03.26-89796e57b
//! Implementation-Patchline: release
//! Implementation-Revision-Id: 89796e57b95fa62c6306b423dde8dfbffaa03ce7
//! ```
//! The launcher's "Version:" label shows the same `Implementation-Version`
//! string. This is the value modders need to put in their manifest so
//! Hytale's "your mod is out of date" warning stops firing - so we surface
//! it verbatim.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use zip::ZipArchive;

#[derive(Debug, Clone, Serialize)]
pub struct HytaleVersion {
    /// The `Implementation-Version` value, e.g. `2026.03.26-89796e57b`.
    pub implementation_version: String,
    /// The `Implementation-Patchline` value, e.g. `release` or `pre-release`.
    pub patchline: Option<String>,
    /// The full `Implementation-Revision-Id` (40-char git SHA).
    pub revision_id: Option<String>,
}

/// Read the version manifest from `install_path/Server/HytaleServer.jar`.
#[allow(dead_code)]
pub fn read_from_install(install_path: &Path) -> Result<HytaleVersion> {
    let jar = install_path.join("Server").join("HytaleServer.jar");
    read_from_jar(&jar)
}

pub fn read_from_jar(jar_path: &Path) -> Result<HytaleVersion> {
    let file =
        File::open(jar_path).with_context(|| format!("opening {}", jar_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("parsing {} as zip", jar_path.display()))?;

    let mut entry = archive
        .by_name("META-INF/MANIFEST.MF")
        .context("MANIFEST.MF missing from JAR")?;
    let mut text = String::new();
    entry.read_to_string(&mut text)?;

    parse_manifest(&text).context("parsing MANIFEST.MF")
}

fn parse_manifest(text: &str) -> Result<HytaleVersion> {
    let mut map = std::collections::HashMap::<String, String>::new();
    let mut current_key: Option<String> = None;

    // MIME-style: lines beginning with space are continuations of the prior value.
    for raw in text.split_terminator(['\r', '\n']) {
        if raw.is_empty() {
            continue;
        }
        if let Some(rest) = raw.strip_prefix(' ') {
            if let Some(k) = &current_key {
                if let Some(v) = map.get_mut(k) {
                    v.push_str(rest);
                }
            }
            continue;
        }
        if let Some((k, v)) = raw.split_once(": ") {
            map.insert(k.to_string(), v.to_string());
            current_key = Some(k.to_string());
        }
    }

    let implementation_version = map
        .remove("Implementation-Version")
        .context("Implementation-Version not present in MANIFEST.MF")?;

    Ok(HytaleVersion {
        implementation_version,
        patchline: map.remove("Implementation-Patchline"),
        revision_id: map.remove("Implementation-Revision-Id"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hytale_manifest() {
        let text = "Manifest-Version: 1.0\r\n\
                    Created-By: Maven JAR Plugin 3.5.0\r\n\
                    Implementation-Branch: release\r\n\
                    Implementation-Patchline: release\r\n\
                    Implementation-Revision-Id: 89796e57b95fa62c6306b423dde8dfbffaa03ce7\r\n\
                    Implementation-Version: 2026.03.26-89796e57b\r\n";
        let v = parse_manifest(text).unwrap();
        assert_eq!(v.implementation_version, "2026.03.26-89796e57b");
        assert_eq!(v.patchline.as_deref(), Some("release"));
        assert_eq!(
            v.revision_id.as_deref(),
            Some("89796e57b95fa62c6306b423dde8dfbffaa03ce7")
        );
    }

    #[test]
    fn handles_continuation_lines() {
        // A value split across lines per MIME continuation rules.
        let text = "Manifest-Version: 1.0\n\
                    Implementation-Version: 2026.03.26\n\
                    \x20-89796e57b\n";
        let v = parse_manifest(text).unwrap();
        assert_eq!(v.implementation_version, "2026.03.26-89796e57b");
    }
}
