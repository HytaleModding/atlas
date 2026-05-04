//! Spawn Vineflower against the extracted classes.
//!
//! Flags mirror Horizon's `common.run_vineflower`:
//!   --decompile-generics=true
//!   --hide-default-constructor=false
//!   --remove-bridge=false
//!   --ascii-strings=true
//!   --use-lvt-names=true
//!   --log-level=warn
//!   -e=.
//!
//! Source is `classes_dir/com/hypixel` if present (keeps decompile tight and
//! skips META-INF etc.), otherwise the whole `classes_dir`. Output goes to
//! `out_dir`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

pub async fn run_vineflower(
    java: &Path,
    vineflower_jar: &Path,
    classes_dir: &Path,
    out_dir: &Path,
) -> Result<()> {
    let source = pick_source(classes_dir);

    tracing::info!(
        "vineflower: java={} jar={} source={} out={}",
        java.display(),
        vineflower_jar.display(),
        source.display(),
        out_dir.display()
    );

    let status = Command::new(java)
        .arg("-jar")
        .arg(vineflower_jar)
        .arg("--decompile-generics=true")
        .arg("--hide-default-constructor=false")
        .arg("--remove-bridge=false")
        .arg("--ascii-strings=true")
        .arg("--use-lvt-names=true")
        .arg("--log-level=warn")
        .arg("-e=.")
        .arg(&source)
        .arg(out_dir)
        .current_dir(classes_dir)
        .status()
        .await
        .with_context(|| {
            format!(
                "spawning {} -jar {}",
                java.display(),
                vineflower_jar.display()
            )
        })?;

    if !status.success() {
        return Err(anyhow!(
            "Vineflower exited with {}; see logs for details",
            status
        ));
    }

    Ok(())
}

fn pick_source(classes_dir: &Path) -> PathBuf {
    let hypixel = classes_dir.join("com").join("hypixel");
    if hypixel.is_dir() {
        hypixel
    } else {
        classes_dir.to_path_buf()
    }
}
