use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::Result;
use glob::glob;
use zip::ZipArchive;

use crate::config::{paper::PAPER_PLUGIN_CONFIG, spigot::SPIGOT_PLUGIN_CONFIG};

pub fn discover_jar_files(plugin_folder: &Path) -> impl Iterator<Item = PathBuf> {
    let pattern = format!("{}/**/*.jar", plugin_folder.display());

    glob(&pattern)
        .expect("Invalid glob pattern")
        .filter_map(|entry| {
            let path = entry
                .map_err(|e| tracing::error!("Glob error: {e:?}"))
                .ok()?
                .canonicalize()
                .map_err(|e| tracing::error!("Canonicalize error: {e:?}"))
                .ok()?;
            if path
                .components()
                .any(|c| c.as_os_str() == "patchbukkit-libs")
            {
                None
            } else {
                Some(path)
            }
        })
}

pub fn read_configs_from_jar<P: AsRef<Path>>(
    jar_path: P,
) -> Result<(Option<String>, Option<String>)> {
    let file = File::open(jar_path.as_ref())?;
    let mut archive = ZipArchive::new(file)?;

    let paper_plugin_yml = match archive.by_name(PAPER_PLUGIN_CONFIG).ok() {
        Some(mut file) => {
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Some(content)
        }
        None => None,
    };

    let spigot_plugin_yml = match archive.by_name(SPIGOT_PLUGIN_CONFIG).ok() {
        Some(mut file) => {
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Some(content)
        }
        None => None,
    };

    Ok((paper_plugin_yml, spigot_plugin_yml))
}

pub fn read_libraries_from_jar<P: AsRef<Path>>(jar_path: P) -> Vec<String> {
    let mut libraries = Vec::new();
    let file = match File::open(jar_path.as_ref()) {
        Ok(f) => f,
        Err(_) => return libraries,
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return libraries,
    };

    // 1. paper-libraries.json
    if let Ok(mut file) = archive.by_name("paper-libraries.json") {
        let mut content = String::new();
        if file.read_to_string(&mut content).is_ok()
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
        {
            if let Some(repos) = val.get("repositories") {
                if let Some(map) = repos.as_object() {
                    for (_, v) in map {
                        if let Some(url) = v.as_str() {
                            let repo_entry = format!("repo:{url}");
                            if !libraries.contains(&repo_entry) {
                                libraries.push(repo_entry);
                            }
                        }
                    }
                } else if let Some(arr) = repos.as_array() {
                    for item in arr {
                        if let Some(url) = item.as_str() {
                            let repo_entry = format!("repo:{url}");
                            if !libraries.contains(&repo_entry) {
                                libraries.push(repo_entry);
                            }
                        } else if let Some(obj) = item.as_object()
                            && let Some(url) = obj.get("url").and_then(|u| u.as_str())
                        {
                            let repo_entry = format!("repo:{url}");
                            if !libraries.contains(&repo_entry) {
                                libraries.push(repo_entry);
                            }
                        }
                    }
                }
            }

            if let Some(deps) = val.get("dependencies") {
                if let Some(arr) = deps.as_array() {
                    for item in arr {
                        if let Some(s) = item.as_str()
                            && !libraries.contains(&s.to_string())
                        {
                            libraries.push(s.to_string());
                        }
                    }
                } else if let Some(map) = deps.as_object() {
                    for (k, v) in map {
                        if let Some(ver) = v.as_str() {
                            let coord = format!("{k}:{ver}");
                            if !libraries.contains(&coord) {
                                libraries.push(coord);
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. paper-libraries.list
    if let Ok(mut file) = archive.by_name("paper-libraries.list") {
        let mut content = String::new();
        if file.read_to_string(&mut content).is_ok() {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with('#')
                    && !libraries.contains(&trimmed.to_string())
                {
                    libraries.push(trimmed.to_string());
                }
            }
        }
    }

    libraries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn test_read_libraries_from_jar_json() {
        let temp_dir = std::env::temp_dir();
        let jar_path = temp_dir.join("test_libraries.jar");

        {
            let file = File::create(&jar_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);

            zip.start_file("paper-libraries.json", SimpleFileOptions::default())
                .unwrap();
            let json = r#"{
                "repositories": {
                    "miraculixx": "https://repo.miraculixx.de"
                },
                "dependencies": [
                    "de.miraculixx:kpaper:1.1.2"
                ]
            }"#;
            zip.write_all(json.as_bytes()).unwrap();
            zip.finish().unwrap();
        }

        let libs = read_libraries_from_jar(&jar_path);
        assert!(libs.contains(&"repo:https://repo.miraculixx.de".to_string()));
        assert!(libs.contains(&"de.miraculixx:kpaper:1.1.2".to_string()));

        let _ = std::fs::remove_file(jar_path);
    }
}
