//! Roles from roles.yml, the /role command and role based chat.

use crate::core::{self, send, uuid_of, with, State, Tell, KNOWN_PERMISSIONS};
use crate::placeholders;
use crate::text;
use crate::yaml::Yaml;
use pumpkin_plugin_api::Player;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Default)]
pub struct Role {
    pub id: String,
    pub display: String,
    pub priority: i64,
    pub prefix: String,
    pub suffix: String,
    pub name_color: Option<String>,
    pub chat_format: Option<String>,
    pub money_multiplier: f64,
    pub permissions: Vec<String>,
    pub inherits: Vec<String>,
}

#[derive(Default)]
pub struct Roles {
    pub list: BTreeMap<String, Role>,
    pub default_role: String,
    /// Nodes set on each online player, so they can be removed on the next change.
    pub applied: HashMap<String, Vec<String>>,
}

impl Roles {
    pub fn load(folder: &str) -> Roles {
        let y = Yaml::load(&format!("{folder}/roles.yml"), Some(core::DEFAULT_ROLES));
        let mut list = BTreeMap::new();
        for id in y.keys("roles") {
            let p = |k: &str| format!("roles.{id}.{k}");
            let lid = id.to_lowercase();
            list.insert(lid.clone(), Role {
                id: lid,
                display: y.str(&p("display"), &id),
                priority: y.i64(&p("priority"), 0),
                prefix: y.str(&p("prefix"), ""),
                suffix: y.str(&p("suffix"), ""),
                name_color: y.opt_str(&p("name-color")),
                chat_format: y.opt_str(&p("chat-format")).filter(|s| !s.is_empty()),
                money_multiplier: y.f64(&p("money-multiplier"), 1.0),
                permissions: y.list(&p("permissions")),
                inherits: y.list(&p("inherits")).into_iter().map(|s| s.to_lowercase()).collect(),
            });
        }
        let default_role = y.str("default-role", "member").to_lowercase();
        if !list.contains_key(&default_role) {
            list.insert(default_role.clone(), Role { id: default_role.clone(), display: default_role.clone(), money_multiplier: 1.0, ..Default::default() });
        }
        Roles { list, default_role, applied: HashMap::new() }
    }

    pub fn save(&self, folder: &str) {
        let mut y = Yaml::empty(&format!("{folder}/roles.yml"));
        y.set_str("default-role", &self.default_role);
        for r in self.list.values() {
            let p = |k: &str| format!("roles.{}.{k}", r.id);
            y.set_str(&p("display"), &r.display);
            y.set_i64(&p("priority"), r.priority);
            y.set_str(&p("prefix"), &r.prefix);
            y.set_str(&p("suffix"), &r.suffix);
            if let Some(c) = &r.name_color { y.set_str(&p("name-color"), c); }
            if let Some(c) = &r.chat_format { y.set_str(&p("chat-format"), c); }
            y.set_f64(&p("money-multiplier"), r.money_multiplier);
            y.set_list(&p("inherits"), &r.inherits);
            y.set_list(&p("permissions"), &r.permissions);
        }
        y.save();
    }

    pub fn role_of(&self, id: Option<&str>) -> Option<&Role> {
        id.and_then(|i| self.list.get(&i.to_lowercase())).or_else(|| self.list.get(&self.default_role))
    }

    pub fn sorted(&self) -> Vec<Role> {
        let mut v: Vec<Role> = self.list.values().cloned().collect();
        v.sort_by(|a, b| b.priority.cmp(&a.priority));
        v
    }

    /// Parents first so a child role can override what it inherits.
    pub fn resolve(&self, role: &Role) -> Vec<(String, bool)> {
        fn collect(roles: &Roles, r: &Role, out: &mut Vec<(String, bool)>, seen: &mut HashSet<String>) {
            if !seen.insert(r.id.clone()) {
                return;
            }
            for parent in &r.inherits {
                if let Some(p) = roles.list.get(parent) {
                    collect(roles, p, out, seen);
                }
            }
            for perm in &r.permissions {
                let value = !perm.starts_with('-');
                let node = perm.trim_start_matches('-').to_string();
                if node == "*" || node.ends_with(".*") {
                    let start = node.trim_end_matches('*');
                    for known in KNOWN_PERMISSIONS {
                        if known.starts_with(start) {
                            out.push((known.to_string(), value));
                        }
                    }
                }
                out.push((node, value));
            }
        }
        let mut out = Vec::new();
        collect(self, role, &mut out, &mut HashSet::new());
        out
    }
}

