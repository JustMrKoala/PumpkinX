//! Shared state and the helpers every module uses: messages, branding, placeholders,
//! module switches and player lookups.
//!
//! Pumpkin may call back into the plugin while it is inside a host call, so state is
//! only ever borrowed for pure Rust work. Host calls (sending messages, opening menus)
//! happen after the borrow ends.

use crate::data::PlayerData;
use crate::text;
use crate::yaml::Yaml;
use pumpkin_plugin_api::command::CommandSender;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{Player, Server};
use std::cell::RefCell;
use std::collections::HashMap;

pub const DEFAULT_CONFIG: &str = include_str!("../resources/config.yml");
pub const DEFAULT_MESSAGES: &str = include_str!("../resources/messages.yml");
pub const DEFAULT_ROLES: &str = include_str!("../resources/roles.yml");
pub const DEFAULT_SIDEBAR: &str = include_str!("../resources/sidebar.yml");
pub const DEFAULT_SHOP: &str = include_str!("../resources/shop.yml");

/// What we know about someone who is online right now.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Online {
    pub name: String,
    pub entity_id: u32,
}

pub struct State {
    pub folder: String,
    pub config: Yaml,
    pub messages: Yaml,
    pub players: HashMap<String, PlayerData>,
    pub players_dirty: bool,
    pub online: HashMap<String, Online>,
    pub roles: crate::roles::Roles,
    pub shop: crate::shop::Shop,
    pub sidebar: crate::sidebar::Sidebar,
    pub auctions: crate::auction::Auctions,
    pub auth: crate::auth::AuthState,
    pub skins: crate::skins::SkinCache,
    pub rewards: crate::rewards::RewardState,
    pub admin_command: String,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static SERVER: RefCell<Option<Server>> = const { RefCell::new(None) };
}

pub fn install(state: State, server: Server) {
    STATE.with(|s| *s.borrow_mut() = Some(state));
    SERVER.with(|s| *s.borrow_mut() = Some(server));
}

/// Runs pure Rust code against the state. Never call into the server from inside.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut guard = s.borrow_mut();
        f(guard.as_mut().expect("Keystone state used before on_load"))
    })
}

pub fn server<R>(f: impl FnOnce(&Server) -> R) -> R {
    SERVER.with(|s| f(s.borrow().as_ref().expect("server not set")))
}

pub fn log(msg: &str) {
    tracing::info!("{msg}");
}

pub fn warn(msg: &str) {
    tracing::warn!("{msg}");
}

