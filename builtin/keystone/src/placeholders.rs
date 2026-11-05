//! {placeholders} in sidebar lines, chat and tab formats.

use crate::core::{self, uuid_of, with};
use pumpkin_plugin_api::Player;

/// Everything a placeholder can need, read from the server before touching the state.
pub struct Snapshot {
    uuid: String,
    name: String,
    world: String,
    x: i64,
    y: i64,
    z: i64,
    ping: u32,
    health: f32,
    max_health: f32,
    food: u8,
    level: i32,
    online: u32,
    max_players: u32,
    tps: f64,
}

pub fn snapshot(p: &Player) -> Snapshot {
    let pos = p.get_position();
    let (online, max_players, tps) = core::server(|s| (s.get_player_count(), s.get_max_players(), s.get_tps()));
    Snapshot {
        uuid: uuid_of(p),
        name: p.get_name(),
        world: p.get_world().get_name(),
        x: pos.0.floor() as i64,
        y: pos.1.floor() as i64,
        z: pos.2.floor() as i64,
        ping: p.get_ping(),
        health: p.get_health(),
        max_health: p.get_max_health(),
        food: p.get_food_level(),
        level: p.get_experience_level(),
        online,
        max_players,
        tps,
    }
}

fn clock() -> (String, String) {
    // WASI has no time zone, so this is UTC.
    let secs = core::now_millis() / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m) = (rem / 3600, rem % 3600 / 60);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    (format!("{h:02}:{m:02}"), format!("{d:02}/{mo:02}/{y}"))
}

pub fn apply(snap: &Snapshot, s: &str) -> String {
    if !s.contains('{') {
        return s.to_string();
    }
    let (time, date) = clock();
    with(|st| {
        let d = st.players.get(&snap.uuid);
        let role = st.roles.role_of(d.and_then(|d| d.role.as_deref()));
        let bal = d.map(|d| d.balance).unwrap_or(0.0);
        let mut out = String::with_capacity(s.len() + 16);
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let key = &after[..end];
            let val = match key {
                "player" | "displayname" => Some(snap.name.clone()),
                "uuid" => Some(snap.uuid.clone()),
                "balance" => Some(st.format(bal)),
                "balance_raw" => Some(bal.to_string()),
                "balance_short" => Some(st.short_format(bal)),
                "currency" => Some(st.config.str("economy.currency-plural", "Coins")),
                "role" => Some(role.map(|r| r.display.clone()).unwrap_or_default()),
                "role_id" => Some(role.map(|r| r.id.clone()).unwrap_or_default()),
                "role_prefix" => Some(role.map(|r| r.prefix.clone()).unwrap_or_default()),
                "role_suffix" => Some(role.map(|r| r.suffix.clone()).unwrap_or_default()),
                "online" => Some(snap.online.to_string()),
                "max_players" => Some(snap.max_players.to_string()),
                "world" => Some(snap.world.rsplit(':').next().unwrap_or(&snap.world).to_string()),
                "x" => Some(snap.x.to_string()),
                "y" => Some(snap.y.to_string()),
                "z" => Some(snap.z.to_string()),
                "ping" => Some(snap.ping.to_string()),
                "health" => Some((snap.health.ceil() as i64).to_string()),
                "max_health" => Some((snap.max_health.ceil() as i64).to_string()),
                "food" => Some(snap.food.to_string()),
                "level" => Some(snap.level.to_string()),
                "kills" => Some(d.map(|d| d.kills).unwrap_or(0).to_string()),
                "deaths" => Some(d.map(|d| d.deaths).unwrap_or(0).to_string()),
                "tps" => Some(format!("{:.1}", snap.tps.min(20.0))),
                "time" => Some(time.clone()),
                "date" => Some(date.clone()),
                "world_time" => Some(time.clone()),
                "server" => Some(st.brand_display()),
                "server_name" => Some(st.brand_name()),
                "server_address" => Some(st.config.str("branding.address", "")),
                "command" => Some(st.admin_command.clone()),
                _ => None,
            };
            match val {
                Some(v) => out.push_str(&v),
                None => {
                    out.push('{');
                    out.push_str(key);
                    out.push('}');
                }
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    })
}
