//! Money for mining, mob kills and playtime, plus kill and death counts.
//!
//! Pumpkin's death event does not say who killed what, so the last player hit on
//! every entity is remembered and used when it dies.

use crate::core::{self, uuid_of, with};
use crate::items;
use crate::text;
use pumpkin_plugin_api::common::{BlockPos, GameMode};
use pumpkin_plugin_api::Player;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct RewardState {
    placed: HashSet<(i32, i32, i32)>,
    /// Victim entity id to (attacker uuid, victim type, time).
    last_hit: HashMap<i32, (String, String, i64)>,
    minutes: HashMap<String, u32>,
}

const MAX_TRACKED: usize = 200_000;

/// "WitherSkeleton" becomes "WITHER_SKELETON".
pub fn upper_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

fn eligible(p: &Player) -> bool {
    if !crate::auth::is_authenticated(p) {
        return false;
    }
    let survival_only = with(|s| s.config.bool("rewards.survival-only", true));
    !survival_only || matches!(p.get_gamemode(), GameMode::Survival | GameMode::Adventure)
}

fn pay(p: &Player, base: f64, key: &str) {
    let id = uuid_of(p);
    let shown = with(|s| {
        let mult = s.player_role(&id).map(|r| r.money_multiplier).unwrap_or(1.0);
        let amount = s.round(base * mult);
        if amount <= 0.0 || !s.deposit(&id, amount) {
            return None;
        }
        if s.config.bool("rewards.show-actionbar", true) { Some(s.msg(key, &[("amount", &s.format(amount))])) } else { None }
    });
    if let Some(m) = shown {
        p.show_actionbar(text::parse(&m));
    }
}

pub fn on_place(pos: &BlockPos) {
    if !core::module("rewards") {
        return;
    }
    with(|s| {
        if s.rewards.placed.len() > MAX_TRACKED {
            s.rewards.placed.clear();
        }
        s.rewards.placed.insert((pos.x, pos.y, pos.z));
    });
}

pub fn on_break(p: &Player, block: &str, pos: &BlockPos) {
    if !core::module("rewards") {
        return;
    }
    // Placed blocks never pay, otherwise people farm money by placing and breaking ore.
    if with(|s| s.rewards.placed.remove(&(pos.x, pos.y, pos.z))) {
        return;
    }
    if !eligible(p) {
        return;
    }
    let mat = items::material_of(block);
    let amount = with(|s| s.config.f64(&format!("rewards.block-break.{mat}"), 0.0));
    if amount > 0.0 {
        pay(p, amount, "rewards.block");
    }
}

/// A player hit something: remember it so the death can be credited.
pub fn on_hit(victim_id: i32, damager_id: i32) {
    let attacker = core::online_players().into_iter().find(|p| p.as_entity().get_id() as i32 == damager_id);
    let Some(attacker) = attacker else { return };
    // The victim is almost always what the attacker is looking at.
    let kind = attacker
        .get_target_entity(6.0)
        .filter(|e| e.get_id() as i32 == victim_id)
        .map(|e| upper_snake(&format!("{:?}", e.get_type())))
        .or_else(|| core::online_players().into_iter().find(|p| p.as_entity().get_id() as i32 == victim_id).map(|_| "PLAYER".to_string()))
        .unwrap_or_default();
    let id = uuid_of(&attacker);
    with(|s| {
        if s.rewards.last_hit.len() > 10_000 {
            s.rewards.last_hit.clear();
        }
        s.rewards.last_hit.insert(victim_id, (id, kind, core::now_millis()));
    });
}

pub fn on_entity_death(entity_id: i32) {
    let hit = with(|s| s.rewards.last_hit.remove(&entity_id));
    let Some((attacker, kind, at)) = hit else { return };
    if core::now_millis() - at > 10_000 || kind.is_empty() || !core::module("rewards") {
        return;
    }
    let Some(p) = core::player_by_uuid(&attacker) else { return };
    if !eligible(&p) {
        return;
    }
    let amount = with(|s| s.config.f64(&format!("rewards.mob-kill.{kind}"), 0.0));
    if amount > 0.0 {
        pay(&p, amount, "rewards.mob");
    }
}

pub fn on_player_death(victim: &Player) {
    let vid = uuid_of(victim);
    let entity = victim.as_entity().get_id() as i32;
    let killer = with(|s| {
        if let Some(d) = s.players.get_mut(&vid) {
            d.deaths += 1;
        }
        s.players_dirty = true;
        s.rewards.last_hit.remove(&entity)
    });
    if let Some((killer, _, at)) = killer {
        if killer != vid && core::now_millis() - at < 10_000 {
            with(|s| {
                if let Some(d) = s.players.get_mut(&killer) {
                    d.kills += 1;
                }
            });
        }
    }
}

/// Runs every minute.
pub fn playtime() {
    if !core::module("rewards") {
        return;
    }
    let (every, amount) = with(|s| (s.config.i64("rewards.playtime.every-minutes", 0), s.config.f64("rewards.playtime.amount", 0.0)));
    if every <= 0 || amount <= 0.0 {
        return;
    }
    for p in core::online_players() {
        if !crate::auth::is_authenticated(&p) {
            continue;
        }
        let id = uuid_of(&p);
        let m = with(|s| {
            let m = s.rewards.minutes.entry(id).or_default();
            *m += 1;
            *m
        });
        if m as i64 % every == 0 {
            pay(&p, amount, "rewards.playtime");
        }
    }
}

pub fn quit(uuid: &str) {
    with(|s| s.rewards.minutes.remove(uuid));
}
