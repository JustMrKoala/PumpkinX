//! The sidebar on the right side of the screen and the role name tags, using each
//! player's own scoreboard. Lines are team prefixes, so they can hold any colors.

use crate::core::{self, send, uuid_of, with, Tell};
use crate::placeholders;
use crate::text;
use crate::yaml::Yaml;
use pumpkin_plugin_api::common::NamedColor;
use pumpkin_plugin_api::scoreboard::{DisplaySlot, NumberFormat, RenderType};
use pumpkin_plugin_api::{CollisionRule, NametagVisibility, Player, TeamSettings};
use std::collections::{HashMap, HashSet};

pub const MAX_LINES: usize = 15;
const OBJECTIVE: &str = "ks_sidebar";

#[derive(Clone, Default)]
pub struct Board {
    pub id: String,
    pub roles: Vec<String>,
    pub worlds: Vec<String>,
    pub title: Vec<String>,
    pub lines: Vec<String>,
}

#[derive(Default)]
struct Shown {
    title: Option<String>,
    lines: Vec<String>,
    /// Name tag teams on this player's scoreboard: team to (settings, members).
    teams: HashMap<String, (String, HashSet<String>)>,
}

#[derive(Default)]
pub struct Sidebar {
    pub boards: Vec<Board>,
    pub update_ticks: u64,
    pub frame_ticks: u64,
    pub tick: u64,
    shown: HashMap<String, Shown>,
}

impl Sidebar {
    pub fn load(folder: &str) -> Sidebar {
        let y = Yaml::load(&format!("{folder}/sidebar.yml"), Some(core::DEFAULT_SIDEBAR));
        let mut boards = Vec::new();
        for id in y.keys("boards") {
            let p = |k: &str| format!("boards.{id}.{k}");
            boards.push(Board {
                id: id.to_lowercase(),
                roles: y.list(&p("roles")).into_iter().map(|s| s.to_lowercase()).collect(),
                worlds: y.list(&p("worlds")),
                title: y.list(&p("title")),
                lines: y.list(&p("lines")),
            });
        }
        Sidebar {
            boards,
            update_ticks: y.i64("update-ticks", 20).max(1) as u64,
            frame_ticks: y.i64("title-frame-ticks", 10).max(1) as u64,
            tick: 0,
            shown: HashMap::new(),
        }
    }

    /// Reloads the boards but keeps track of what players already see.
    pub fn reload(&mut self, folder: &str) {
        let fresh = Sidebar::load(folder);
        self.boards = fresh.boards;
        self.update_ticks = fresh.update_ticks;
        self.frame_ticks = fresh.frame_ticks;
    }

    pub fn save(&self, folder: &str) {
        let path = format!("{folder}/sidebar.yml");
        let mut y = Yaml::load(&path, None);
        y.remove("boards");
        y.set_i64("update-ticks", self.update_ticks as i64);
        y.set_i64("title-frame-ticks", self.frame_ticks as i64);
        for b in &self.boards {
            let p = |k: &str| format!("boards.{}.{k}", b.id);
            y.set_list(&p("roles"), &b.roles);
            y.set_list(&p("worlds"), &b.worlds);
            y.set_list(&p("title"), &b.title);
            y.set_list(&p("lines"), &b.lines);
        }
        y.save();
    }

    pub fn board(&self, id: &str) -> Option<&Board> {
        self.boards.iter().find(|b| b.id == id.to_lowercase())
    }

    pub fn board_mut(&mut self, id: &str) -> Option<&mut Board> {
        self.boards.iter_mut().find(|b| b.id == id.to_lowercase())
    }

    /// The first board for this role and world, else the board called "default".
    fn board_for(&self, role: &str, world: &str) -> Option<&Board> {
        let mut fallback = None;
        for b in &self.boards {
            if !b.worlds.is_empty() && !b.worlds.iter().any(|w| w == world || world.ends_with(&format!(":{w}"))) {
                continue;
            }
            if b.roles.is_empty() {
                if fallback.is_none() || b.id == "default" {
                    fallback = Some(b);
                }
                continue;
            }
            if b.roles.iter().any(|r| r == role) {
                return Some(b);
            }
        }
        fallback
    }
}

fn line_entry(i: usize) -> String {
    format!("§{:x}§r", i)
}

fn settings(prefix: &str, suffix: &str, color: NamedColor) -> TeamSettings {
    TeamSettings {
        display_name: text::parse(""),
        friendly_fire: true,
        see_friendly_invisibles: false,
        nametag_visibility: NametagVisibility::Always,
        collision_rule: CollisionRule::Always,
        color,
        prefix: text::parse(prefix),
        suffix: text::parse(suffix),
    }
}