impl State {
    pub fn player_role(&self, uuid: &str) -> Option<Role> {
        self.roles.role_of(self.players.get(uuid).and_then(|d| d.role.as_deref())).cloned()
    }
}

/// Gives the player the permissions of their role and their tab name.
pub fn apply(p: &Player) {
    let id = uuid_of(p);
    let enabled = core::module("roles");
    let (old, nodes, tab) = with(|s| {
        let old = s.roles.applied.remove(&id).unwrap_or_default();
        if !enabled {
            return (old, Vec::new(), None);
        }
        let role = s.player_role(&id);
        let nodes = role.as_ref().map(|r| s.roles.resolve(r)).unwrap_or_default();
        s.roles.applied.insert(id.clone(), nodes.iter().map(|(n, _)| n.clone()).collect());
        let tab = if s.config.bool("roles.tab-names", true) {
            Some(s.config.str("roles.tab-format", "{role_prefix}{player}{role_suffix}"))
        } else {
            None
        };
        (old, nodes, tab)
    });
    for n in old {
        p.unset_permission(&n);
    }
    for (n, v) in &nodes {
        p.set_permission(n, *v);
    }
    match tab {
        Some(fmt) => {
            let snap = placeholders::snapshot(p);
            p.set_tab_list_name(Some(text::parse(&placeholders::apply(&snap, &fmt))));
        }
        None => p.set_tab_list_name(None),
    }
    let prio = with(|s| s.player_role(&id).map(|r| r.priority).unwrap_or(0));
    p.set_tab_list_order(prio.clamp(-10_000, 10_000) as i32);
}

pub fn refresh_all() {
    for p in core::online_players() {
        apply(&p);
    }
    crate::sidebar::refresh_teams();
}

/// Formats a chat message. Returns the line to send to everyone.
pub fn chat_line(p: &Player) -> String {
    let id = uuid_of(p);
    let fmt = with(|s| {
        s.player_role(&id)
            .and_then(|r| r.chat_format)
            .unwrap_or_else(|| s.config.str("chat.format", "{role_prefix}{player}{role_suffix} <dark_gray>»</dark_gray> <message>"))
    });
    let snap = placeholders::snapshot(p);
    placeholders::apply(&snap, &fmt)
}

