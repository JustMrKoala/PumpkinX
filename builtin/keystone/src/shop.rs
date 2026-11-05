//! The shop (shop.yml), its menus, /shop editing commands, /sell and the drop in sell menu.

use crate::core::{self, send, uuid_of, with, State, Tell};
use crate::items;
use crate::menu::{self, act, Kind, Page};
use crate::text;
use crate::yaml::Yaml;
use pumpkin_plugin_api::{Inventory, ItemStack, Player};
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone)]
pub struct ShopItem {
    pub id: String,
    pub category: String,
    pub material: String,
    pub name: Option<String>,
    pub lore: Vec<String>,
    /// A full item saved by /shop add on Pumpkin (enchantments, names and so on).
    pub data: Option<String>,
    pub amount: u32,
    pub buy: f64,
    pub sell: f64,
    pub give_item: bool,
    pub permission: String,
    pub commands: Vec<String>,
}

impl ShopItem {
    pub fn can_buy(&self) -> bool {
        self.buy >= 0.0
    }
    pub fn can_sell(&self) -> bool {
        self.sell >= 0.0 && self.give_item
    }
    /// The item as given to players, one of it.
    pub fn template(&self) -> ItemStack {
        let it = self.data.as_deref().and_then(items::deserialize).unwrap_or_else(|| items::make(&self.material, 1));
        it.set_count(1);
        items::set_name_lore(&it, self.name.as_deref(), &self.lore);
        it
    }
}

#[derive(Clone)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub slot: u32,
    pub items: Vec<ShopItem>,
}

pub struct Shop {
    pub yaml: Yaml,
    pub categories: Vec<Category>,
}

impl Shop {
    pub fn load(folder: &str) -> Shop {
        let yaml = Yaml::load(&format!("{folder}/shop.yml"), Some(core::DEFAULT_SHOP));
        let mut categories = Vec::new();
        let mut auto = 10;
        for cid in yaml.keys("categories") {
            let base = format!("categories.{cid}");
            let lc = cid.to_lowercase();
            let mut cat = Category {
                id: lc.clone(),
                name: yaml.str(&format!("{base}.name"), &format!("<white>{cid}")),
                icon: yaml.str(&format!("{base}.icon"), "chest"),
                slot: yaml.i64(&format!("{base}.slot"), auto) as u32,
                items: Vec::new(),
            };
            auto += 1;
            for iid in yaml.keys(&format!("{base}.items")) {
                let p = |k: &str| format!("{base}.items.{iid}.{k}");
                let commands = yaml.list(&p("commands"));
                let data = yaml.opt_str(&p("pumpkin-item"));
                let material = yaml.str(&p("material"), "");
                if material.is_empty() && data.is_none() {
                    core::warn(&format!("Shop item {lc}.{iid} needs a material (items added on a Java server with item-data only work there)."));
                    continue;
                }
                cat.items.push(ShopItem {
                    id: iid.clone(),
                    category: lc.clone(),
                    material,
                    name: yaml.opt_str(&p("name")),
                    lore: yaml.list(&p("lore")),
                    data,
                    amount: yaml.i64(&p("amount"), 1).max(1) as u32,
                    buy: yaml.f64(&p("buy"), -1.0),
                    sell: yaml.f64(&p("sell"), -1.0),
                    give_item: yaml.bool(&p("give-item"), commands.is_empty()),
                    permission: yaml.str(&p("permission"), ""),
                    commands,
                });
            }
            categories.push(cat);
        }
        Shop { yaml, categories }
    }

    pub fn category(&self, id: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id.to_lowercase())
    }

    pub fn item(&self, cat: &str, id: &str) -> Option<&ShopItem> {
        self.category(cat)?.items.iter().find(|i| i.id.eq_ignore_ascii_case(id))
    }
}

impl State {
    /// Writes shop.yml and reloads the shop from it.
    pub fn shop_set(&mut self, path: &str, value: serde_yaml_ng::Value) {
        self.shop.yaml.set(path, value);
        self.shop.yaml.save();
        self.shop = Shop::load(&self.folder);
    }
}

pub fn item_path(i: &ShopItem) -> String {
    format!("categories.{}.items.{}", i.category, i.id)
}

/// The shop item that buys this item, if any.
pub fn sellable_for(stack: &ItemStack) -> Option<ShopItem> {
    let all: Vec<ShopItem> = with(|s| s.shop.categories.iter().flat_map(|c| c.items.iter().filter(|i| i.can_sell()).cloned()).collect());
    all.into_iter().find(|i| items::similar(&i.template(), stack))
}

