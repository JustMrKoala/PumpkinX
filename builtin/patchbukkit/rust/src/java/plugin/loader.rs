use std::{any::Any, path::Path, sync::Arc};

use pumpkin::plugin::{
    Context, PluginMetadata,
    api::{Plugin, PluginFuture},
    loader::{LoaderError, PluginLoadFuture, PluginLoader, PluginUnloadFuture},
};

use crate::java::jar::read_configs_from_jar;

pub struct BukkitPluginStub;

impl Plugin for BukkitPluginStub {
    fn on_load(&self, _server: Arc<Context>) -> PluginFuture<'_, Result<(), String>> {
        Box::pin(async move { Ok(()) })
    }

    fn on_unload(&self, _server: Arc<Context>) -> PluginFuture<'_, Result<(), String>> {
        Box::pin(async move { Ok(()) })
    }
}

pub struct BukkitPluginLoader;

impl PluginLoader for BukkitPluginLoader {
    fn can_load(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("jar")
    }

    fn load<'a>(&'a self, path: &'a Path) -> PluginLoadFuture<'a> {
        Box::pin(async move {
            let (paper_yml_opt, spigot_yml_opt) = read_configs_from_jar(path)
                .map_err(|e| LoaderError::InitializationFailed(e.to_string()))?;

            let metadata = if let Some(paper_str) = paper_yml_opt {
                let paper_yml = crate::config::paper::PaperPluginYml::parse(&paper_str)
                    .map_err(|e| LoaderError::InitializationFailed(e.to_string()))?;
                PluginMetadata {
                    name: format!("[Paper] {}", paper_yml.name),
                    version: paper_yml.version,
                    authors: paper_yml.author.map_or_else(Vec::new, |a| vec![a]),
                    description: paper_yml.description.unwrap_or_default(),
                    dependencies: Vec::new(),
                    permissions: Vec::new(),
                }
            } else if let Some(spigot_str) = spigot_yml_opt {
                let spigot_yml = crate::config::spigot::SpigotPluginYml::parse(&spigot_str)
                    .map_err(|e| LoaderError::InitializationFailed(e.to_string()))?;
                let authors = spigot_yml.get_all_authors();
                PluginMetadata {
                    name: format!("[Bukkit] {}", spigot_yml.name),
                    version: spigot_yml.version,
                    authors,
                    description: spigot_yml.description.unwrap_or_default(),
                    dependencies: spigot_yml.depend.unwrap_or_default(),
                    permissions: Vec::new(),
                }
            } else {
                return Err(LoaderError::MetadataMissing);
            };

            let plugin: Arc<dyn Plugin> = Arc::new(BukkitPluginStub);
            let loader_data: Box<dyn Any + Send + Sync> = Box::new(());

            Ok((plugin, metadata, loader_data))
        })
    }

    fn unload(&self, _data: Box<dyn Any + Send + Sync>) -> PluginUnloadFuture<'_> {
        Box::pin(async move { Ok(()) })
    }

    fn can_unload(&self) -> bool {
        true
    }
}
