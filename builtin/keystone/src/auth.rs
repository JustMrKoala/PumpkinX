//! /register and /login for offline mode servers. Uses the same password hash format
//! as the Java version, so players.yml can move between both.

use crate::core::{self, now_millis, send, uuid_of, with, Tell};
use crate::text;
use base64::Engine;
use pumpkin_plugin_api::player::{JavaKickOptions, SocketTeardownPolicy};
use pumpkin_plugin_api::Player;
use std::collections::{HashMap, HashSet};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
/// Lower than the Java version (210,000) because WebAssembly hashes on the server thread.
/// The count is stored with each hash, so both versions read each other's passwords.
const ITERATIONS: u32 = 100_000;

#[derive(Default)]
pub struct AuthState {
    authenticated: HashSet<String>,
    attempts: HashMap<String, u32>,
    joined: HashMap<String, i64>,
}

pub fn hash(password: &str) -> String {
    let mut salt = [0u8; 16];
    let _ = getrandom::fill(&mut salt);
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), &salt, ITERATIONS, &mut out);
    format!("pbkdf2${ITERATIONS}${}${}", B64.encode(salt), B64.encode(out))
}

pub fn verify(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 4 || parts[0] != "pbkdf2" {
        return false;
    }
    let (Ok(iter), Ok(salt), Ok(expected)) = (parts[1].parse::<u32>(), B64.decode(parts[2]), B64.decode(parts[3])) else { return false };
    let mut out = vec![0u8; expected.len()];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), &salt, iter, &mut out);
    // Constant time compare.
    out.len() == expected.len() && out.iter().zip(expected.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn enabled() -> bool {
    core::module("auth")
}

pub fn is_authenticated(p: &Player) -> bool {
    if !enabled() {
        return true;
    }
    let id = uuid_of(p);
    with(|s| s.auth.authenticated.contains(&id))
}

pub fn is_locked(p: &Player) -> bool {
    !is_authenticated(p)
}

pub fn kick(p: &Player, key: &str) {
    let reason = text::parse(&core::msg(key, &[]));
    if let Some(j) = p.as_java() {
        j.kick(JavaKickOptions { reason, log_to_console: true, teardown_policy: SocketTeardownPolicy::Graceful });
    }
}

pub fn on_join(p: &Player) {
    let id = uuid_of(p);
    if !enabled() {
        with(|s| s.auth.authenticated.insert(id));
        return;
    }
    let ip = p.get_ip();
    let session = with(|s| {
        s.auth.authenticated.remove(&id);
        let mins = s.config.i64("auth.session-minutes", 30);
        let d = s.players.get(&id)?;
        (d.password.is_some() && mins > 0 && d.last_ip.as_deref() == Some(ip.as_str()) && now_millis() - d.last_login < mins * 60_000).then_some(())
    });
    if session.is_some() {
        complete(p, "auth.session-resumed");
    } else {
        start_pending(p);
    }
}

fn start_pending(p: &Player) {
    let id = uuid_of(p);
    with(|s| {
        s.auth.attempts.insert(id.clone(), 0);
        s.auth.joined.insert(id, now_millis());
    });
    p.set_movement_locked(true);
    remind(p);
    crate::sidebar::update(p);
}

fn registered(id: &str) -> bool {
    with(|s| s.players.get(id).map(|d| d.password.is_some()).unwrap_or(false))
}

fn remind(p: &Player) {
    let key = if registered(&uuid_of(p)) { "auth.login" } else { "auth.register" };
    send(p, &format!("{key}-prompt"), &[]);
    p.send_title_animation(0, 120, 10);
    p.show_subtitle(text::parse(&core::msg(&format!("{key}-subtitle"), &[])));
    p.show_title(text::parse(&core::msg(&format!("{key}-title"), &[])));
}

/// Every five seconds: remind people who still need to log in, kick the slow ones.
pub fn tick() {
    if !enabled() {
        return;
    }
    let timeout = with(|s| s.config.i64("auth.timeout-seconds", 60));
    for p in core::online_players() {
        let id = uuid_of(&p);
        let joined = with(|s| if s.auth.authenticated.contains(&id) { None } else { s.auth.joined.get(&id).copied() });
        let Some(joined) = joined else { continue };
        if timeout > 0 && now_millis() - joined >= timeout * 1000 {
            kick(&p, "auth.kick-timeout");
        } else {
            remind(&p);
        }
    }
}

fn complete(p: &Player, key: &str) {
    let id = uuid_of(p);
    let ip = p.get_ip();
    let name = p.get_name();
    with(|s| {
        s.auth.authenticated.insert(id.clone());
        s.auth.attempts.remove(&id);
        s.auth.joined.remove(&id);
        crate::data::ensure(s, &id, &name);
        if let Some(d) = s.players.get_mut(&id) {
            d.last_ip = Some(ip);
            d.last_login = now_millis();
        }
        s.players_dirty = true;
    });
    p.set_movement_locked(false);
    p.show_title(text::parse(""));
    send(p, key, &[]);
    crate::sidebar::update(p);
}

pub fn on_quit(p: &Player) {
    let id = uuid_of(p);
    let on = enabled();
    with(|s| {
        if on && s.auth.authenticated.contains(&id) {
            if let Some(d) = s.players.get_mut(&id) {
                d.last_login = now_millis();
            }
        }
        s.auth.authenticated.remove(&id);
        s.auth.attempts.remove(&id);
        s.auth.joined.remove(&id);
    });
}

/// Checks before a player is let in. Returns a kick message key when they must be refused.
pub fn pre_login(name: &str) -> Option<(String, Vec<(String, String)>)> {
    if !enabled() {
        return None;
    }
    if name.len() < 3 || name.len() > 16 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some(("auth.kick-bad-name".into(), vec![]));
    }
    // In offline mode anyone can pick any name, so do not let a newcomer kick a logged in player.
    if let Some(online) = core::server(|s| s.get_player_by_name(name)) {
        if is_authenticated(&online) {
            return Some(("auth.kick-already-online".into(), vec![]));
        }
    }
    let existing = with(|s| s.players.values().find(|d| d.name.eq_ignore_ascii_case(name) && d.password.is_some()).map(|d| d.name.clone()));
    match existing {
        Some(real) if real != name => Some(("auth.kick-wrong-case".into(), vec![("name".into(), real)])),
        _ => None,
    }
}