pub fn unit_price(stack: &ItemStack) -> f64 {
    sellable_for(stack).map(|i| i.sell / i.amount as f64).unwrap_or(0.0)
}

// ---- buying and selling ----

pub fn buy(p: &Player, item: &ShopItem, bulk: bool) -> bool {
    if !item.can_buy() {
        send(p, "shop.not-buyable", &[]);
        return false;
    }
    if !item.permission.is_empty() && !p.has_permission(&item.permission) {
        send(p, "general.no-permission", &[]);
        return false;
    }
    let template = item.template();
    let mut times = 1u32;
    if bulk && item.give_item {
        times = (template.get_max_count() as u32 / item.amount).max(1);
    }
    let id = uuid_of(p);
    let paid = with(|s| {
        let price = s.round(item.buy * times as f64);
        if s.withdraw(&id, price) { Ok(s.format(price)) } else { Err(s.format(s.balance(&id))) }
    });
    let price = match paid {
        Ok(p) => p,
        Err(b) => {
            send(p, "economy.not-enough", &[("balance", &b)]);
            return false;
        }
    };
    if item.give_item {
        let left = items::give(p, &template, item.amount * times);
        if left > 0 {
            // No room: refund what did not fit instead of dropping it.
            let refund = with(|s| {
                let r = s.round(item.buy * times as f64 * left as f64 / (item.amount * times) as f64);
                s.deposit(&id, r);
                s.format(r)
            });
            core::say(p, &format!("<yellow>Your inventory was full, {refund} was refunded."));
        }
    }
    let name = p.get_name();
    for cmd in &item.commands {
        let c = cmd.replace("{player}", &name).replace("{uuid}", &id);
        let c = c.trim_start_matches('/').to_string();
        core::server(|s| s.execute_command(&c, pumpkin_plugin_api::server::CommandSender::Console));
    }
    send(p, "shop.bought", &[("item", &items::display_name(&template)), ("amount", &(item.amount * times).to_string()), ("price", &price)]);
    true
}

/// Sells one batch, or all batches when `all`. Returns the money earned.
pub fn sell(p: &Player, item: &ShopItem, all: bool) -> f64 {
    if !item.can_sell() {
        return 0.0;
    }
    let template = item.template();
    let have = items::count(p, &template);
    let mut batches = have / item.amount;
    if batches == 0 {
        return 0.0;
    }
    if !all {
        batches = 1;
    }
    let id = uuid_of(p);
    let earned = with(|s| {
        let e = s.round(item.sell * batches as f64);
        if s.deposit(&id, e) { e } else { 0.0 }
    });
    if earned > 0.0 {
        items::remove(p, &template, batches * item.amount);
    }
    earned
}

// ---- menus ----

pub fn open(p: &Player, category: Option<String>) {
    let single = with(|s| if s.shop.categories.len() == 1 { Some(s.shop.categories[0].id.clone()) } else { None });
    match category.or(single) {
        Some(c) => menu::open(p, category_menu(c)),
        None => menu::open(p, main_menu()),
    }
}

fn main_menu() -> menu::Builder {
    Rc::new(|_p: &Player| {
        let (title, rows, cats) = with(|s| (s.shop.yaml.str("title", "Shop"), s.shop.yaml.i64("main-rows", 3).clamp(1, 6) as u32, s.shop.categories.clone()));
        let mut page = Page::new(&title, rows).player_menu();
        for i in 0..rows * 9 {
            page.show(i, items::filler());
        }
        for c in cats {
            let count = c.items.len().to_string();
            let lore = vec![core::msg("shop.category-lore", &[("count", &count)])];
            let id = c.id.clone();
            page.button(c.slot, items::icon(&c.icon, &c.name, &lore), move |pl, _| menu::open(pl, category_menu(id.clone())));
        }
        page
    })
}

fn category_menu(cat: String) -> menu::Builder {
    let current = Rc::new(Cell::new(0usize));
    Rc::new(move |p: &Player| {
        let (title, c, multi) = with(|s| (s.shop.yaml.str("title", "Shop"), s.shop.category(&cat).cloned(), s.shop.categories.len() > 1));
        let Some(c) = c else { return Page::new(&title, 1).player_menu() };
        let mut page = Page::new(&format!("{title} <dark_gray>{}", text::plain(&c.name)), 6).player_menu();
        if multi {
            page = page.parent(Some(main_menu()));
        }
        let entries = c.items.iter().map(|i| {
            let item = i.clone();
            (display(i), act(move |pl, click| {
                if menu::is_right(click) {
                    let earned = sell(pl, &item, menu::is_shift(click));
                    if earned > 0.0 {
                        let e = crate::economy::format(earned);
                        send(pl, "shop.sold", &[("item", &items::display_name(&item.template())), ("price", &e)]);
                    } else {
                        send(pl, if item.can_sell() { "shop.nothing-to-sell" } else { "shop.not-sellable" }, &[]);
                    }
                } else {
                    buy(pl, &item, menu::is_shift(click));
                }
                menu::refresh(pl);
            }))
        }).collect();
        menu::paged(&mut page, entries, current.clone());
        let id = uuid_of(p);
        let bal = with(|s| s.format(s.balance(&id)));
        page.show(49, items::icon("sunflower", &core::msg("shop.button-balance", &[("balance", &bal)]), &[]));
        page
    })
}

