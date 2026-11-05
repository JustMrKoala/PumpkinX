//! Balances, money formatting and the /balance, /pay, /baltop and /eco commands.

use crate::core::{self, send, with, State, Tell};
use crate::data::{self, PlayerData};

impl State {
    fn decimals(&self) -> usize {
        self.config.i64("economy.decimals", 2).clamp(0, 4) as usize
    }

    pub fn round(&self, v: f64) -> f64 {
        let f = 10f64.powi(self.decimals() as i32);
        (v * f).round() / f
    }

    fn number(&self, v: f64) -> String {
        let s = format!("{:.*}", self.decimals(), v.abs());
        let (int, frac) = s.split_once('.').map(|(a, b)| (a.to_string(), Some(b.to_string()))).unwrap_or((s.clone(), None));
        let mut grouped = String::new();
        for (i, ch) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(ch);
        }
        let sign = if v < 0.0 { "-" } else { "" };
        match frac {
            Some(f) => format!("{sign}{grouped}.{f}"),
            None => format!("{sign}{grouped}"),
        }
    }

    pub fn format(&self, v: f64) -> String {
        let c = &self.config;
        c.str("economy.format", "{symbol}{amount}")
            .replace("{symbol}", &c.str("economy.symbol", "$"))
            .replace("{amount}", &self.number(v))
            .replace("{currency}", &if v == 1.0 { c.str("economy.currency-singular", "Coin") } else { c.str("economy.currency-plural", "Coins") })
    }

    pub fn short_format(&self, v: f64) -> String {
        let units = ["", "K", "M", "B", "T"];
        let mut i = 0;
        let mut x = v;
        while x.abs() >= 1000.0 && i < units.len() - 1 {
            x /= 1000.0;
            i += 1;
        }
        let n = if i == 0 { self.number(x) } else { format!("{x:.1}") };
        self.config
            .str("economy.format", "{symbol}{amount}")
            .replace("{symbol}", &self.config.str("economy.symbol", "$"))
            .replace("{amount}", &format!("{n}{}", units[i]))
            .replace("{currency}", &self.config.str("economy.currency-plural", "Coins"))
    }

    pub fn balance(&self, uuid: &str) -> f64 {
        self.players.get(uuid).map(|d| d.balance).unwrap_or(0.0)
    }

    fn max_balance(&self) -> f64 {
        self.config.f64("economy.max-balance", 1e12)
    }

    fn account(&mut self, uuid: &str) -> &mut PlayerData {
        let start = self.config.f64("economy.starting-balance", 0.0);
        self.players.entry(uuid.to_string()).or_insert_with(|| PlayerData::new(uuid, start))
    }

    pub fn withdraw(&mut self, uuid: &str, amount: f64) -> bool {
        if amount < 0.0 || amount.is_nan() {
            return false;
        }
        let r = self.round_fn();
        let d = self.account(uuid);
        if d.balance < amount {
            return false;
        }
        d.balance = r(d.balance - amount);
        self.players_dirty = true;
        true
    }

    pub fn deposit(&mut self, uuid: &str, amount: f64) -> bool {
        if amount < 0.0 || amount.is_nan() {
            return false;
        }
        let max = self.max_balance();
        let r = self.round_fn();
        let d = self.account(uuid);
        if d.balance + amount > max {
            return false;
        }
        d.balance = r(d.balance + amount);
        self.players_dirty = true;
        true
    }

    pub fn set_balance(&mut self, uuid: &str, amount: f64) {
        let max = self.max_balance();
        let r = self.round_fn();
        self.account(uuid).balance = r(amount.clamp(0.0, max));
        self.players_dirty = true;
    }

    /// Moves money in one step so a pay can never create or lose money halfway.
    pub fn transfer(&mut self, from: &str, to: &str, amount: f64) -> bool {
        let max = self.max_balance();
        if amount <= 0.0 || self.balance(from) < amount || self.balance(to) + amount > max {
            return false;
        }
        self.withdraw(from, amount) && self.deposit(to, amount)
    }

    fn round_fn(&self) -> impl Fn(f64) -> f64 + use<> {
        let f = 10f64.powi(self.decimals() as i32);
        move |v| (v * f).round() / f
    }

    /// Parses "250", "1.5k" or "2m". None for anything invalid.
    pub fn parse_amount(&self, s: &str) -> Option<f64> {
        let t = s.trim().to_lowercase().replace(',', "");
        let (num, mult) = match t.chars().last()? {
            'k' => (&t[..t.len() - 1], 1e3),
            'm' => (&t[..t.len() - 1], 1e6),
            'b' => (&t[..t.len() - 1], 1e9),
            _ => (&t[..], 1.0),
        };
        let v = num.parse::<f64>().ok()? * mult;
        if v.is_finite() && v >= 0.0 { Some(self.round(v)) } else { None }
    }
}

pub fn format(v: f64) -> String {
    with(|s| s.format(v))
}

