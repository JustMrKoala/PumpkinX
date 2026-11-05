use crate::{java::setup_java, protobufs::setup_protobufs};
use std::{error::Error, path::PathBuf};

pub mod java;
pub mod protobufs;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build/main.rs");
    println!("cargo::rerun-if-changed=build/java.rs");
    println!("cargo::rerun-if-changed=build/protobufs.rs");
    println!("cargo::rerun-if-changed=../java/patchbukkit/build/libs/patchbukkit.jar");
    println!("cargo::rerun-if-changed=resources/jassets/");
    env_logger::init();

    let base = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    setup_protobufs(base.clone());
    setup_java(base);
    Ok(())
}
