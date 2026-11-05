//! PatchBukkit toggle. The plugin is linked into the binary (cargo feature `patchbukkit`)
//! and only started when `pumpkinx.toml` turns it on, so the JVM never starts otherwise.

use std::{path::PathBuf, process::Command, sync::Arc};

use pumpkin_core::server::Server;
use serde::Serialize;

/// Where PatchBukkit reads Bukkit, Spigot and Paper plugin jars from.
pub const PLUGIN_DIR: &str = "plugins/data/patchbukkit/patchbukkit-plugins";

#[derive(Clone, Debug, Serialize)]
pub struct JavaInfo {
    pub found: bool,
    pub version: Option<String>,
}

/// Looks for a `java` binary (`JAVA_HOME` first, then `PATH`) and reads its version line.
#[must_use]
pub fn detect_java() -> JavaInfo {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        candidates.push(PathBuf::from(home).join("bin").join("java"));
    }
    candidates.push(PathBuf::from("java"));
    for java in candidates {
        if let Ok(output) = Command::new(&java).arg("-version").output() {
            let text = String::from_utf8_lossy(&output.stderr).into_owned();
            let version = text.lines().next().map(str::to_string);
            return JavaInfo {
                found: true,
                version,
            };
        }
    }
    JavaInfo {
        found: false,
        version: None,
    }
}

#[must_use]
pub const fn compiled_in() -> bool {
    cfg!(feature = "patchbukkit")
}

/// Plugin jars waiting in [`PLUGIN_DIR`].
#[must_use]
pub fn plugin_jars() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(PLUGIN_DIR) else {
        return Vec::new();
    };
    let mut jars: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_ascii_lowercase().ends_with(".jar"))
        .collect();
    jars.sort();
    jars
}

#[cfg(feature = "patchbukkit")]
static KEEP_ALIVE: std::sync::OnceLock<Arc<dyn pumpkin_core::plugin::Plugin>> =
    std::sync::OnceLock::new();

#[cfg(feature = "patchbukkit")]
pub async fn start(server: &Arc<Server>) {
    let java = detect_java();
    if !java.found {
        tracing::error!(
            "PatchBukkit is enabled but no Java was found. Install Java 25 or newer, or set JAVA_HOME."
        );
        return;
    }
    let _ = std::fs::create_dir_all(PLUGIN_DIR);
    let instance: Arc<dyn pumpkin_core::plugin::Plugin> = Arc::from(patchbukkit::plugin());
    // The plugin owns a tokio runtime, and dropping one on an async thread panics. That panic
    // would abort shutdown before the world save, so the instance lives until the process exits.
    let _ = KEEP_ALIVE.set(instance.clone());
    let metadata = patchbukkit::METADATA.clone();
    tracing::info!("Starting built-in PatchBukkit {}", metadata.version);
    if let Err(err) = server
        .plugin_manager
        .load_builtin_plugin(server, instance, metadata)
        .await
    {
        tracing::error!("PatchBukkit failed to start: {err}");
    }
}

#[cfg(not(feature = "patchbukkit"))]
pub async fn start(_server: &Arc<Server>) {
    tracing::error!(
        "PatchBukkit is enabled in pumpkinx.toml, but this build was made without the `patchbukkit` feature."
    );
}
