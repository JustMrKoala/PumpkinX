//! PumpkinX: the layer on top of Pumpkin that adds built-in multiversion support, an
//! optional PatchBukkit runtime and a web dashboard.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Instant,
};

use pumpkin_core::server::Server;

pub mod config;
pub mod dashboard;
pub mod keystone;
pub mod multiversion;
pub mod patchbukkit;

use config::PumpkinXConfig;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct PumpkinX {
    /// Config as it was when the server started. Toggles only apply after a restart.
    pub running: PumpkinXConfig,
    /// Config as it is on disk, edited through the dashboard.
    pub saved: RwLock<PumpkinXConfig>,
    pub dir: PathBuf,
    pub started: Instant,
}

impl PumpkinX {
    /// Reads `pumpkinx.toml`. Call before the server is built so the log shows it early.
    #[must_use]
    pub fn load(dir: &Path) -> Arc<Self> {
        let config = PumpkinXConfig::load(dir);
        Arc::new(Self {
            running: config.clone(),
            saved: RwLock::new(config),
            dir: dir.to_path_buf(),
            started: Instant::now(),
        })
    }

    /// Hooks that must exist before players connect.
    pub fn on_server_created(&self, server: &Arc<Server>) {
        let explosions = &self.running.explosions;
        pumpkin_core::pumpkinx::set_chain_explosions(
            if explosions.merge_chains {
                explosions.min_tnt.max(2)
            } else {
                0
            },
            explosions.tick_budget_ms,
        );
        if self.running.multiversion.enabled {
            multiversion::enable(server);
        }
        keystone::prepare(
            &self.dir,
            self.running.keystone.enabled && server.advanced_config.plugins.enabled,
        );
    }

    /// Starts the built-in plugins and the dashboard once the regular plugins are loaded.
    pub async fn on_plugins_loaded(self: &Arc<Self>, server: &Arc<Server>) {
        if self.running.patchbukkit.enabled {
            if server.advanced_config.plugins.enabled {
                patchbukkit::start(server).await;
            } else {
                tracing::warn!("PatchBukkit is enabled, but the plugin system is off in pumpkin.toml");
            }
        }
        if self.running.dashboard.enabled {
            dashboard::spawn(self.clone(), server.clone()).await;
        }
    }
}
