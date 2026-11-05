use std::{fs, path::PathBuf};

pub fn setup_java(base: PathBuf) {
    let resources = base.join("resources");
    let jassets = resources.join("jassets");

    let mut java_path = base;
    java_path.pop();
    let java_path = java_path.join("java");

    let patchbukkit_jar = java_path
        .join("patchbukkit")
        .join("build")
        .join("libs")
        .join("patchbukkit.jar");

    if !patchbukkit_jar.exists() {
        panic!(
            "Failed to find patchbukkit.jar, build the java library first by running `./gradlew build` in the java directory!"
        );
    }

    fs::create_dir_all(&jassets).unwrap();
    let dest_patchbukkit = jassets.join("patchbukkit.jar");
    fs::copy(&patchbukkit_jar, &dest_patchbukkit)
        .map_err(|err| format!("Failed to copy patchbukkit.jar to resources/jassets: {err:?}"))
        .unwrap();
}