pub fn allowed_command(cmd: &str) -> bool {
    let label = cmd.trim_start_matches('/').split_whitespace().next().unwrap_or("").to_lowercase();
    let label = label.rsplit(':').next().unwrap_or(&label).to_string();
    with(|s| s.config.list("auth.allowed-commands").iter().any(|c| c.eq_ignore_ascii_case(&label)))
}

fn check_password(pw: &str) -> Option<&'static str> {
    let (min, max) = with(|s| (s.config.i64("auth.min-password-length", 6) as usize, s.config.i64("auth.max-password-length", 64) as usize));
    if pw.chars().count() < min { Some("auth.too-short") } else if pw.chars().count() > max { Some("auth.too-long") } else { None }
}

fn lengths() -> (String, String) {
    with(|s| (s.config.i64("auth.min-password-length", 6).to_string(), s.config.i64("auth.max-password-length", 64).to_string()))
}

pub fn register(p: &Player, pw: &str, confirm: &str) {
    let id = uuid_of(p);
    if is_authenticated(p) { return send(p, "auth.already-logged-in", &[]); }
    if registered(&id) { return send(p, "auth.already-registered", &[]); }
    if pw != confirm { return send(p, "auth.no-match", &[]); }
    if let Some(k) = check_password(pw) {
        let (min, max) = lengths();
        return send(p, k, &[("min", &min), ("max", &max)]);
    }
    let ip = p.get_ip();
    let limit = with(|s| {
        let max = s.config.i64("auth.max-accounts-per-ip", 3);
        let count = s.players.values().filter(|d| d.password.is_some() && d.last_ip.as_deref() == Some(ip.as_str())).count() as i64;
        (max > 0 && count >= max).then_some(max)
    });
    if let Some(max) = limit {
        return send(p, "auth.too-many-accounts", &[("max", &max.to_string())]);
    }
    let h = hash(pw);
    let name = p.get_name();
    with(|s| {
        crate::data::ensure(s, &id, &name);
        s.players.get_mut(&id).unwrap().password = Some(h);
    });
    crate::data::save_now();
    complete(p, "auth.registered");
}

pub fn login(p: &Player, pw: &str) {
    let id = uuid_of(p);
    if is_authenticated(p) { return send(p, "auth.already-logged-in", &[]); }
    let Some(stored) = with(|s| s.players.get(&id).and_then(|d| d.password.clone())) else { return send(p, "auth.not-registered", &[]) };
    if verify(pw, &stored) {
        return complete(p, "auth.logged-in");
    }
    let (n, max) = with(|s| {
        let n = s.auth.attempts.entry(id.clone()).or_default();
        *n += 1;
        (*n, s.config.i64("auth.max-attempts", 5) as u32)
    });
    if max > 0 && n >= max {
        kick(p, "auth.kick-attempts");
    } else {
        send(p, "auth.wrong-password", &[("left", &max.saturating_sub(n).to_string())]);
    }
}