pub fn balance_cmd(sender: &dyn Tell, args: &[String]) {
    let Some(me) = sender.player() else {
        if args.is_empty() {
            send(sender, "general.players-only", &[]);
            return;
        }
        return other_balance(sender, &args[0]);
    };
    if args.is_empty() {
        let id = core::uuid_of(&me);
        let b = with(|s| s.format(s.balance(&id)));
        send(sender, "economy.balance", &[("balance", &b)]);
    } else {
        other_balance(sender, &args[0]);
    }
}

fn other_balance(sender: &dyn Tell, name: &str) {
    if !sender.has("keystone.balance.others") {
        return send(sender, "general.no-permission", &[]);
    }
    match core::lookup(name) {
        Some(id) => {
            let (n, b) = with(|s| (s.players[&id].name.clone(), s.format(s.balance(&id))));
            send(sender, "economy.balance-other", &[("player", &n), ("balance", &b)]);
        }
        None => send(sender, "general.unknown-player", &[("player", name)]),
    }
}

pub fn pay_cmd(sender: &dyn Tell, args: &[String]) {
    let Some(me) = sender.player() else { return send(sender, "general.players-only", &[]) };
    if args.len() < 2 {
        return send(sender, "economy.pay-usage", &[]);
    }
    let Some(target) = core::lookup(&args[0]) else {
        return send(sender, "general.unknown-player", &[("player", &args[0])]);
    };
    let my = core::uuid_of(&me);
    if target == my {
        return send(sender, "economy.pay-self", &[]);
    }
    let result = with(|s| {
        let min = s.config.f64("economy.min-pay", 0.01);
        match s.parse_amount(&args[1]) {
            Some(a) if a >= min => {
                if s.transfer(&my, &target, a) {
                    Ok((s.format(a), s.players[&target].name.clone()))
                } else {
                    Err(("economy.not-enough", s.format(s.balance(&my))))
                }
            }
            _ => Err(("economy.invalid-amount", s.format(min))),
        }
    });
    match result {
        Ok((amount, name)) => {
            send(sender, "economy.paid", &[("player", &name), ("amount", &amount)]);
            if let Some(p) = core::player_by_uuid(&target) {
                send(&p, "economy.received", &[("player", &me.get_name()), ("amount", &amount)]);
            }
        }
        Err(("economy.not-enough", b)) => send(sender, "economy.not-enough", &[("balance", &b)]),
        Err((_, min)) => send(sender, "economy.invalid-amount", &[("min", &min)]),
    }
}

pub fn baltop_cmd(sender: &dyn Tell, args: &[String]) {
    let lines = with(|s| {
        let size = s.config.i64("economy.baltop-page-size", 10).max(1) as usize;
        let mut top: Vec<(&String, &PlayerData)> = s.players.iter().collect();
        top.sort_by(|a, b| b.1.balance.partial_cmp(&a.1.balance).unwrap_or(std::cmp::Ordering::Equal));
        let pages = top.len().div_ceil(size).max(1);
        let page = args.first().and_then(|a| a.parse::<usize>().ok()).unwrap_or(1).clamp(1, pages);
        let mut out = vec![s.prefix() + &s.msg("economy.baltop-header", &[("page", &page.to_string()), ("pages", &pages.to_string())])];
        for (i, (_, d)) in top.iter().enumerate().skip((page - 1) * size).take(size) {
            out.push(s.prefix() + &s.msg("economy.baltop-line", &[("rank", &(i + 1).to_string()), ("player", &d.name), ("balance", &s.format(d.balance))]));
        }
        out
    });
    for l in lines {
        sender.tell_component(crate::text::parse(&l));
    }
}

pub fn eco_cmd(sender: &dyn Tell, args: &[String]) {
    if !sender.has("keystone.eco") {
        return send(sender, "general.no-permission", &[]);
    }
    if args.len() < 2 {
        return send(sender, "economy.eco-usage", &[]);
    }
    let action = args[0].to_lowercase();
    let amount = if action == "reset" { Some(0.0) } else { args.get(2).and_then(|a| with(|s| s.parse_amount(a))) };
    let Some(amount) = amount else { return send(sender, "economy.eco-usage", &[]) };
    let mut targets = Vec::new();
    if args[1] == "*" {
        for p in core::online_players() {
            let id = core::uuid_of(&p);
            let n = p.get_name();
            with(|s| data::ensure(s, &id, &n));
            targets.push(id);
        }
    } else {
        match core::lookup(&args[1]) {
            Some(id) => targets.push(id),
            None => return send(sender, "general.unknown-player", &[("player", &args[1])]),
        }
    }
    for id in targets {
        let done = with(|s| {
            let start = s.config.f64("economy.starting-balance", 0.0);
            match action.as_str() {
                "give" => { s.deposit(&id, amount); }
                "take" => { let b = s.balance(&id) - amount; s.set_balance(&id, b); }
                "set" => s.set_balance(&id, amount),
                "reset" => s.set_balance(&id, start),
                _ => return None,
            }
            Some((s.players[&id].name.clone(), s.format(s.balance(&id))))
        });
        match done {
            Some((n, b)) => send(sender, "economy.eco-done", &[("player", &n), ("balance", &b)]),
            None => return send(sender, "economy.eco-usage", &[]),
        }
    }
}