fn display(i: &ShopItem) -> ItemStack {
    let it = i.template();
    it.set_count(i.amount.clamp(1, 99) as u8);
    let (buy, sell) = with(|s| (s.format(i.buy), s.format(i.sell)));
    let amount = i.amount.to_string();
    let mut lore = vec![String::new()];
    if i.can_buy() { lore.push(core::msg("shop.lore-buy", &[("price", &buy), ("amount", &amount)])); }
    if i.can_sell() { lore.push(core::msg("shop.lore-sell", &[("price", &sell), ("amount", &amount)])); }
    if i.can_buy() { lore.push(core::msg(if i.give_item { "shop.lore-hint-buy" } else { "shop.lore-hint-buy-once" }, &[])); }
    if i.can_sell() { lore.push(core::msg("shop.lore-hint-sell", &[])); }
    items::add_lore(&it, &lore);
    it
}

// ---- sell menu ----

/// /sell: a chest to drop items in. Closing it sells what the shop buys and gives back the rest.
pub fn sell_menu() -> menu::Builder {
    Rc::new(|_p: &Player| {
        let mut page = Page::new(&core::msg("sellmenu.title", &[]), 6).player_menu();
        page.kind = Kind::Drop;
        for i in 45..54 {
            page.show(i, items::filler());
        }
        page.button(45, items::icon("red_concrete", &core::msg("sellmenu.cancel-button", &[]), &[]), |pl, _| {
            CANCELLED.with(|c| c.set(true));
            menu::close(pl);
        });
        page.button(49, items::icon("lime_concrete", &core::msg("sellmenu.sell-button", &[("price", "...")]), &[]), |pl, _| menu::close(pl));
        page.on_close = Some(Rc::new(settle));
        page.on_render = Some(Rc::new(update_sell_button));
        page
    })
}

thread_local! {
    static CANCELLED: Cell<bool> = const { Cell::new(false) };
}

/// Shows the live total on the sell button.
pub fn update_sell_button(gui: &Inventory) {
    let mut total = 0.0;
    let mut sellable = 0u32;
    let mut unsellable = 0u32;
    for slot in 0..45 {
        if let Some(it) = gui.get_item(slot) {
            let each = unit_price(&it);
            if each > 0.0 { total += each * it.get_count() as f64; sellable += it.get_count() as u32; } else { unsellable += it.get_count() as u32; }
        }
    }
    let price = crate::economy::format(with(|s| s.round(total)));
    let mut lore = vec![core::msg("sellmenu.sell-lore", &[("count", &sellable.to_string())])];
    if unsellable > 0 {
        lore.push(core::msg("sellmenu.unsellable-lore", &[("count", &unsellable.to_string())]));
    }
    gui.set_item(49, Some(items::icon("lime_concrete", &core::msg("sellmenu.sell-button", &[("price", &price)]), &lore)));
}

fn settle(p: &Player, gui: &Inventory) {
    let cancelled = CANCELLED.with(|c| c.replace(false));
    let id = uuid_of(p);
    let mut total = 0.0;
    let mut sold = 0u32;
    let mut back: Vec<ItemStack> = Vec::new();
    let mut sale: Vec<ItemStack> = Vec::new();
    for slot in 0..45 {
        let Some(it) = gui.get_item(slot) else { continue };
        let each = if cancelled { 0.0 } else { unit_price(&it) };
        if each > 0.0 {
            total += each * it.get_count() as f64;
            sold += it.get_count() as u32;
            sale.push(it);
        } else {
            back.push(it);
        }
    }
    for slot in 0..45 {
        gui.set_item(slot, None);
    }
    let paid = with(|s| {
        let t = s.round(total);
        if t <= 0.0 || s.deposit(&id, t) { Some(s.format(t)) } else { None }
    });
    if paid.is_none() {
        send(p, "sellmenu.balance-full", &[]);
        back.extend(sale);
        sold = 0;
    }
    for it in back {
        let n = it.get_count() as u32;
        let left = items::give(p, &it, n);
        if left > 0 {
            drop_at_feet(p, &it, left);
        }
    }
    if sold > 0 {
        send(p, "sellmenu.sold", &[("count", &sold.to_string()), ("price", &paid.unwrap_or_default())]);
    }
}

