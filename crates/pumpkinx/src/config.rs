//! `pumpkinx.toml`: settings of the PumpkinX layer. Kept apart from `pumpkin.toml` so the
//! upstream config format stays untouched.

use std::{fmt::Write as _, fs, path::Path};

use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

pub const FILE_NAME: &str = "pumpkinx.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PumpkinXConfig {
    pub dashboard: DashboardConfig,
    pub multiversion: MultiversionConfig,
    pub patchbukkit: PatchBukkitConfig,
    pub keystone: KeystoneConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    /// Serve the web dashboard.
    pub enabled: bool,
    /// Address the dashboard listens on.
    pub address: String,
    /// One-time setup token for creating the first account. Empty once an account exists.
    pub token: String,
    /// Offer the file manager. Every account can then upload files into `plugins/`, which run
    /// as code on this machine.
    pub file_manager: bool,
    /// Dashboard accounts, managed from the dashboard.
    pub users: Vec<DashboardUser>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DashboardUser {
    pub username: String,
    /// Argon2 hash. The password itself is never stored.
    pub password_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiversionConfig {
    /// Admit Java clients older than 26.3 and translate their packets. Only some versions
    /// can finish joining so far; see PUMPKINX.md.
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KeystoneConfig {
    /// Run the built-in Keystone plugin: economy, shop, auctions, roles, sidebar, login and skins.
    pub enabled: bool,
}

impl Default for KeystoneConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PatchBukkitConfig {
    /// Run Bukkit, Spigot and Paper plugins from `plugins/data/patchbukkit/patchbukkit-plugins`.
    /// Needs Java 25 or newer.
    pub enabled: bool,
}

impl Default for PumpkinXConfig {
    fn default() -> Self {
        Self {
            dashboard: DashboardConfig::default(),
            multiversion: MultiversionConfig::default(),
            patchbukkit: PatchBukkitConfig::default(),
            keystone: KeystoneConfig::default(),
        }
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            address: "0.0.0.0:8080".to_string(),
            token: String::new(),
            file_manager: true,
            users: Vec::new(),
        }
    }
}

impl Default for MultiversionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

pub(crate) fn generate_token() -> String {
    let mut rng = rand::rng();
    let mut token = String::with_capacity(48);
    for _ in 0..24 {
        let _ = write!(token, "{:02x}", rng.random::<u8>());
    }
    token
}

impl PumpkinXConfig {
    /// Reads `pumpkinx.toml` from `dir`, creating it (with a fresh dashboard token) when missing.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        let mut config = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                error!("Couldn't parse {FILE_NAME}: {err}. Using defaults for this run.");
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        if config.dashboard.users.is_empty() && config.dashboard.token.trim().is_empty() {
            config.dashboard.token = generate_token();
            if let Err(err) = config.save(dir) {
                warn!("Couldn't write {FILE_NAME}: {err}");
            }
        } else if !path.exists()
            && let Err(err) = config.save(dir)
        {
            warn!("Couldn't write {FILE_NAME}: {err}");
        }
        config
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        fs::write(dir.join(FILE_NAME), text).map_err(|e| e.to_string())
    }
}
