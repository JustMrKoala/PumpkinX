//! Keystone, built in. The plugin is a Wasm component embedded in the binary. At startup it is
//! written to `plugins/` for the regular Wasm loader, or removed when it is turned off.

use std::{fs, path::Path};

/// The Wasm API version of Keystone that is in this binary. Set by `build.rs`.
#[cfg(keystone_embedded)]
const WASM: &[u8] = include_bytes!(env!("KEYSTONE_WASM"));

const FILE_NAME: &str = "builtin-keystone.wasm";
/// Plugin name as Keystone reports it, used to keep it off the plugins page.
pub const PLUGIN_NAME: &str = "Keystone";

#[must_use]
pub const fn embedded() -> bool {
    cfg!(keystone_embedded)
}

#[cfg(keystone_embedded)]
fn install(path: &Path) {
    let current = fs::read(path).ok();
    if current.as_deref() == Some(WASM) {
        return;
    }
    let result = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, WASM));
    if let Err(err) = result {
        tracing::error!("Couldn't install built-in Keystone: {err}");
    }
}

#[cfg(not(keystone_embedded))]
fn install(_path: &Path) {
    tracing::warn!("Keystone is enabled, but this build doesn't contain it. Use build.sh or build.ps1.");
}

/// Must run before the plugins are loaded.
pub fn prepare(dir: &Path, enabled: bool) {
    let path = dir.join("plugins").join(FILE_NAME);
    if enabled {
        install(&path);
        // It ships with the server, so its permissions don't need a console confirmation.
        pumpkin_core::pumpkinx::set_trusted_plugin(FILE_NAME);
    } else if path.exists()
        && let Err(err) = fs::remove_file(&path)
    {
        tracing::warn!("Couldn't remove built-in Keystone: {err}");
    }
}