/// Pumpkin has no drop item call for plugins, so a full inventory spills into the ender chest.
fn drop_at_feet(p: &Player, it: &ItemStack, amount: u32) {
    let ec = p.get_ender_chest();
    let mut left = amount;
    for slot in 0..ec.get_size() {
        if left == 0 { break; }
        if ec.get_item(slot).is_none() {
            let put = left.min(it.get_max_count() as u32);
            ec.set_item(slot, Some(items::copy(it, put as u8)));
            left -= put;
        }
    }
    if left < amount {
        core::say(p, "<yellow>Your inventory was full, so some items went to your ender chest.");
    }
    if left > 0 {
        core::warn(&format!("Could not give {left} {} back to {}.", it.get_registry_key(), p.get_name()));
    }
}

// ---- commands ----

pub fn sell_cmd(sender: &dyn Tell, args: &[String]) {
    if !core::module("shop") {
        return send(sender, "general.module-disabled", &[("module", "shop")]);
    }
    let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
    if args.is_empty() {
        return menu::open(&p, sell_menu());
    }
    let mut total = 0.0;
    if args[0].eq_ignore_ascii_case("all") {
        let all: Vec<ShopItem> = with(|s| s.shop.categories.iter().flat_map(|c| c.items.iter().filter(|i| i.can_sell()).cloned()).collect());
        for i in all {
            total += sell(&p, &i, true);
        }
    } else if let Some(hand) = p.get_item_in_hand(pumpkin_plugin_api::common::Hand::Right) {
        if let Some(i) = sellable_for(&hand) {
            total = sell(&p, &i, true);
        }
    }
    if total <= 0.0 {
        send(&p, "shop.nothing-to-sell", &[]);
    } else {
        send(&p, "shop.sold-total", &[("price", &crate::economy::format(total))]);
    }
}

pub fn shop_cmd(sender: &dyn Tell, args: &[String]) {
    if !core::module("shop") {
        return send(sender, "general.module-disabled", &[("module", "shop")]);
    }
    const ADMIN: [&str; 9] = ["add", "remove", "price", "command", "giveitem", "permission", "category", "delcategory", "list"];
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    if ADMIN.contains(&sub.as_str()) {
        if !sender.has("keystone.shop.admin") {
            return send(sender, "general.no-permission", &[]);
        }
        return admin(sender, &sub, args);
    }
    let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
    if with(|s| s.shop.categories.is_empty()) {
        return send(&p, "shop.empty", &[]);
    }
    if !sub.is_empty() && with(|s| s.shop.category(&sub).is_none()) {
        return send(&p, "shop.unknown-category", &[("category", &sub)]);
    }
    open(&p, if sub.is_empty() { None } else { Some(sub) });
}

fn price(arg: &str) -> Option<f64> {
    if arg == "-1" || arg.eq_ignore_ascii_case("off") { Some(-1.0) } else { with(|s| s.parse_amount(arg)) }
}

/// Adds the item in a player's hand to a category. Returns the new id.
pub fn add_hand_item(p: &Player, category: &str, buy: f64, sell: f64) -> Option<String> {
    let hand = p.get_item_in_hand(pumpkin_plugin_api::common::Hand::Right)?;
    let plain = items::is_plain(&hand);
    let material = items::material_of(&hand.get_registry_key());
    let count = hand.get_count() as i64;
    let data = if plain { None } else { Some(items::serialize(&items::copy(&hand, 1))) };
    let cid = category.to_lowercase();
    Some(with(|s| {
        if s.shop.category(&cid).is_none() {
            s.shop.yaml.set_str(&format!("categories.{cid}.icon"), &material);
            s.shop.yaml.set_str(&format!("categories.{cid}.name"), &format!("<white>{category}"));
        }
        let base = material.to_lowercase();
        let mut id = base.clone();
        let mut n = 2;
        while s.shop.yaml.is_set(&format!("categories.{cid}.items.{id}")) {
            id = format!("{base}_{n}");
            n += 1;
        }
        let p = format!("categories.{cid}.items.{id}");
        s.shop.yaml.set_str(&format!("{p}.material"), &material);
        if let Some(d) = data { s.shop.yaml.set_str(&format!("{p}.pumpkin-item"), &d); }
        s.shop.yaml.set_i64(&format!("{p}.amount"), count);
        s.shop.yaml.set_f64(&format!("{p}.buy"), buy);
        s.shop_set(&format!("{p}.sell"), serde_yaml_ng::Value::from(sell));
        id
    }))
}

