//! Embeds the Keystone Wasm plugin when it has been built (build.sh and build.ps1 do that first).

use std::{env, path::PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(keystone_embedded)");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let wasm = manifest
        .join("../../builtin/keystone/target/wasm32-wasip2/release/keystone_pumpkin.wasm");
    println!("cargo::rerun-if-changed={}", wasm.display());
    if wasm.exists() {
        println!("cargo::rustc-cfg=keystone_embedded");
        println!("cargo::rustc-env=KEYSTONE_WASM={}", wasm.display());
    } else {
        println!("cargo::warning=Keystone is not built, so this binary won't contain it");
    }
}