/// Updates the sidebar of one player.
pub fn update(p: &Player) {
    let Some(java) = p.as_java() else { return };
    let id = uuid_of(p);
    let enabled = core::module("sidebar");
    let world = p.get_world().get_name();
    let authed = crate::auth::is_authenticated(p);
    let plan = with(|s| {
        let hidden = s.players.get(&id).map(|d| d.sidebar_hidden).unwrap_or(false);
        if !enabled || hidden || !authed {
            return None;
        }
        let role = s.player_role(&id).map(|r| r.id).unwrap_or_default();
        let b = s.sidebar.board_for(&role, &world)?;
        let title = if b.title.is_empty() {
            String::new()
        } else {
            b.title[(s.sidebar.tick / s.sidebar.frame_ticks) as usize % b.title.len()].clone()
        };
        Some((title, b.lines.iter().take(MAX_LINES).cloned().collect::<Vec<_>>()))
    });
    let sb = java.get_scoreboard();
    let Some((title, lines)) = plan else {
        let had = with(|s| s.sidebar.shown.get_mut(&id).map(|sh| (sh.title.take().is_some(), std::mem::take(&mut sh.lines).len())));
        if let Some((true, count)) = had {
            sb.remove_objective(OBJECTIVE);
            for i in 0..count {
                sb.remove_team(&format!("ks_l{i}"));
            }
        }
        return;
    };
    let snap = placeholders::snapshot(p);
    let title = placeholders::apply(&snap, &title);
    let lines: Vec<String> = lines.iter().map(|l| placeholders::apply(&snap, l)).collect();
    let (old_title, old_lines) = with(|s| {
        let sh = s.sidebar.shown.entry(id.clone()).or_default();
        (sh.title.clone(), sh.lines.clone())
    });
    match &old_title {
        None => {
            sb.add_objective(OBJECTIVE, text::parse(&title), RenderType::Integer, Some(NumberFormat::Blank));
            sb.set_display_slot(DisplaySlot::Sidebar, OBJECTIVE);
        }
        Some(t) if *t != title => sb.update_objective(OBJECTIVE, text::parse(&title), RenderType::Integer, Some(NumberFormat::Blank)),
        _ => {}
    }
    let count = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let team = format!("ks_l{i}");
        let entry = line_entry(i);
        if i >= old_lines.len() {
            sb.create_team(&team, settings(line, "", NamedColor::White));
            sb.add_player_to_team(&team, &entry);
        } else if old_lines[i] != *line {
            sb.update_team(&team, settings(line, "", NamedColor::White));
        }
        if i >= old_lines.len() || old_lines.len() != count {
            sb.update_score(&entry, OBJECTIVE, (count - i) as i32, Some(NumberFormat::Blank));
        }
    }
    for i in count..old_lines.len() {
        sb.remove_score(&line_entry(i), OBJECTIVE);
        sb.remove_team(&format!("ks_l{i}"));
    }
    with(|s| {
        let sh = s.sidebar.shown.entry(id).or_default();
        sh.title = Some(title);
        sh.lines = lines;
    });
}

/// Called every `step` ticks; updates whenever the update or title period comes round.
pub fn tick(step: u64) {
    let due = with(|s| {
        let period = s.sidebar.update_ticks.min(s.sidebar.frame_ticks).max(1);
        let before = s.sidebar.tick / period;
        s.sidebar.tick += step;
        s.sidebar.tick / period != before
    });
    if due {
        for p in core::online_players() {
            update(&p);
        }
    }
}

pub fn quit(p: &Player) {
    let id = uuid_of(p);
    with(|s| s.sidebar.shown.remove(&id));
}

/// Puts everyone in a role team on every scoreboard, for prefixes above heads.
pub fn refresh_teams() {
    let enabled = core::module("roles") && with(|s| s.config.bool("roles.nametags", true));
    let players = core::online_players();
    let wanted: HashMap<String, (String, String, String, NamedColor, HashSet<String>)> = {
        let info: Vec<(String, String)> = players.iter().map(|p| (uuid_of(p), p.get_name())).collect();
        with(|s| {
            let mut m: HashMap<String, (String, String, String, NamedColor, HashSet<String>)> = HashMap::new();
            if !enabled {
                return m;
            }
            for (id, name) in &info {
                let Some(r) = s.player_role(id) else { continue };
                let team = format!("ks{:04}{}", 9999 - r.priority.clamp(0, 9999), r.id);
                let team: String = team.chars().take(32).collect();
                let color = r.name_color.as_deref().and_then(text::named_color).unwrap_or(NamedColor::White);
                let key = format!("{}|{}|{:?}", r.prefix, r.suffix, color as u8);
                m.entry(team).or_insert_with(|| (key, r.prefix.clone(), r.suffix.clone(), color, HashSet::new())).4.insert(name.clone());
            }
            m
        })
    };
    for viewer in &players {
        let Some(java) = viewer.as_java() else { continue };
        let vid = uuid_of(viewer);
        let have = with(|s| s.sidebar.shown.entry(vid.clone()).or_default().teams.clone());
        let sb = java.get_scoreboard();
        for (team, (key, members)) in &have {
            if !wanted.contains_key(team) {
                sb.remove_team(team);
                let _ = (key, members);
            }
        }
        let mut now: HashMap<String, (String, HashSet<String>)> = HashMap::new();
        for (team, (key, prefix, suffix, color, members)) in &wanted {
            match have.get(team) {
                None => {
                    sb.create_team(team, settings(prefix, suffix, *color));
                }
                Some((old_key, _)) if old_key != key => sb.update_team(team, settings(prefix, suffix, *color)),
                _ => {}
            }
            let old_members = have.get(team).map(|(_, m)| m.clone()).unwrap_or_default();
            for m in members.difference(&old_members) {
                sb.add_player_to_team(team, m);
            }
            for m in old_members.difference(members) {
                sb.remove_player_from_team(team, m);
            }
            now.insert(team.clone(), (key.clone(), members.clone()));
        }
        with(|s| s.sidebar.shown.entry(vid).or_default().teams = now);
    }
}

