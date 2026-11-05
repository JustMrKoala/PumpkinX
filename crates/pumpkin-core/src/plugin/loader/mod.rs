use crate::{
    plugin::{PluginMetadata, api::Plugin},
    server::Server,
};
use std::{any::Any, path::Path, pin::Pin, sync::Arc};
use thiserror::Error;

pub mod native;

/// Loader for plugins compiled into the server binary (PumpkinX built-ins).
/// They never come from a file and stay loaded until shutdown.
pub struct BuiltinLoader;

impl PluginLoader for BuiltinLoader {
    fn load<'a>(&'a self, _path: &'a Path) -> PluginLoadFuture<'a> {
        Box::pin(async {
            Err(LoaderError::RuntimeError(
                "built-in plugins are not loaded from files".to_string(),
            ))
        })
    }

    fn can_load(&self, _path: &Path) -> bool {
        false
    }

    fn unload(&self, _data: Box<dyn Any + Send + Sync>) -> PluginUnloadFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn can_unload(&self) -> bool {
        false
    }
}

pub type PluginLoadFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<
                    (Arc<dyn Plugin>, PluginMetadata, Box<dyn Any + Send + Sync>),
                    LoaderError,
                >,
            > + Send
            + 'a,
    >,
>;

pub type PluginUnloadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + 'a>>;

pub trait PluginLoader: Send + Sync {
    /// Load a plugin from the specified path
    fn load<'a>(&'a self, path: &'a Path) -> PluginLoadFuture<'a>;

    /// Check if this loader can handle the given file
    fn can_load(&self, path: &Path) -> bool;

    fn unload(&self, data: Box<dyn Any + Send + Sync>) -> PluginUnloadFuture<'_>;

    /// Checks if the plugin can be safely unloaded.
    fn can_unload(&self) -> bool;

    /// Whether the plugin file at `path` carries a valid signature. Loaders without signing accept every file.
    fn is_signed(&self, _path: &Path) -> bool {
        true
    }

    /// Called once per server tick for loaders that run work of their own, like scheduled plugin tasks.
    fn on_tick(&self, _server: &Arc<Server>) {}
}

/// Unified loader error type
#[derive(Error, Debug)]
pub enum LoaderError {
    #[error("Failed to load library: {0}")]
    LibraryLoad(String),

    #[error("Missing plugin metadata")]
    MetadataMissing,

    #[error("Missing plugin entrypoint")]
    EntrypointMissing,

    #[error("Plugin initialization failed: {0}")]
    InitializationFailed(String),

    #[error("Runtime error: {0}")]
    RuntimeError(String),

    #[error("Invalid loader data")]
    InvalidLoaderData,

    #[error(
        "Plugin was built for an incompatible API version. Please rebuild it against this Pumpkin build."
    )]
    ApiVersionMissing,

    #[error(
        "Plugin API version mismatch (plugin {plugin_version}, server {server_version}). Please rebuild it against this Pumpkin build."
    )]
    ApiVersionMismatch {
        plugin_version: u32,
        server_version: u32,
    },

    #[error("Wasm plugin initialization error: {0}")]
    WasmInitializationError(Box<dyn std::error::Error + Send + Sync>),
}