fn admin(sender: &dyn Tell, sub: &str, args: &[String]) {
    match sub {
        "list" => {
            let cats = with(|s| s.shop.categories.clone());
            for c in cats {
                let ids: Vec<String> = c.items.iter().map(|i| i.id.clone()).collect();
                send(sender, "shop.list-line", &[("category", &c.id), ("items", &ids.join(", "))]);
            }
        }
        "add" => {
            let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
            if args.len() < 3 { return send(sender, "shop.usage-add", &[]); }
            let (Some(b), Some(se)) = (price(&args[2]), args.get(3).map(|a| price(a)).unwrap_or(Some(-1.0))) else { return send(sender, "shop.usage-add", &[]) };
            match add_hand_item(&p, &args[1], b, se) {
                Some(id) => send(sender, "shop.added", &[("id", &id), ("category", &args[1].to_lowercase())]),
                None => send(sender, "shop.hold-item", &[]),
            }
        }
        "category" => {
            if args.len() < 3 { return send(sender, "shop.usage-category", &[]); }
            let cid = args[1].to_lowercase();
            let name = args[3.min(args.len())..].join(" ");
            with(|s| {
                s.shop.yaml.set_str(&format!("categories.{cid}.icon"), &args[2].to_uppercase());
                let n = if name.is_empty() { format!("<white>{cid}") } else { name };
                s.shop_set(&format!("categories.{cid}.name"), serde_yaml_ng::Value::String(n));
            });
            send(sender, "shop.category-saved", &[("category", &cid)]);
        }
        "delcategory" => {
            let cid = args.get(1).map(|a| a.to_lowercase()).unwrap_or_default();
            if with(|s| s.shop.category(&cid).is_none()) { return send(sender, "shop.unknown-category", &[("category", &cid)]); }
            with(|s| s.shop_set(&format!("categories.{cid}"), serde_yaml_ng::Value::Null));
            send(sender, "shop.category-deleted", &[("category", &cid)]);
        }
        _ => {
            if args.len() < 3 { return send(sender, "shop.usage-edit", &[]); }
            let Some(item) = with(|s| s.shop.item(&args[1], &args[2]).cloned()) else {
                return send(sender, "shop.unknown-item", &[("category", &args[1]), ("id", &args[2])]);
            };
            let path = item_path(&item);
            let ok = match sub {
                "remove" => { with(|s| s.shop_set(&path, serde_yaml_ng::Value::Null)); true }
                "price" => match (args.get(3).and_then(|a| price(a)), args.get(4).map(|a| price(a)).unwrap_or(Some(item.sell))) {
                    (Some(b), Some(se)) => {
                        with(|s| { s.shop.yaml.set_f64(&format!("{path}.buy"), b); s.shop_set(&format!("{path}.sell"), serde_yaml_ng::Value::from(se)); });
                        true
                    }
                    _ => false,
                },
                "command" => {
                    let cmd = args[3.min(args.len())..].join(" ");
                    if cmd.is_empty() { false } else {
                        let mut list = item.commands.clone();
                        list.push(cmd);
                        with(|s| {
                            s.shop.yaml.set_bool(&format!("{path}.give-item"), item.give_item);
                            s.shop.yaml.set_list(&format!("{path}.commands"), &list);
                            s.shop_set(&format!("{path}.commands"), serde_yaml_ng::Value::Sequence(list.iter().map(|c| serde_yaml_ng::Value::String(c.clone())).collect()));
                        });
                        true
                    }
                }
                "giveitem" => { let v = args.get(3).map(|a| a.eq_ignore_ascii_case("true")).unwrap_or(false); with(|s| s.shop_set(&format!("{path}.give-item"), serde_yaml_ng::Value::Bool(v))); true }
                "permission" => {
                    let v = args.get(3).filter(|a| !a.eq_ignore_ascii_case("none")).cloned().unwrap_or_default();
                    with(|s| s.shop_set(&format!("{path}.permission"), serde_yaml_ng::Value::String(v)));
                    true
                }
                _ => false,
            };
            if ok {
                send(sender, "shop.item-saved", &[("id", &item.id), ("category", &item.category)]);
            } else {
                send(sender, "shop.usage-edit", &[]);
            }
        }
    }
}