pub fn role_cmd(sender: &dyn Tell, args: &[String]) {
    if !core::module("roles") {
        return send(sender, "general.module-disabled", &[("module", "roles")]);
    }
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    if sub.is_empty() || sub == "who" {
        let name = args.get(1).cloned().or_else(|| sender.player().map(|p| p.get_name())).unwrap_or_default();
        let Some(id) = core::lookup(&name) else { return send(sender, "general.unknown-player", &[("player", &name)]) };
        let (n, r) = with(|s| (s.players[&id].name.clone(), s.player_role(&id).map(|r| r.display).unwrap_or_default()));
        return send(sender, "roles.who", &[("player", &n), ("role", &r)]);
    }
    if sub == "list" {
        for r in with(|s| s.roles.sorted()) {
            send(sender, "roles.list-line", &[("role", &r.id), ("display", &r.display), ("priority", &r.priority.to_string()), ("prefix", &r.prefix)]);
        }
        return;
    }
    if !sender.has("keystone.role.admin") {
        return send(sender, "general.no-permission", &[]);
    }
    if args.len() < 2 {
        return send(sender, "roles.usage", &[]);
    }
    let target = args[1].to_lowercase();
    let rest = args[2..].join(" ");
    if sub == "create" {
        let exists = with(|s| s.roles.list.contains_key(&target));
        if exists {
            return send(sender, "roles.exists", &[("role", &target)]);
        }
        let prio = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0);
        with(|s| {
            s.roles.list.insert(target.clone(), Role { id: target.clone(), display: target.clone(), priority: prio, money_multiplier: 1.0, ..Default::default() });
            s.roles.save(&s.folder);
        });
        return send(sender, "roles.created", &[("role", &target)]);
    }
    if sub == "set" {
        let Some(role) = args.get(2).map(|r| r.to_lowercase()) else { return send(sender, "roles.usage", &[]) };
        let Some(id) = core::lookup(&args[1]) else { return send(sender, "general.unknown-player", &[("player", &args[1])]) };
        let Some(display) = with(|s| s.roles.list.get(&role).map(|r| r.display.clone())) else {
            return send(sender, "roles.unknown", &[("role", &role)]);
        };
        set_role(&id, &role);
        let n = core::name_of(&id);
        return send(sender, "roles.set", &[("player", &n), ("role", &display)]);
    }
    let exists = with(|s| s.roles.list.contains_key(&target));
    if !exists {
        return send(sender, "roles.unknown", &[("role", &target)]);
    }
    if sub == "info" {
        let r = with(|s| s.roles.list[&target].clone());
        return send(sender, "roles.info", &[("role", &r.id), ("display", &r.display), ("priority", &r.priority.to_string()),
            ("prefix", &r.prefix), ("suffix", &r.suffix), ("inherits", &r.inherits.join(", ")),
            ("multiplier", &r.money_multiplier.to_string()), ("permissions", &r.permissions.join(", "))]);
    }
    if sub == "delete" {
        let is_default = with(|s| s.roles.default_role == target);
        if is_default {
            return send(sender, "roles.cannot-delete-default", &[]);
        }
        delete_role(&target);
        return send(sender, "roles.deleted", &[("role", &target)]);
    }
    let ok = with(|s| {
        let parent_ok = s.roles.list.contains_key(&rest.to_lowercase());
        let r = s.roles.list.get_mut(&target).unwrap();
        match sub.as_str() {
            "prefix" => r.prefix = rest.clone(),
            "suffix" => r.suffix = rest.clone(),
            "display" => r.display = if rest.is_empty() { target.clone() } else { rest.clone() },
            "chatformat" => r.chat_format = if rest.is_empty() || rest.eq_ignore_ascii_case("none") { None } else { Some(rest.clone()) },
            "color" => r.name_color = if rest.is_empty() || rest.eq_ignore_ascii_case("none") { None } else { Some(rest.to_lowercase()) },
            "priority" => match rest.parse() { Ok(v) => r.priority = v, Err(_) => return false },
            "multiplier" => match rest.parse::<f64>() { Ok(v) => r.money_multiplier = v.max(0.0), Err(_) => return false },
            "addperm" => { if rest.is_empty() { return false; } if !r.permissions.contains(&rest) { r.permissions.push(rest.clone()); } }
            "delperm" => r.permissions.retain(|x| x != &rest),
            "inherit" => { let l = rest.to_lowercase(); if !parent_ok || l == target { return false; } if !r.inherits.contains(&l) { r.inherits.push(l); } }
            "uninherit" => { let l = rest.to_lowercase(); r.inherits.retain(|x| x != &l); }
            _ => return false,
        }
        s.roles.save(&s.folder);
        true
    });
    if !ok {
        return send(sender, "roles.usage", &[]);
    }
    refresh_all();
    send(sender, "roles.updated", &[("role", &target)]);
}

pub fn set_role(uuid: &str, role: &str) {
    with(|s| {
        let def = s.roles.default_role.clone();
        if let Some(d) = s.players.get_mut(uuid) {
            d.role = if role == def { None } else { Some(role.to_string()) };
        }
        s.players_dirty = true;
    });
    if let Some(p) = core::player_by_uuid(uuid) {
        apply(&p);
    }
    crate::sidebar::refresh_teams();
}

pub fn delete_role(id: &str) {
    with(|s| {
        s.roles.list.remove(id);
        for r in s.roles.list.values_mut() {
            r.inherits.retain(|x| x != id);
        }
        for d in s.players.values_mut() {
            if d.role.as_deref() == Some(id) {
                d.role = None;
            }
        }
        s.players_dirty = true;
        s.roles.save(&s.folder);
    });
    refresh_all();
}
