//! players.yml, in the same layout as the Java version.

use crate::core::{self, State};
use crate::yaml::Yaml;
use std::collections::HashMap;

#[derive(Clone, Default)]
pub struct PlayerData {
    pub name: String,
    pub balance: f64,
    pub role: Option<String>,
    pub password: Option<String>,
    pub last_ip: Option<String>,
    pub last_login: i64,
    pub kills: i64,
    pub deaths: i64,
    pub sidebar_hidden: bool,
    pub skin: Option<String>,
}

impl PlayerData {
    pub fn new(name: &str, balance: f64) -> PlayerData {
        PlayerData { name: name.to_string(), balance, ..Default::default() }
    }
}

pub fn load(folder: &str) -> HashMap<String, PlayerData> {
    let y = Yaml::load(&format!("{folder}/players.yml"), None);
    let mut out = HashMap::new();
    for id in y.keys("") {
        let p = |k: &str| format!("{id}.{k}");
        out.insert(id.clone(), PlayerData {
            name: y.str(&p("name"), "unknown"),
            balance: y.f64(&p("balance"), 0.0),
            role: y.opt_str(&p("role")),
            password: y.opt_str(&p("password")),
            last_ip: y.opt_str(&p("last-ip")),
            last_login: y.i64(&p("last-login"), 0),
            kills: y.i64(&p("kills"), 0),
            deaths: y.i64(&p("deaths"), 0),
            sidebar_hidden: y.bool(&p("sidebar-hidden"), false),
            skin: y.opt_str(&p("skin")),
        });
    }
    out
}

pub fn save(s: &State) {
    let mut y = Yaml::empty(&format!("{}/players.yml", s.folder));
    for (id, d) in &s.players {
        let p = |k: &str| format!("{id}.{k}");
        y.set_str(&p("name"), &d.name);
        y.set_f64(&p("balance"), d.balance);
        if let Some(r) = &d.role { y.set_str(&p("role"), r); }
        if let Some(pw) = &d.password { y.set_str(&p("password"), pw); }
        if let Some(ip) = &d.last_ip { y.set_str(&p("last-ip"), ip); }
        y.set_i64(&p("last-login"), d.last_login);
        y.set_i64(&p("kills"), d.kills);
        y.set_i64(&p("deaths"), d.deaths);
        y.set_bool(&p("sidebar-hidden"), d.sidebar_hidden);
        if let Some(sk) = &d.skin { y.set_str(&p("skin"), sk); }
    }
    y.save();
}

pub fn save_now() {
    core::with(|s| {
        save(s);
        s.players_dirty = false;
    });
}

/// The data for a player, created with the starting balance when missing.
pub fn ensure(s: &mut State, uuid: &str, name: &str) {
    let start = s.config.f64("economy.starting-balance", 0.0);
    s.players.entry(uuid.to_string()).or_insert_with(|| PlayerData::new(name, start));
}
