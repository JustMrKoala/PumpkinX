//! Premium skins on offline mode servers: looks up the premium account with the
//! player's name at Mojang and puts its signed skin on the player.

use crate::core::{self, now_millis, send, uuid_of, with, Tell};
use crate::yaml::Yaml;
use pumpkin_plugin_api::player::PlayerSkin;
use pumpkin_plugin_api::Player;
use std::collections::HashMap;

#[derive(Clone)]
pub struct Skin {
    /// None means the name has no premium account.
    pub value: Option<String>,
    pub signature: Option<String>,
    pub fetched: i64,
}

#[derive(Default)]
pub struct SkinCache {
    pub map: HashMap<String, Skin>,
    pub dirty: bool,
}

fn file(folder: &str) -> String {
    format!("{folder}/skins-cache.yml")
}

impl SkinCache {
    pub fn load(folder: &str) -> SkinCache {
        let y = Yaml::load(&file(folder), None);
        let mut map = HashMap::new();
        for name in y.keys("") {
            map.insert(name.clone(), Skin { value: y.opt_str(&format!("{name}.value")), signature: y.opt_str(&format!("{name}.signature")), fetched: y.i64(&format!("{name}.fetched"), 0) });
        }
        SkinCache { map, dirty: false }
    }

    pub fn save(&mut self, folder: &str) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let mut y = Yaml::empty(&file(folder));
        for (name, s) in &self.map {
            if let Some(v) = &s.value { y.set_str(&format!("{name}.value"), v); }
            if let Some(v) = &s.signature { y.set_str(&format!("{name}.signature"), v); }
            y.set_i64(&format!("{name}.fetched"), s.fetched);
        }
        y.save();
    }
}

fn get(url: &str) -> Option<(u16, String)> {
    let res = waki::Client::new()
        .get(url)
        .header("User-Agent", "Keystone-Minecraft-Plugin")
        .connect_timeout(std::time::Duration::from_secs(5))
        .send()
        .ok()?;
    let status = res.status_code();
    let body = res.body().ok()?;
    Some((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Looks a skin up, using the cache while it is fresh. None when Mojang could not be reached.
pub fn fetch(name: &str, force: bool) -> Option<Skin> {
    let key = name.to_lowercase();
    let cache_ms = with(|s| s.config.i64("skins.cache-hours", 12)) * 3_600_000;
    let cached = with(|s| s.skins.map.get(&key).cloned());
    if let Some(c) = &cached {
        if !force && now_millis() - c.fetched < cache_ms {
            return Some(c.clone());
        }
    }
    let store = |skin: Skin| {
        with(|s| {
            s.skins.map.insert(key.clone(), skin.clone());
            s.skins.dirty = true;
        });
        Some(skin)
    };
    let none = Skin { value: None, signature: None, fetched: now_millis() };
    let Some((status, body)) = get(&format!("https://api.mojang.com/users/profiles/minecraft/{name}")) else { return cached };
    if status == 204 || status == 404 {
        return store(none);
    }
    if status != 200 {
        return cached;
    }
    let id = serde_json::from_str::<serde_json::Value>(&body).ok()?.get("id")?.as_str()?.to_string();
    let Some((status, body)) = get(&format!("https://sessionserver.mojang.com/session/minecraft/profile/{id}?unsigned=false")) else { return cached };
    if status != 200 {
        return cached;
    }
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    for prop in json.get("properties")?.as_array()? {
        if prop.get("name").and_then(|n| n.as_str()) == Some("textures") {
            return store(Skin {
                value: prop.get("value").and_then(|v| v.as_str()).map(String::from),
                signature: prop.get("signature").and_then(|v| v.as_str()).map(String::from),
                fetched: now_millis(),
            });
        }
    }
    store(none)
}

pub fn apply(p: &Player, skin: &Skin) {
    if let Some(v) = &skin.value {
        p.set_skin(&PlayerSkin { value: v.clone(), signature: skin.signature.clone() });
    }
}

/// Runs before the player joins, off the main thread, so the lookup does not lag anyone.
pub fn prefetch(name: &str, uuid: &str) {
    if !core::module("skins") {
        return;
    }
    let chosen = with(|s| s.players.get(uuid).and_then(|d| d.skin.clone())).unwrap_or_else(|| name.to_string());
    let _ = fetch(&chosen, false);
}

pub fn on_join(p: &Player) {
    if !core::module("skins") {
        return;
    }
    let id = uuid_of(p);
    let chosen = with(|s| s.players.get(&id).and_then(|d| d.skin.clone())).unwrap_or_else(|| p.get_name());
    if let Some(skin) = fetch(&chosen, false) {
        apply(p, &skin);
    }
}

pub fn change(sender: &dyn Tell, who: &Player, name: &str, force: bool) {
    let own = name.eq_ignore_ascii_case(&who.get_name());
    send(sender, "skins.loading", &[("name", name)]);
    let Some(skin) = fetch(name, force) else { return send(sender, "skins.failed", &[]) };
    if skin.value.is_none() && !own {
        return send(sender, "skins.not-premium", &[("name", name)]);
    }
    let id = uuid_of(who);
    with(|s| {
        if let Some(d) = s.players.get_mut(&id) {
            d.skin = if own { None } else { Some(name.to_string()) };
        }
        s.players_dirty = true;
    });
    apply(who, &skin);
    send(sender, "skins.changed", &[("player", &who.get_name()), ("name", name)]);
}

pub fn skin_cmd(sender: &dyn Tell, args: &[String]) {
    if !core::module("skins") {
        return send(sender, "general.module-disabled", &[("module", "skins")]);
    }
    let Some(first) = args.first() else { return send(sender, "skins.usage", &[]) };
    let valid = |n: &str| !n.is_empty() && n.len() <= 16 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    match first.to_lowercase().as_str() {
        "set" => {
            if !sender.has("keystone.skin.others") { return send(sender, "general.no-permission", &[]); }
            let (Some(target), Some(name)) = (args.get(1), args.get(2)) else { return send(sender, "skins.usage", &[]) };
            let Some(p) = core::server(|s| s.get_player_by_name(target)) else { return send(sender, "general.unknown-player", &[("player", target)]) };
            if !valid(name) { return send(sender, "skins.bad-name", &[]); }
            change(sender, &p, name, false);
        }
        "reset" | "update" => {
            let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
            let id = uuid_of(&p);
            let force = first.eq_ignore_ascii_case("update");
            let name = if force { with(|s| s.players.get(&id).and_then(|d| d.skin.clone())).unwrap_or_else(|| p.get_name()) } else { p.get_name() };
            change(sender, &p, &name, force);
        }
        name => {
            let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
            if !valid(name) { return send(sender, "skins.bad-name", &[]); }
            change(sender, &p, &args[0], false);
        }
    }
}