pub fn uuid_of(p: &Player) -> String {
    let id = p.get_id();
    let hex = format!("{:016x}{:016x}", id.high, id.low);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- modules and branding ----

impl State {
    /// Reads modules.<name>. "auto" means on when the server runs in offline mode.
    pub fn module(&self, name: &str, online_mode: bool) -> bool {
        let def = if name == "auth" { "false" } else { "true" };
        let v = self.config.str(&format!("modules.{name}"), def);
        let on = if v.eq_ignore_ascii_case("auto") { !online_mode } else { v.eq_ignore_ascii_case("true") };
        if on && matches!(name, "shop" | "rewards" | "auctions") {
            return self.module("economy", online_mode);
        }
        on
    }

    pub fn brand_name(&self) -> String {
        let n = self.config.str("branding.name", "MyServer");
        if n.trim().is_empty() { "MyServer".into() } else { n }
    }

    pub fn brand_display(&self) -> String {
        let d = self.config.str("branding.display", "");
        if d.trim().is_empty() { format!("<gold>{}", self.brand_name()) } else { d }
    }

    pub fn brand_command(&self) -> String {
        let mut c = self.config.str("branding.command", "");
        if c.trim().is_empty() {
            c = self.brand_name();
        }
        let c: String = c.to_lowercase().chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect();
        if c.is_empty() { "panel".into() } else { c }
    }

    pub fn brand(&self, s: &str) -> String {
        if !s.contains('{') {
            return s.to_string();
        }
        s.replace("{server}", &self.brand_display())
            .replace("{server_name}", &self.brand_name())
            .replace("{server_address}", &self.config.str("branding.address", ""))
            .replace("{command}", &self.admin_command)
    }

    /// A message from messages.yml with {key} values filled in.
    pub fn msg(&self, key: &str, kv: &[(&str, &str)]) -> String {
        let mut s = self.messages.str(key, key);
        for (k, v) in kv {
            s = s.replace(&format!("{{{k}}}"), v);
        }
        self.brand(&s)
    }

    pub fn prefix(&self) -> String {
        self.brand(&self.messages.str("prefix", ""))
    }

    pub fn find_by_name(&self, name: &str) -> Option<String> {
        self.players.iter().find(|(_, d)| d.name.eq_ignore_ascii_case(name)).map(|(k, _)| k.clone())
    }
}

/// Module switch, safe to call from anywhere.
pub fn module(name: &str) -> bool {
    let online = server(|s| s.is_online_mode());
    with(|s| s.module(name, online))
}

// ---- sending messages ----

/// Anything that can receive a chat message.
pub trait Tell {
    fn tell_component(&self, c: TextComponent);
    fn has(&self, node: &str) -> bool;
    fn player(&self) -> Option<Player>;
}

impl Tell for Player {
    fn tell_component(&self, c: TextComponent) {
        self.send_system_message(c, false);
    }
    fn has(&self, node: &str) -> bool {
        self.has_permission(node)
    }
    fn player(&self) -> Option<Player> {
        crate::core::server(|s| s.get_player_by_uuid(self.get_id()))
    }
}

impl Tell for CommandSender {
    fn tell_component(&self, c: TextComponent) {
        self.send_message(c);
    }
    fn has(&self, node: &str) -> bool {
        if self.is_console() {
            return true;
        }
        server(|s| self.has_permission(s, node))
    }
    fn player(&self) -> Option<Player> {
        self.as_player()
    }
}

/// Sends a message from messages.yml with the chat prefix. Empty messages are skipped.
pub fn send<T: Tell + ?Sized>(to: &T, key: &str, kv: &[(&str, &str)]) {
    let text = with(|s| {
        let m = s.msg(key, kv);
        if m.is_empty() { None } else { Some(s.prefix() + &m) }
    });
    if let Some(t) = text {
        to.tell_component(text::parse(&t));
    }
}

/// Sends raw MiniMessage with the chat prefix.
pub fn say<T: Tell + ?Sized>(to: &T, mm: &str) {
    let t = with(|s| s.prefix() + &s.brand(mm));
    to.tell_component(text::parse(&t));
}

pub fn msg(key: &str, kv: &[(&str, &str)]) -> String {
    with(|s| s.msg(key, kv))
}

pub fn online_players() -> Vec<Player> {
    server(|s| s.get_all_players())
}

pub fn player_by_uuid(uuid: &str) -> Option<Player> {
    online_players().into_iter().find(|p| uuid_of(p) == uuid)
}

/// Finds a player by name: online players first, then everyone who ever joined.
pub fn lookup(name: &str) -> Option<String> {
    if let Some(p) = server(|s| s.get_player_by_name(name)) {
        let id = uuid_of(&p);
        let pname = p.get_name();
        with(|s| {
            s.players.entry(id.clone()).or_insert_with(|| PlayerData::new(&pname, s.config.f64("economy.starting-balance", 0.0)));
        });
        return Some(id);
    }
    with(|s| s.find_by_name(name))
}

pub fn name_of(uuid: &str) -> String {
    with(|s| s.players.get(uuid).map(|d| d.name.clone()).unwrap_or_else(|| uuid.to_string()))
}

/// Remembers which permission nodes were requested so wildcards can grant them all.
pub static KNOWN_PERMISSIONS: &[&str] = &[
    "keystone.admin", "keystone.balance", "keystone.balance.others", "keystone.pay", "keystone.baltop",
    "keystone.eco", "keystone.shop", "keystone.sell", "keystone.shop.admin", "keystone.role.admin",
    "keystone.sidebar.admin", "keystone.auth.admin", "keystone.skin", "keystone.skin.others",
    "keystone.chat.color", "keystone.ah", "keystone.ah.sell", "keystone.ah.admin",
];