pub fn sidebar_cmd(sender: &dyn Tell, args: &[String]) {
    if !core::module("sidebar") {
        return send(sender, "general.module-disabled", &[("module", "sidebar")]);
    }
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    if sub.is_empty() || sub == "toggle" {
        let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
        let id = uuid_of(&p);
        let hidden = with(|s| {
            s.players_dirty = true;
            s.players.get_mut(&id).map(|d| {
                d.sidebar_hidden = !d.sidebar_hidden;
                d.sidebar_hidden
            })
        });
        update(&p);
        return send(sender, if hidden == Some(true) { "sidebar.hidden" } else { "sidebar.shown" }, &[]);
    }
    if !sender.has("keystone.sidebar.admin") {
        return send(sender, "general.no-permission", &[]);
    }
    if sub == "list" {
        let boards = with(|s| s.sidebar.boards.clone());
        for b in boards {
            let roles = if b.roles.is_empty() { "everyone".into() } else { b.roles.join(", ") };
            send(sender, "sidebar.list-line", &[("board", &b.id), ("roles", &roles), ("lines", &b.lines.len().to_string())]);
        }
        return;
    }
    if args.len() < 2 {
        return send(sender, "sidebar.usage", &[]);
    }
    let id = args[1].to_lowercase();
    let rest = args[2..].join(" ");
    if sub == "create" {
        if with(|s| s.sidebar.board(&id).is_some()) {
            return send(sender, "sidebar.exists", &[("board", &id)]);
        }
        create(&id);
        return send(sender, "sidebar.created", &[("board", &id)]);
    }
    if with(|s| s.sidebar.board(&id).is_none()) {
        return send(sender, "sidebar.unknown", &[("board", &id)]);
    }
    if sub == "show" {
        let b = with(|s| s.sidebar.board(&id).cloned()).unwrap();
        for (i, t) in b.title.iter().enumerate() {
            send(sender, "sidebar.show-title", &[("n", &(i + 1).to_string()), ("text", &text::escape(t))]);
        }
        for (i, t) in b.lines.iter().enumerate() {
            send(sender, "sidebar.show-line", &[("n", &(i + 1).to_string()), ("text", &text::escape(t))]);
        }
        return;
    }
    let result: Result<(), (&str, String)> = with(|s| {
        if sub == "delete" {
            s.sidebar.boards.retain(|b| b.id != id);
            s.sidebar.save(&s.folder);
            return Ok(());
        }
        let b = s.sidebar.board_mut(&id).unwrap();
        match sub.as_str() {
            "title" => b.title = vec![rest.clone()],
            "addtitle" => b.title.push(rest.clone()),
            "add" => {
                if b.lines.len() >= MAX_LINES { return Err(("sidebar.too-many", String::new())); }
                b.lines.push(rest.clone());
            }
            "set" | "insert" | "remove" => {
                let n: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0);
                let max = if sub == "insert" { b.lines.len() + 1 } else { b.lines.len() };
                if n < 1 || n > max { return Err(("sidebar.bad-line", max.to_string())); }
                let line = args[3.min(args.len())..].join(" ");
                match sub.as_str() {
                    "set" => b.lines[n - 1] = line,
                    "remove" => { b.lines.remove(n - 1); }
                    _ => {
                        if b.lines.len() >= MAX_LINES { return Err(("sidebar.too-many", String::new())); }
                        b.lines.insert(n - 1, line);
                    }
                }
            }
            "roles" => b.roles = if rest.is_empty() || rest.eq_ignore_ascii_case("none") { vec![] } else { rest.to_lowercase().split([' ', ',']).filter(|x| !x.is_empty()).map(String::from).collect() },
            "worlds" => b.worlds = if rest.is_empty() || rest.eq_ignore_ascii_case("none") { vec![] } else { rest.split([' ', ',']).filter(|x| !x.is_empty()).map(String::from).collect() },
            _ => return Err(("sidebar.usage", String::new())),
        }
        s.sidebar.save(&s.folder);
        Ok(())
    });
    match result {
        Ok(()) => {
            for p in core::online_players() {
                update(&p);
            }
            send(sender, "sidebar.updated", &[("board", &id)]);
        }
        Err((key, max)) => send(sender, key, &[("max", &max)]),
    }
}

pub fn create(id: &str) {
    with(|s| {
        s.sidebar.boards.push(Board {
            id: id.to_lowercase(),
            title: vec![format!("<gold><bold>{id}")],
            lines: vec!["<white>Hello <yellow>{player}".into()],
            ..Default::default()
        });
        s.sidebar.save(&s.folder);
    });
}