pub fn change_password(p: &Player, old: &str, new: &str) {
    let id = uuid_of(p);
    let stored = with(|s| s.players.get(&id).and_then(|d| d.password.clone()));
    let Some(stored) = stored.filter(|_| is_authenticated(p)) else { return send(p, "auth.not-registered", &[]) };
    if let Some(k) = check_password(new) {
        let (min, max) = lengths();
        return send(p, k, &[("min", &min), ("max", &max)]);
    }
    if !verify(old, &stored) {
        return send(p, "auth.wrong-password", &[("left", "?")]);
    }
    let h = hash(new);
    with(|s| s.players.get_mut(&id).unwrap().password = Some(h));
    crate::data::save_now();
    send(p, "auth.password-changed", &[]);
}

pub fn logout(p: &Player) {
    let id = uuid_of(p);
    let was = with(|s| {
        let was = s.auth.authenticated.remove(&id);
        if let Some(d) = s.players.get_mut(&id) { d.last_login = 0; }
        was
    });
    if enabled() && was {
        send(p, "auth.logged-out", &[]);
        start_pending(p);
    }
}

pub fn force_login(p: &Player) {
    if !is_authenticated(p) {
        complete(p, "auth.logged-in");
    }
}

pub fn unregister(uuid: &str) {
    with(|s| {
        if let Some(d) = s.players.get_mut(uuid) {
            d.password = None;
            d.last_login = 0;
        }
        s.auth.authenticated.remove(uuid);
    });
    crate::data::save_now();
    if enabled() {
        if let Some(p) = core::player_by_uuid(uuid) {
            start_pending(&p);
        }
    }
}

pub fn set_password(uuid: &str, pw: &str) {
    let h = hash(pw);
    with(|s| if let Some(d) = s.players.get_mut(uuid) { d.password = Some(h) });
    crate::data::save_now();
}

/// When auth is switched on while people are online, they have to log in too.
pub fn reload() {
    if !enabled() {
        return;
    }
    for p in core::online_players() {
        let id = uuid_of(&p);
        let known = with(|s| s.auth.authenticated.contains(&id) || s.auth.joined.contains_key(&id));
        if !known {
            on_join(&p);
        }
    }
}

pub fn auth_cmd(name: &str, sender: &dyn Tell, args: &[String]) {
    if !enabled() {
        return send(sender, "general.module-disabled", &[("module", "auth")]);
    }
    if name == "authadmin" {
        return admin(sender, args);
    }
    let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
    match name {
        "register" => if args.len() < 2 { send(&p, "auth.register-usage", &[]) } else { register(&p, &args[0], &args[1]) },
        "login" => if args.is_empty() { send(&p, "auth.login-usage", &[]) } else { login(&p, &args[0]) },
        "logout" => logout(&p),
        "changepassword" => if args.len() < 2 { send(&p, "auth.change-usage", &[]) } else { change_password(&p, &args[0], &args[1]) },
        _ => {}
    }
}

fn admin(sender: &dyn Tell, args: &[String]) {
    if !sender.has("keystone.auth.admin") { return send(sender, "general.no-permission", &[]); }
    if args.len() < 2 { return send(sender, "auth.admin-usage", &[]); }
    let Some(id) = core::lookup(&args[1]) else { return send(sender, "general.unknown-player", &[("player", &args[1])]) };
    let name = core::name_of(&id);
    match args[0].to_lowercase().as_str() {
        "forcelogin" => match core::player_by_uuid(&id) {
            Some(p) => { force_login(&p); send(sender, "auth.admin-done", &[("player", &name)]) }
            None => send(sender, "general.unknown-player", &[("player", &args[1])]),
        },
        "unregister" => { unregister(&id); send(sender, "auth.admin-done", &[("player", &name)]) }
        "setpassword" => match args.get(2) {
            Some(pw) => { set_password(&id, pw); send(sender, "auth.admin-done", &[("player", &name)]) }
            None => send(sender, "auth.admin-usage", &[]),
        },
        "info" => {
            let (reg, ip, last) = with(|s| {
                let d = &s.players[&id];
                (d.password.is_some().to_string(), d.last_ip.clone().unwrap_or("?".into()), d.last_login.to_string())
            });
            send(sender, "auth.admin-info", &[("player", &name), ("registered", &reg), ("ip", &ip), ("last", &last)]);
        }
        _ => send(sender, "auth.admin-usage", &[]),
    }
}
