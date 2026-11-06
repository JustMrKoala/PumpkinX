//! The admin menu, opened with the admin command. Every setting can be changed from here.

use crate::core::{self, say, uuid_of, with};
use crate::items;
use crate::menu::{self, act, current, Action, Builder, Page};
use crate::text;
use pumpkin_plugin_api::Player;
use serde_yaml_ng::Value;
use std::cell::Cell;
use std::rc::Rc;

type Entry = (pumpkin_plugin_api::ItemStack, Option<Action>);
type Entries = Rc<dyn Fn(&Player) -> Vec<Entry>>;

const FRAMED: [u32; 28] = [10, 11, 12, 13, 14, 15, 16, 19, 20, 21, 22, 23, 24, 25, 28, 29, 30, 31, 32, 33, 34, 37, 38, 39, 40, 41, 42, 43];

/// A screen made of entries, with a margin when they fit.
fn entry_menu(title: &str, parent: Option<Builder>, entries: Entries) -> Builder {
    let title = title.to_string();
    Rc::new(move |p: &Player| {
        let mut page = Page::new(&title, 6).parent(parent.clone());
        let list = entries(p);
        let framed = list.len() <= FRAMED.len();
        for (i, (item, action)) in list.into_iter().enumerate().take(45) {
            let slot = if framed { FRAMED[i] } else { i as u32 };
            page.items.push((slot, item, action));
        }
        page
    })
}

fn link(icon: &str, name: &str, lore: &[&str], target: impl Fn(Option<Builder>) -> Builder + 'static) -> Entry {
    let mut l: Vec<String> = lore.iter().map(|s| s.to_string()).collect();
    l.push("<dark_gray>Click to open".into());
    (items::icon(icon, name, &l), act(move |p, _| menu::open(p, target(current(p)))))
}

fn action(icon: &str, name: &str, lore: &[&str], run: impl Fn(&Player) + 'static) -> Entry {
    let l: Vec<String> = lore.iter().map(|s| s.to_string()).collect();
    (items::icon(icon, name, &l), act(move |p, _| {
        run(p);
        if menu::is_open(p) {
            menu::refresh(p);
        }
    }))
}

/// Only runs on a shift click, for things that cannot be undone.
fn danger(icon: &str, name: &str, lore: &[&str], run: impl Fn(&Player) + 'static) -> Entry {
    let mut l: Vec<String> = lore.iter().map(|s| s.to_string()).collect();
    l.push("<red>Shift click to confirm".into());
    (items::icon(icon, name, &l), act(move |p, c| if menu::is_shift(c) { run(p) }))
}

fn info(icon: &str, name: &str, lore: &[String]) -> Entry {
    (items::icon(icon, name, lore), None)
}

#[derive(Clone)]
enum Kind {
    Text,
    Int,
    Decimal,
    Bool,
    Choice(Vec<String>),
}

fn value_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn text_lore(v: &str) -> Vec<String> {
    if v.is_empty() {
        vec!["<gray>Value: <dark_gray>(empty)".into()]
    } else {
        vec![format!("<gray>Shows as: <white>{v}"), format!("<gray>Typed as: <white>{}", text::escape(v))]
    }
}

/// One editable value. `clearable` lets "none" remove it.
fn setting(label: &str, icon: &str, kind: Kind, help: &[&str], clearable: bool,
           get: impl Fn() -> Value + 'static, set: impl Fn(Value) + 'static) -> Entry {
    let v = get();
    let mut lore = match &kind {
        Kind::Text => text_lore(&value_str(&v).unwrap_or_default()),
        Kind::Bool => vec![if matches!(v, Value::Bool(true)) || value_str(&v).as_deref() == Some("true") { "<green>On".into() } else { "<red>Off".into() }],
        _ => vec![format!("<gray>Value: <white>{}", value_str(&v).unwrap_or("(not set)".into()))],
    };
    for h in help {
        lore.push(format!("<dark_gray>{h}"));
    }
    lore.push(match &kind {
        Kind::Bool => "<yellow>Click to switch".into(),
        Kind::Choice(_) => "<yellow>Click for the next option, right click goes back".into(),
        _ => format!("<yellow>Click to change{}", if clearable { ", type none to clear" } else { "" }),
    });
    let item = items::icon(icon, &format!("<gold>{label}"), &lore);
    let label = label.to_string();
    let get = Rc::new(get);
    let set = Rc::new(set);
    (item, act(move |p, click| {
        let v = get();
        match &kind {
            Kind::Bool => {
                let on = matches!(v, Value::Bool(true)) || value_str(&v).as_deref() == Some("true");
                set(Value::Bool(!on));
                menu::refresh(p);
            }
            Kind::Choice(opts) => {
                if opts.is_empty() { return; }
                let cur = value_str(&v).unwrap_or_default();
                let i = opts.iter().position(|o| *o == cur).map(|i| i as i64).unwrap_or(-1);
                let step = if menu::is_right(click) { -1 } else { 1 };
                set(Value::String(opts[(i + step).rem_euclid(opts.len() as i64) as usize].clone()));
                menu::refresh(p);
            }
            _ => {
                let (set, kind) = (set.clone(), kind.clone());
                menu::ask(p, &format!("New value for {label}:"), &value_str(&v).unwrap_or_default(), current(p), move |pl, t| {
                    if clearable && t.eq_ignore_ascii_case("none") {
                        return set(Value::Null);
                    }
                    let parsed = match kind {
                        Kind::Int => t.parse::<i64>().ok().map(Value::from),
                        Kind::Decimal => t.parse::<f64>().ok().map(Value::from),
                        _ => Some(Value::String(t)),
                    };
                    match parsed {
                        Some(v) => set(v),
                        None => say(pl, "<red>That is not a valid number."),
                    }
                });
            }
        }
    }))
}

/// Saves config.yml and reloads everything so the change takes effect straight away.
fn save_config(path: &str, v: Value) {
    with(|s| {
        s.config.set(path, v);
        s.config.save();
    });
    crate::reload_all();
}

fn cfg(path: &'static str, label: &str, icon: &str, kind: Kind, help: &[&str]) -> Entry {
    setting(label, icon, kind, help, false, move || with(|s| s.config.get(path).cloned().unwrap_or(Value::Null)), move |v| save_config(path, v))
}

fn cfg_clear(path: &'static str, label: &str, icon: &str, kind: Kind, help: &[&str]) -> Entry {
    setting(label, icon, kind, help, true, move || with(|s| s.config.get(path).cloned().unwrap_or(Value::Null)), move |v| save_config(path, v))
}

// ---- reusable list screens ----

fn string_list(title: &str, parent: Option<Builder>, get: Rc<dyn Fn() -> Vec<String>>, save: Rc<dyn Fn(Vec<String>)>, max: usize, formatted: bool) -> Builder {
    let title = title.to_string();
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new(&title, 6).parent(parent.clone());
        let list = get();
        let entries = list.iter().enumerate().map(|(i, line)| {
            let mut lore = if formatted { text_lore(line) } else { vec![format!("<white>{}", text::escape(line))] };
            lore.extend(["".into(), "<yellow>Left click <gray>edit".into(), "<yellow>Shift left <gray>move up, <yellow>shift right <gray>move down".into(), "<red>Right click <gray>delete".into()]);
            let shown = if line.is_empty() { "<white>(empty)".to_string() } else if formatted { line.clone() } else { format!("<white>{}", text::escape(line)) };
            let (get, save) = (get.clone(), save.clone());
            (items::icon("paper", &format!("<gray>{}. <reset>{shown}", i + 1), &lore), act(move |p, click| {
                let mut l = get();
                if i >= l.len() { return; }
                if menu::is_shift(click) {
                    let to = if click == pumpkin_plugin_api::common::ClickType::ShiftLeft { i as i64 - 1 } else { i as i64 + 1 };
                    if to < 0 || to as usize >= l.len() { return; }
                    let x = l.remove(i);
                    l.insert(to as usize, x);
                    save(l);
                    menu::refresh(p);
                } else if menu::is_right(click) {
                    l.remove(i);
                    save(l);
                    menu::refresh(p);
                } else {
                    let (get, save) = (get.clone(), save.clone());
                    menu::ask(p, &format!("New text for line {} (type empty for a blank line):", i + 1), &l[i], current(p), move |_pl, t| {
                        let mut l = get();
                        if i < l.len() {
                            l[i] = if t.eq_ignore_ascii_case("empty") { String::new() } else { t };
                            save(l);
                        }
                    });
                }
            }))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        if list.len() < max {
            let (get, save) = (get.clone(), save.clone());
            page.button(49, items::icon("lime_dye", "<green>Add a line", &["<gray>Type empty for a blank line".into()]), move |p, _| {
                let (get, save) = (get.clone(), save.clone());
                menu::ask(p, "Text for the new line:", "", current(p), move |_pl, t| {
                    let mut l = get();
                    l.push(if t.eq_ignore_ascii_case("empty") { String::new() } else { t });
                    save(l);
                });
            });
        }
        page
    })
}

fn choice(title: &str, parent: Option<Builder>, options: Rc<dyn Fn() -> Vec<(String, String)>>, selected: Rc<dyn Fn(&str) -> bool>, pick: Rc<dyn Fn(&Player, &str)>, single: bool) -> Builder {
    let title = title.to_string();
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new(&title, 6).parent(parent.clone());
        let entries = options().into_iter().map(|(id, label)| {
            let on = selected(&id);
            let icon = items::icon(if on { "lime_dye" } else { "gray_dye" }, &format!("{}{label}", if on { "<green>" } else { "<gray>" }),
                &[if on { "<green>Selected".into() } else { "<gray>Not selected".into() }, if single { "<yellow>Click to pick".into() } else { "<yellow>Click to switch".into() }]);
            let (pick, parent) = (pick.clone(), parent.clone());
            (icon, act(move |p, _| {
                pick(p, &id);
                match (&parent, single) {
                    (Some(b), true) => menu::open(p, b.clone()),
                    _ => menu::refresh(p),
                }
            }))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        page
    })
}

// ---- screens ----

pub fn open(p: &Player) {
    menu::open(p, main());
}

fn main() -> Builder {
    entry_menu("<dark_gray>{server_name} admin", None, Rc::new(|_p| vec![
        link("beacon", "<gold>Server name", &["<gray>Now: <white>{server}", "<gray>Used in chat, menus, the sidebar and the admin command"], branding),
        link("lever", "<gold>Modules", &["<gray>Switch whole parts on or off"], modules),
        link("gold_ingot", "<gold>Economy", &["<gray>Currency, balances and rewards"], economy),
        link("chest", "<gold>Shop", &["<gray>Categories, items, prices"], shop_categories),
        link("gold_block", "<gold>Auction house", &["<gray>Duration, tax, fees, limits"], auctions),
        link("name_tag", "<gold>Roles", &["<gray>Prefixes, colors, permissions"], roles),
        link("oak_sign", "<gold>Sidebar", &["<gray>The scoreboard on the right"], boards),
        link("writable_book", "<gold>Chat and tab", &["<gray>Chat format, tab list, name tags"], chat),
        link("iron_door", "<gold>Login", &["<gray>Passwords for offline mode"], login),
        link("player_head", "<gold>Skins", &["<gray>Premium skins on offline mode"], skins),
        link("totem_of_undying", "<gold>Players", &["<gray>Money, roles, accounts, skins"], players),
        link("book", "<gold>Messages", &["<gray>Every text players see"], |p| messages(p, None)),
        action("clock", "<green>Reload files", &["<gray>Use this after editing files by hand"], |p| {
            crate::reload_all();
            say(p, "<green>Reloaded everything from disk.");
        }),
        action("ender_chest", "<green>Save player data", &[], |p| {
            crate::data::save_now();
            say(p, "<green>Saved.");
        }),
    ]))
}

fn branding(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Server name", parent, Rc::new(|_p| vec![
        cfg("branding.name", "Server name", "name_tag", Kind::Text, &["Plain text, used as {server_name}"]),
        cfg_clear("branding.display", "Styled name", "glow_ink_sac", Kind::Text, &["With colors, used as {server} in the chat prefix"]),
        cfg("branding.address", "Server address", "compass", Kind::Text, &["Used as {server_address}, for example on the sidebar"]),
        cfg_clear("branding.command", "Admin command", "command_block", Kind::Text, &["Opens this menu. On Pumpkin a new name works after a restart", "/keystone always works"]),
    ]))
}

fn module(name: &'static str, label: &str, icon: &str, auto: bool, help: &str) -> Entry {
    let opts: Vec<String> = if auto { vec!["auto".into(), "true".into(), "false".into()] } else { vec!["true".into(), "false".into()] };
    setting(label, icon, Kind::Choice(opts), &[help, "auto means on only when online-mode is false"], false,
        move || Value::String(with(|s| s.config.str(&format!("modules.{name}"), if name == "auth" { "false" } else { "true" }))),
        move |v| {
            let s = value_str(&v).unwrap_or_default();
            save_config(&format!("modules.{name}"), if s == "auto" { Value::String(s) } else { Value::Bool(s == "true") });
        })
}

fn modules(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Modules", parent, Rc::new(|_p| vec![
        module("economy", "Economy", "gold_ingot", false, "Money, /pay, /baltop"),
        module("shop", "Shop", "chest", false, "The /shop menu and /sell"),
        module("auctions", "Auction house", "gold_block", false, "Players sell items to each other with /ah"),
        module("rewards", "Rewards", "diamond_pickaxe", false, "Money for mining, mobs and playtime"),
        module("roles", "Roles", "name_tag", false, "Prefixes and permissions"),
        module("chat", "Chat format", "writable_book", false, "Role based chat"),
        module("sidebar", "Sidebar", "oak_sign", false, "Scoreboard on the right"),
        module("auth", "Login", "iron_door", true, "/register and /login"),
        module("skins", "Skins", "player_head", true, "Premium skins on offline mode"),
    ]))
}

fn economy(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Economy", parent, Rc::new(|_p| vec![
        cfg("economy.currency-singular", "Currency name (one)", "gold_nugget", Kind::Text, &[]),
        cfg("economy.currency-plural", "Currency name (many)", "gold_ingot", Kind::Text, &[]),
        cfg("economy.symbol", "Symbol", "sunflower", Kind::Text, &[]),
        cfg("economy.format", "Money format", "paper", Kind::Text, &["Use {symbol}, {amount} and {currency}"]),
        cfg("economy.decimals", "Decimals", "repeater", Kind::Int, &[]),
        cfg("economy.starting-balance", "Starting balance", "emerald", Kind::Decimal, &[]),
        cfg("economy.max-balance", "Maximum balance", "emerald_block", Kind::Decimal, &[]),
        cfg("economy.min-pay", "Smallest /pay", "iron_nugget", Kind::Decimal, &[]),
        cfg("economy.baltop-page-size", "Baltop page size", "ladder", Kind::Int, &[]),
        cfg("rewards.show-actionbar", "Show reward popups", "experience_bottle", Kind::Bool, &[]),
        cfg("rewards.survival-only", "Rewards in survival only", "iron_sword", Kind::Bool, &[]),
        cfg("rewards.playtime.every-minutes", "Playtime reward every (minutes)", "clock", Kind::Int, &["0 turns it off"]),
        cfg("rewards.playtime.amount", "Playtime reward amount", "clock", Kind::Decimal, &[]),
        link("diamond_ore", "<gold>Mining rewards", &["<gray>Money per block broken"], |p| reward_table(p, true)),
        link("zombie_head", "<gold>Mob rewards", &["<gray>Money per mob killed"], |p| reward_table(p, false)),
    ]))
}

fn reward_table(parent: Option<Builder>, blocks: bool) -> Builder {
    let path = if blocks { "rewards.block-break" } else { "rewards.mob-kill" };
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new(if blocks { "<dark_gray>Mining rewards" } else { "<dark_gray>Mob rewards" }, 6).parent(parent.clone());
        let mut keys = with(|s| s.config.keys(path));
        keys.sort();
        let entries = keys.into_iter().map(|k| {
            let amount = with(|s| s.format(s.config.f64(&format!("{path}.{k}"), 0.0)));
            let icon_mat = if blocks { k.to_lowercase() } else { format!("{}_spawn_egg", k.to_lowercase()) };
            let icon = items::icon(&icon_mat, &format!("<white>{k}"), &[format!("<gray>Pays <green>{amount}"), "<yellow>Left click <gray>change".into(), "<red>Right click <gray>remove".into()]);
            (icon, act(move |p, click| {
                let full = format!("{path}.{k}");
                if menu::is_right(click) {
                    save_config(&full, Value::Null);
                    return menu::refresh(p);
                }
                let cur = with(|s| s.config.str(&full, ""));
                menu::ask(p, &format!("Money for {k}:"), &cur, current(p), move |pl, t| match t.parse::<f64>() {
                    Ok(v) => save_config(&full, Value::from(v.max(0.0))),
                    Err(_) => say(pl, "<red>That is not a number."),
                });
            }))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        let lore: Vec<String> = if blocks { vec!["<yellow>Left click <gray>add the block in your hand".into(), "<yellow>Right click <gray>type a block name".into()] } else { vec!["<yellow>Click <gray>type a mob name".into()] };
        page.button(49, items::icon("lime_dye", "<green>Add", &lore), move |p, click| {
            if blocks && !menu::is_right(click) {
                let Some(hand) = p.get_item_in_hand(pumpkin_plugin_api::common::Hand::Right) else { return say(p, "<red>Hold a block first.") };
                let k = items::material_of(&hand.get_registry_key());
                save_config(&format!("{path}.{k}"), Value::from(1.0));
                say(p, &format!("<green>Added {k}. Click it to set the amount."));
                return menu::refresh(p);
            }
            menu::ask(p, if blocks { "Block name, like DIAMOND_ORE:" } else { "Mob name, like ZOMBIE:" }, "", current(p), move |pl, t| {
                let k = t.trim().to_uppercase().replace(' ', "_");
                if k.is_empty() { return; }
                save_config(&format!("{path}.{k}"), Value::from(1.0));
                say(pl, &format!("<green>Added {k}. Click it to set the amount."));
            });
        });
        page
    })
}

fn chat(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Chat and tab", parent, Rc::new(|_p| vec![
        cfg("chat.format", "Chat format", "writable_book", Kind::Text, &["<message> is the message, placeholders like {role_prefix} work"]),
        cfg("roles.tab-names", "Role names in tab", "painting", Kind::Bool, &[]),
        cfg("roles.tab-format", "Tab format", "item_frame", Kind::Text, &[]),
        cfg("roles.nametags", "Prefix above heads", "name_tag", Kind::Bool, &["Also sorts tab by role priority"]),
    ]))
}

fn login(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Login", parent, Rc::new(|_p| vec![
        cfg("auth.timeout-seconds", "Seconds to log in", "clock", Kind::Int, &[]),
        cfg("auth.max-attempts", "Wrong passwords before kick", "tnt", Kind::Int, &[]),
        cfg("auth.min-password-length", "Shortest password", "string", Kind::Int, &[]),
        cfg("auth.max-password-length", "Longest password", "lead", Kind::Int, &[]),
        cfg("auth.session-minutes", "Auto login minutes (same IP)", "compass", Kind::Int, &["0 turns it off"]),
        cfg("auth.max-accounts-per-ip", "Accounts per IP", "oak_door", Kind::Int, &["0 means no limit"]),
        link("command_block", "<gold>Commands allowed before login", &["<gray>Without the slash"], |p| string_list("<dark_gray>Allowed commands", p,
            Rc::new(|| with(|s| s.config.list("auth.allowed-commands"))),
            Rc::new(|l: Vec<String>| save_config("auth.allowed-commands", Value::Sequence(l.into_iter().map(Value::String).collect()))), 50, false)),
    ]))
}

fn skins(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Skins", parent, Rc::new(|_p| vec![
        cfg("skins.cache-hours", "Keep skins for (hours)", "clock", Kind::Int, &["Lower means fresher skins but more Mojang lookups"]),
        cfg("data.autosave-minutes", "Autosave every (minutes)", "ender_chest", Kind::Int, &["Needs a restart to change"]),
    ]))
}

fn auctions(parent: Option<Builder>) -> Builder {
    entry_menu("<dark_gray>Auction house", parent, Rc::new(|_p| vec![
        cfg("auctions.duration-hours", "Listing time (hours)", "clock", Kind::Decimal, &["Unsold items go back to the seller's collection"]),
        cfg("auctions.sales-tax-percent", "Sales tax (%)", "gold_nugget", Kind::Decimal, &["Taken from the money the seller gets"]),
        cfg("auctions.listing-fee-percent", "Listing fee (%)", "paper", Kind::Decimal, &["Paid up front by the seller, not returned"]),
        cfg("auctions.max-listings", "Listings per player", "chest", Kind::Int, &["Give keystone.ah.limit.<number> to a role for more"]),
        cfg("auctions.min-price", "Lowest price", "iron_nugget", Kind::Decimal, &[]),
        cfg("auctions.max-price", "Highest price", "diamond", Kind::Decimal, &[]),
        (items::icon("gold_block", "<gold>Open the auction house", &["<gray>Shift right click a listing there to remove it".into()]),
            act(|p, _| menu::open(p, crate::auction::browse(String::new())))),
    ]))
}

// ---- shop ----

fn shop_value(path: String, def: Value) -> impl Fn() -> Value {
    move || with(|s| s.shop.yaml.get(&path).cloned().unwrap_or(def.clone()))
}

fn shop_set(path: String) -> impl Fn(Value) {
    move |v| with(|s| s.shop_set(&path, v))
}

fn shop_categories(parent: Option<Builder>) -> Builder {
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new("<dark_gray>Shop categories", 6).parent(parent.clone());
        let cats = with(|s| s.shop.categories.clone());
        let entries = cats.into_iter().map(|c| {
            let lore = vec![format!("<gray>id: {}", c.id), format!("<gray>{} items, slot {}", c.items.len(), c.slot), "<yellow>Left click <gray>items".into(), "<yellow>Right click <gray>category settings".into()];
            let id = c.id.clone();
            (items::icon(&c.icon, &c.name, &lore), act(move |p, click| {
                if menu::is_right(click) { menu::open(p, shop_category(current(p), id.clone())) } else { menu::open(p, shop_items(current(p), id.clone())) }
            }))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        page.button(49, items::icon("lime_dye", "<green>New category", &[]), |p, _| {
            menu::ask(p, "Id for the new category (letters, numbers, _):", "", current(p), |pl, id| {
                let clean = id.to_lowercase();
                if !clean.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || clean.is_empty() {
                    return say(pl, "<red>Only letters, numbers and _ please.");
                }
                with(|s| {
                    s.shop.yaml.set_str(&format!("categories.{clean}.icon"), "CHEST");
                    s.shop_set(&format!("categories.{clean}.name"), Value::String(format!("<white>{id}")));
                });
            });
        });
        page.button(47, items::icon("comparator", "<gold>Shop settings", &["<gray>Title, rows, filler".into()]), |p, _| {
            menu::open(p, entry_menu("<dark_gray>Shop settings", current(p), Rc::new(|_p| vec![
                setting("Menu title", "name_tag", Kind::Text, &[], false, shop_value("title".into(), Value::from("Shop")), shop_set("title".into())),
                setting("Rows of the category menu", "ladder", Kind::Int, &["1 to 6"], false, shop_value("main-rows".into(), Value::from(3)), shop_set("main-rows".into())),
            ])));
        });
        page
    })
}

fn shop_category(parent: Option<Builder>, id: String) -> Builder {
    let base = format!("categories.{id}");
    entry_menu(&format!("<dark_gray>Category {id}"), parent.clone(), Rc::new(move |_p| {
        let (b1, b2, b3) = (base.clone(), base.clone(), base.clone());
        let parent = parent.clone();
        vec![
            setting("Name", "name_tag", Kind::Text, &[], false, shop_value(format!("{base}.name"), Value::Null), shop_set(format!("{base}.name"))),
            (items::icon(&with(|s| s.shop.yaml.str(&format!("{b1}.icon"), "chest")), "<gold>Icon", &["<yellow>Left click <gray>use the item in your hand".into(), "<yellow>Right click <gray>type a name".into()]),
                act(move |p, click| {
                    let path = format!("{b1}.icon");
                    if menu::is_right(click) {
                        let path2 = path.clone();
                        menu::ask(p, "Item name, like DIAMOND:", "", current(p), move |_pl, t| with(|s| s.shop_set(&path2, Value::String(t.to_uppercase()))));
                    } else if let Some(h) = p.get_item_in_hand(pumpkin_plugin_api::common::Hand::Right) {
                        with(|s| s.shop_set(&path, Value::String(items::material_of(&h.get_registry_key()))));
                        menu::refresh(p);
                    } else {
                        say(p, "<red>Hold an item first.");
                    }
                })),
            setting("Slot in the category menu", "item_frame", Kind::Int, &["0 is the top left"], false, shop_value(format!("{b2}.slot"), Value::from(10)), shop_set(format!("{b2}.slot"))),
            danger("lava_bucket", "<red>Delete category", &["<gray>Removes it and all its items"], move |p| {
                with(|s| s.shop_set(&b3, Value::Null));
                if let Some(b) = parent.clone() { menu::open(p, b) }
            }),
        ]
    }))
}

fn shop_items(parent: Option<Builder>, cat: String) -> Builder {
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new(&format!("<dark_gray>Items in {cat}"), 6).parent(parent.clone());
        let list = with(|s| s.shop.category(&cat).map(|c| c.items.clone()).unwrap_or_default());
        let entries = list.into_iter().map(|i| {
            let it = i.template();
            it.set_count(i.amount.clamp(1, 99) as u8);
            let (buy, sell) = with(|s| (if i.can_buy() { format!("<green>{}", s.format(i.buy)) } else { "<red>off".into() }, if i.sell >= 0.0 { format!("<gold>{}", s.format(i.sell)) } else { "<red>off".into() }));
            let mut lore = vec![format!("<gray>id: {}", i.id), format!("<gray>Buy: {buy}"), format!("<gray>Sell: {sell}")];
            if !i.commands.is_empty() { lore.push(format!("<gray>Runs {} commands", i.commands.len())); }
            lore.push("<yellow>Click to edit".into());
            items::add_lore(&it, &lore);
            let (c, id) = (cat.clone(), i.id.clone());
            (it, act(move |p, _| menu::open(p, shop_item(current(p), c.clone(), id.clone()))))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        let c = cat.clone();
        page.button(49, items::icon("lime_dye", "<green>Add the item in your hand", &["<gray>Keeps enchantments and names".into(), "<gray>Starts at buy 100, selling off".into()]), move |p, _| {
            match crate::shop::add_hand_item(p, &c, 100.0, -1.0) {
                Some(id) => menu::open(p, shop_item(current(p), c.clone(), id)),
                None => say(p, "<red>Hold an item first."),
            }
        });
        let c = cat.clone();
        page.button(47, items::icon("comparator", "<gold>Category settings", &[]), move |p, _| menu::open(p, shop_category(current(p), c.clone())));
        page
    })
}

fn shop_item(parent: Option<Builder>, cat: String, id: String) -> Builder {
    let base = format!("categories.{cat}.items.{id}");
    entry_menu(&format!("<dark_gray>Item {id}"), parent.clone(), Rc::new(move |_p| {
        let b = base.clone();
        let list_of = |key: &'static str| {
            let path = format!("{b}.{key}");
            let p2 = path.clone();
            (Rc::new(move || with(|s| s.shop.yaml.list(&path))) as Rc<dyn Fn() -> Vec<String>>,
             Rc::new(move |l: Vec<String>| with(|s| s.shop_set(&p2, Value::Sequence(l.into_iter().map(Value::String).collect())))) as Rc<dyn Fn(Vec<String>)>)
        };
        let (lore_get, lore_set) = list_of("lore");
        let (cmd_get, cmd_set) = list_of("commands");
        let (b_replace, b_delete, parent) = (b.clone(), b.clone(), parent.clone());
        let id2 = id.clone();
        vec![
            setting("Buy price", "emerald", Kind::Decimal, &["-1 turns buying off"], false, shop_value(format!("{b}.buy"), Value::from(-1.0)), shop_set(format!("{b}.buy"))),
            setting("Sell price", "gold_ingot", Kind::Decimal, &["-1 turns selling off"], false, shop_value(format!("{b}.sell"), Value::from(-1.0)), shop_set(format!("{b}.sell"))),
            setting("Amount per purchase", "bundle", Kind::Int, &[], false, shop_value(format!("{b}.amount"), Value::from(1)), shop_set(format!("{b}.amount"))),
            setting("Give the item", "chest", Kind::Bool, &["Off means buying only runs the commands"], false, shop_value(format!("{b}.give-item"), Value::Bool(true)), shop_set(format!("{b}.give-item"))),
            setting("Custom name", "name_tag", Kind::Text, &[], true, shop_value(format!("{b}.name"), Value::Null), shop_set(format!("{b}.name"))),
            setting("Permission needed", "tripwire_hook", Kind::Text, &["Type none for everyone"], true, shop_value(format!("{b}.permission"), Value::from("")), shop_set(format!("{b}.permission"))),
            link("writable_book", "<gold>Lore", &[], move |p| string_list(&format!("<dark_gray>Lore of {id2}"), p, lore_get.clone(), lore_set.clone(), 20, true)),
            link("command_block", "<gold>Commands on purchase", &["<gray>Run by the console, {player} is the buyer", "<gray>Example: role set {player} vip"], move |p| string_list("<dark_gray>Commands", p, cmd_get.clone(), cmd_set.clone(), 20, false)),
            action("hopper", "<gold>Replace with the item in your hand", &[], move |p| {
                let Some(h) = p.get_item_in_hand(pumpkin_plugin_api::common::Hand::Right) else { return say(p, "<red>Hold an item first.") };
                let data = if items::is_plain(&h) { Value::Null } else { Value::String(items::serialize(&items::copy(&h, 1))) };
                let mat = items::material_of(&h.get_registry_key());
                with(|s| {
                    s.shop.yaml.set_str(&format!("{b_replace}.material"), &mat);
                    s.shop_set(&format!("{b_replace}.pumpkin-item"), data);
                });
            }),
            danger("lava_bucket", "<red>Delete item", &[], move |p| {
                with(|s| s.shop_set(&b_delete, Value::Null));
                if let Some(b) = parent.clone() { menu::open(p, b) }
            }),
        ]
    }))
}

// ---- roles ----

const COLORS: [&str; 17] = ["none", "black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue", "green", "aqua", "red", "light_purple", "yellow", "white"];

fn roles(parent: Option<Builder>) -> Builder {
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new("<dark_gray>Roles", 6).parent(parent.clone());
        let (list, def) = with(|s| (s.roles.sorted(), s.roles.default_role.clone()));
        let entries = list.into_iter().map(|r| {
            let is_def = r.id == def;
            let members = with(|s| s.players.values().filter(|d| d.role.as_deref() == Some(r.id.as_str())).count());
            let lore = vec![format!("<gray>id: {}{}", r.id, if is_def { " <green>(default)" } else { "" }), format!("<gray>priority {}, {} permissions", r.priority, r.permissions.len()),
                if is_def { "<gray>everyone without a role".into() } else { format!("<gray>{members} players") }, "<yellow>Click to edit".into()];
            let id = r.id.clone();
            (items::icon(if is_def { "name_tag" } else { "paper" }, &format!("{}<white>{}", r.prefix, r.display), &lore), act(move |p, _| menu::open(p, role(current(p), id.clone()))))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        page.button(49, items::icon("lime_dye", "<green>New role", &[]), |p, _| {
            menu::ask(p, "Id for the new role (letters, numbers, _):", "", current(p), |pl, id| {
                let clean = id.to_lowercase();
                if clean.is_empty() || !clean.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return say(pl, "<red>Only letters, numbers and _ please.");
                }
                let exists = with(|s| s.roles.list.contains_key(&clean));
                if exists { return say(pl, "<red>That role already exists."); }
                with(|s| {
                    s.roles.list.insert(clean.clone(), crate::roles::Role { id: clean.clone(), display: clean.clone(), money_multiplier: 1.0, ..Default::default() });
                    s.roles.save(&s.folder);
                });
            });
        });
        page
    })
}

fn role_field(id: &str, label: &str, icon: &str, kind: Kind, help: &[&str], clearable: bool,
              get: impl Fn(&crate::roles::Role) -> Value + 'static, set: impl Fn(&mut crate::roles::Role, Value) + 'static) -> Entry {
    let (a, b) = (id.to_string(), id.to_string());
    setting(label, icon, kind, help, clearable,
        move || with(|s| s.roles.list.get(&a).map(&get).unwrap_or(Value::Null)),
        move |v| {
            with(|s| {
                if let Some(r) = s.roles.list.get_mut(&b) {
                    set(r, v);
                }
                s.roles.save(&s.folder);
            });
            crate::roles::refresh_all();
        })
}

fn role(parent: Option<Builder>, id: String) -> Builder {
    entry_menu(&format!("<dark_gray>Role {id}"), parent.clone(), Rc::new(move |_p| {
        let s = |v: &str| Value::String(v.to_string());
        let txt = |v: Value| value_str(&v).unwrap_or_default();
        let (i1, i2, i3, i4, parent) = (id.clone(), id.clone(), id.clone(), id.clone(), parent.clone());
        vec![
            role_field(&id, "Display name", "name_tag", Kind::Text, &[], false, move |r| s(&r.display), move |r, v| r.display = txt(v)),
            role_field(&id, "Prefix", "oak_sign", Kind::Text, &["End with a space to separate it from the name"], true, move |r| s(&r.prefix), move |r, v| r.prefix = txt(v)),
            role_field(&id, "Suffix", "birch_sign", Kind::Text, &[], true, move |r| s(&r.suffix), move |r, v| r.suffix = txt(v)),
            role_field(&id, "Priority", "ladder", Kind::Int, &["Higher shows first in tab"], false, |r| Value::from(r.priority), |r, v| r.priority = v.as_i64().unwrap_or(r.priority)),
            role_field(&id, "Name color", "red_dye", Kind::Choice(COLORS.iter().map(|c| c.to_string()).collect()), &["Color of the name above the head"], false,
                move |r| s(r.name_color.as_deref().unwrap_or("none")), move |r, v| { let c = txt(v); r.name_color = if c == "none" { None } else { Some(c) } }),
            role_field(&id, "Chat format", "writable_book", Kind::Text, &["Empty uses the normal chat format"], true,
                move |r| r.chat_format.clone().map(Value::String).unwrap_or(Value::Null), move |r, v| { let c = txt(v); r.chat_format = if c.is_empty() { None } else { Some(c) } }),
            role_field(&id, "Money multiplier", "gold_ingot", Kind::Decimal, &["1.5 means 50% more reward money"], false, |r| Value::from(r.money_multiplier), |r, v| r.money_multiplier = v.as_f64().unwrap_or(1.0).max(0.0)),
            link("book", "<gold>Permissions", &["<gray>* is everything, plugin.* a whole plugin", "<gray>-node takes a permission away"], move |p| {
                let (a, b) = (i1.clone(), i1.clone());
                string_list(&format!("<dark_gray>Permissions of {a}"), p,
                    Rc::new(move || with(|s| s.roles.list.get(&a).map(|r| r.permissions.clone()).unwrap_or_default())),
                    Rc::new(move |l: Vec<String>| { with(|s| { if let Some(r) = s.roles.list.get_mut(&b) { r.permissions = l; } s.roles.save(&s.folder); }); crate::roles::refresh_all(); }), 500, false)
            }),
            link("lead", "<gold>Inherits from", &["<gray>Gets every permission of those roles"], move |p| {
                let (a, b, c) = (i2.clone(), i2.clone(), i2.clone());
                choice(&format!("<dark_gray>{a} inherits"), p,
                    Rc::new(move || with(|s| s.roles.sorted().into_iter().filter(|r| r.id != a).map(|r| (r.id.clone(), r.display.clone())).collect())),
                    Rc::new(move |x: &str| with(|s| s.roles.list.get(&b).map(|r| r.inherits.iter().any(|i| i == x)).unwrap_or(false))),
                    Rc::new(move |_p: &Player, x: &str| { with(|s| { if let Some(r) = s.roles.list.get_mut(&c) { if r.inherits.iter().any(|i| i == x) { r.inherits.retain(|i| i != x) } else { r.inherits.push(x.to_string()) } } s.roles.save(&s.folder); }); crate::roles::refresh_all(); }),
                    false)
            }),
            action("emerald", "<gold>Make this the default role", &["<gray>Everyone without a role gets this"], move |_p| {
                with(|s| { s.roles.default_role = i3.clone(); s.roles.save(&s.folder); });
                crate::roles::refresh_all();
            }),
            danger("lava_bucket", "<red>Delete role", &["<gray>Its players go back to the default role"], move |p| {
                let is_def = with(|s| s.roles.default_role == i4);
                if is_def { return say(p, "<red>Pick another default role first."); }
                crate::roles::delete_role(&i4);
                if let Some(b) = parent.clone() { menu::open(p, b) }
            }),
        ]
    }))
}

// ---- sidebar ----

fn boards(parent: Option<Builder>) -> Builder {
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new("<dark_gray>Sidebar boards", 6).parent(parent.clone());
        let list = with(|s| s.sidebar.boards.clone());
        let entries = list.into_iter().map(|b| {
            let mut lore = vec![format!("<gray>Shown to: <white>{}", if b.roles.is_empty() { "everyone".into() } else { b.roles.join(", ") }),
                format!("<gray>Worlds: <white>{}", if b.worlds.is_empty() { "all".into() } else { b.worlds.join(", ") }), String::new()];
            if let Some(t) = b.title.first() { lore.push(t.clone()); }
            for l in &b.lines { lore.push(if l.is_empty() { " ".into() } else { l.clone() }); }
            lore.push(String::new());
            lore.push("<yellow>Click to edit".into());
            let id = b.id.clone();
            (items::icon("oak_sign", &format!("<gold>{}", b.id), &lore), act(move |p, _| menu::open(p, board(current(p), id.clone()))))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        page.button(49, items::icon("lime_dye", "<green>New board", &[]), |p, _| {
            menu::ask(p, "Id for the new board (letters, numbers, _):", "", current(p), |pl, id| {
                let clean = id.to_lowercase();
                if clean.is_empty() || !clean.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { return say(pl, "<red>Only letters, numbers and _ please."); }
                if with(|s| s.sidebar.board(&clean).is_some()) { return say(pl, "<red>That board already exists."); }
                crate::sidebar::create(&clean);
            });
        });
        page.button(47, items::icon("comparator", "<gold>Sidebar speed", &[]), |p, _| {
            menu::open(p, entry_menu("<dark_gray>Sidebar speed", current(p), Rc::new(|_p| vec![
                setting("Update every (ticks)", "clock", Kind::Int, &["20 ticks is one second"], false, || Value::from(with(|s| s.sidebar.update_ticks)),
                    |v| with(|s| { s.sidebar.update_ticks = v.as_u64().unwrap_or(20).max(1); s.sidebar.save(&s.folder); })),
                setting("Next title frame every (ticks)", "repeater", Kind::Int, &[], false, || Value::from(with(|s| s.sidebar.frame_ticks)),
                    |v| with(|s| { s.sidebar.frame_ticks = v.as_u64().unwrap_or(10).max(1); s.sidebar.save(&s.folder); })),
            ])));
        });
        page
    })
}

fn edit_board(id: &str, f: impl FnOnce(&mut crate::sidebar::Board)) {
    with(|s| {
        if let Some(b) = s.sidebar.board_mut(id) {
            f(b);
        }
        s.sidebar.save(&s.folder);
    });
    for p in core::online_players() {
        crate::sidebar::update(&p);
    }
}

fn board(parent: Option<Builder>, id: String) -> Builder {
    entry_menu(&format!("<dark_gray>Board {id}"), parent.clone(), Rc::new(move |_p| {
        let (i1, i2, i3, i4, i5, parent) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), parent.clone());
        vec![
            link("name_tag", "<gold>Title", &["<gray>Add more frames to animate it"], move |p| {
                let (a, b) = (i1.clone(), i1.clone());
                string_list(&format!("<dark_gray>Title frames of {a}"), p, Rc::new(move || with(|s| s.sidebar.board(&a).map(|b| b.title.clone()).unwrap_or_default())),
                    Rc::new(move |l: Vec<String>| edit_board(&b, |bd| bd.title = l)), 30, true)
            }),
            link("paper", "<gold>Lines", &["<gray>Up to 15 lines with placeholders like {balance}"], move |p| {
                let (a, b) = (i2.clone(), i2.clone());
                string_list(&format!("<dark_gray>Lines of {a}"), p, Rc::new(move || with(|s| s.sidebar.board(&a).map(|b| b.lines.clone()).unwrap_or_default())),
                    Rc::new(move |l: Vec<String>| edit_board(&b, |bd| bd.lines = l)), crate::sidebar::MAX_LINES, true)
            }),
            link("player_head", "<gold>Who sees it", &["<gray>Nothing selected means everyone"], move |p| {
                let (a, b) = (i3.clone(), i3.clone());
                choice(&format!("<dark_gray>Roles for {a}"), p,
                    Rc::new(|| with(|s| s.roles.sorted().into_iter().map(|r| (r.id.clone(), r.display.clone())).collect())),
                    Rc::new(move |r: &str| with(|s| s.sidebar.board(&a).map(|b| b.roles.iter().any(|x| x == r)).unwrap_or(false))),
                    Rc::new(move |_p: &Player, r: &str| edit_board(&b, |bd| if bd.roles.iter().any(|x| x == r) { bd.roles.retain(|x| x != r) } else { bd.roles.push(r.to_string()) })),
                    false)
            }),
            link("grass_block", "<gold>Worlds", &["<gray>Nothing selected means every world"], move |p| {
                let (a, b) = (i4.clone(), i4.clone());
                choice(&format!("<dark_gray>Worlds for {a}"), p,
                    Rc::new(|| core::server(|s| s.get_all_worlds()).into_iter().map(|w| { let n = w.get_name(); (n.clone(), n) }).collect()),
                    Rc::new(move |w: &str| with(|s| s.sidebar.board(&a).map(|b| b.worlds.iter().any(|x| x == w)).unwrap_or(false))),
                    Rc::new(move |_p: &Player, w: &str| edit_board(&b, |bd| if bd.worlds.iter().any(|x| x == w) { bd.worlds.retain(|x| x != w) } else { bd.worlds.push(w.to_string()) })),
                    false)
            }),
            info("knowledge_book", "<gold>Placeholders", &["<white>{player} {balance} {balance_short} {role}".into(), "<white>{role_prefix} {online} {max_players}".into(),
                "<white>{ping} {world} {x} {y} {z} {kills} {deaths}".into(), "<white>{tps} {time} {date} {server_name}".into(), "<white>{health} {food} {level}".into()]),
            danger("lava_bucket", "<red>Delete board", &[], move |p| {
                with(|s| { s.sidebar.boards.retain(|b| b.id != i5); s.sidebar.save(&s.folder); });
                if let Some(b) = parent.clone() { menu::open(p, b) }
            }),
        ]
    }))
}

// ---- players ----

fn players(parent: Option<Builder>) -> Builder {
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let mut page = Page::new("<dark_gray>Players", 6).parent(parent.clone());
        let online: Vec<String> = core::online_players().iter().map(uuid_of).collect();
        let mut list = with(|s| s.players.iter().map(|(k, d)| (k.clone(), d.clone())).collect::<Vec<_>>());
        list.sort_by(|a, b| (!online.contains(&a.0)).cmp(&!online.contains(&b.0)).then(a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
        let entries = list.into_iter().map(|(id, d)| {
            let on = online.contains(&id);
            let (role, bal) = with(|s| (s.player_role(&id).map(|r| format!("{}<white>{}", r.prefix, r.display)).unwrap_or_default(), s.format(d.balance)));
            let lore = vec![if on { "<green>Online".into() } else { "<gray>Offline".into() }, format!("<gray>Role: {role}"), format!("<gray>Money: <green>{bal}"),
                format!("<gray>Password: {}", if d.password.is_some() { "<green>set" } else { "<red>none" }), "<yellow>Click to manage".into()];
            (items::icon("player_head", &format!("{}{}", if on { "<green>" } else { "<gray>" }, d.name), &lore), act(move |p, _| menu::open(p, player(current(p), id.clone()))))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        page.button(49, items::icon("spyglass", "<gold>Find a player", &[]), |p, _| {
            let back = current(p);
            menu::ask(p, "Name of the player:", "", None, move |pl, name| match core::lookup(&name) {
                Some(id) => menu::open(pl, player(back.clone(), id)),
                None => {
                    say(pl, &format!("<red>Nobody called {name} has joined."));
                    if let Some(b) = back.clone() { menu::open(pl, b) }
                }
            });
        });
        page
    })
}

fn player(parent: Option<Builder>, id: String) -> Builder {
    let name = core::name_of(&id);
    entry_menu(&format!("<dark_gray>{name}"), parent, Rc::new(move |_p| {
        let Some(d) = with(|s| s.players.get(&id).cloned()) else { return vec![] };
        let online = core::player_by_uuid(&id);
        let mut out = Vec::new();
        let (a, b, c, e, f, g, h, i, j, k) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        out.push(setting("Balance", "gold_ingot", Kind::Decimal, &["Sets the exact amount"], false, move || Value::from(with(|s| s.balance(&a))), move |v| with(|s| s.set_balance(&b, v.as_f64().unwrap_or(0.0)))));
        out.push(action("emerald", "<green>Give money", &[], move |p| {
            let id = c.clone();
            menu::ask(p, "How much to give?", "", current(p), move |pl, t| match with(|s| s.parse_amount(&t)) { Some(v) => { with(|s| s.deposit(&id, v)); } None => say(pl, "<red>Not a valid amount.") });
        }));
        out.push(action("redstone", "<red>Take money", &[], move |p| {
            let id = e.clone();
            menu::ask(p, "How much to take?", "", current(p), move |pl, t| match with(|s| s.parse_amount(&t)) { Some(v) => with(|s| { let b = s.balance(&id) - v; s.set_balance(&id, b) }), None => say(pl, "<red>Not a valid amount.") });
        }));
        let role = with(|s| s.player_role(&id).map(|r| format!("{}<white>{}", r.prefix, r.display)).unwrap_or_default());
        out.push(link("name_tag", &format!("<gold>Role: {role}"), &[], move |p| {
            let (x, y) = (f.clone(), f.clone());
            choice("<dark_gray>Role", p, Rc::new(|| with(|s| s.roles.sorted().into_iter().map(|r| (r.id.clone(), r.display.clone())).collect())),
                Rc::new(move |r: &str| with(|s| s.player_role(&x).map(|ro| ro.id == r).unwrap_or(false))),
                Rc::new(move |_p: &Player, r: &str| crate::roles::set_role(&y, r)), true)
        }));
        out.push(setting("Sidebar hidden", "oak_sign", Kind::Bool, &[], false, move || Value::Bool(with(|s| s.players.get(&g).map(|d| d.sidebar_hidden).unwrap_or(false))),
            move |v| { with(|s| if let Some(d) = s.players.get_mut(&h) { d.sidebar_hidden = v.as_bool().unwrap_or(false) }); if let Some(p) = core::player_by_uuid(&h) { crate::sidebar::update(&p) } }));
        out.push(action("bone", "<gold>Reset kills and deaths", &[&format!("<gray>Now {} kills, {} deaths", d.kills, d.deaths)], move |_p| with(|s| if let Some(d) = s.players.get_mut(&i) { d.kills = 0; d.deaths = 0; })));
        out.push(info("iron_door", "<gold>Account", &[format!("<gray>Password: {}", if d.password.is_some() { "<green>set" } else { "<red>none" }),
            format!("<gray>Last IP: <white>{}", d.last_ip.clone().unwrap_or("?".into()))]));
        if let Some(op) = &online {
            if !crate::auth::is_authenticated(op) {
                let x = id.clone();
                out.push(action("lime_dye", "<green>Log them in", &[], move |_p| if let Some(t) = core::player_by_uuid(&x) { crate::auth::force_login(&t) }));
            }
        }
        out.push(action("tripwire_hook", "<gold>Set a new password", &[], move |p| {
            let id = j.clone();
            menu::ask(p, "New password:", "", current(p), move |pl, pw| { crate::auth::set_password(&id, &pw); say(pl, "<green>Password changed."); });
        }));
        out.push(danger("barrier", "<red>Remove password", &["<gray>They have to /register again"], move |p| { crate::auth::unregister(&k); menu::refresh(p); }));
        let skin = d.skin.clone().unwrap_or("own".into());
        let x = id.clone();
        out.push(action("player_head", &format!("<gold>Skin: <white>{skin}"), &["<yellow>Left click <gray>pick a skin", "<yellow>Right click <gray>back to their own"], move |p| {
            let Some(t) = core::player_by_uuid(&x) else { return say(p, "<red>They need to be online.") };
            let tid = x.clone();
            menu::ask(p, "Premium name whose skin they should wear (or reset):", "", current(p), move |pl, n| {
                let Some(t) = core::player_by_uuid(&tid) else { return };
                let name = if n.eq_ignore_ascii_case("reset") { t.get_name() } else { n };
                crate::skins::change(pl, &t, &name, false);
            });
            let _ = t;
        }));
        if online.is_some() {
            let x = id.clone();
            out.push(action("ender_pearl", "<gold>Teleport to them", &[], move |p| {
                if let Some(t) = core::player_by_uuid(&x) {
                    menu::close(p);
                    p.teleport(t.get_position(), None, None, t.get_world());
                }
            }));
        }
        out
    }))
}

// ---- messages ----

fn messages(parent: Option<Builder>, section: Option<String>) -> Builder {
    let page_no = Rc::new(Cell::new(0usize));
    Rc::new(move |_p: &Player| {
        let title = match &section { Some(s) => format!("<dark_gray>Messages: {s}"), None => "<dark_gray>Messages".into() };
        let mut page = Page::new(&title, 6).parent(parent.clone());
        let full = |k: &str| match &section { Some(s) => format!("{s}.{k}"), None => k.to_string() };
        let keys = with(|s| s.messages.keys(section.as_deref().unwrap_or("")));
        let entries = keys.into_iter().map(|k| {
            let path = full(&k);
            if with(|s| s.messages.is_section(&path)) {
                let count = with(|s| s.messages.keys(&path).len());
                let sec = path.clone();
                return (items::icon("bookshelf", &format!("<gold>{k}"), &[format!("<gray>{count} messages"), "<yellow>Click to open".into()]),
                    act(move |p, _| menu::open(p, messages(current(p), Some(sec.clone())))));
            }
            let raw = with(|s| s.messages.str(&path, ""));
            let mut lore = text_lore(&raw);
            lore.push("<yellow>Left click <gray>change, type none to stop it being sent".into());
            lore.push("<yellow>Right click <gray>back to the default".into());
            (items::icon("paper", &format!("<white>{k}"), &lore), act(move |p, click| {
                let path = path.clone();
                if menu::is_right(click) {
                    with(|s| { s.messages.remove(&path); s.messages.save(); });
                    return menu::refresh(p);
                }
                let cur = with(|s| s.messages.str(&path, ""));
                menu::ask(p, &format!("New text for {path}:"), &cur, current(p), move |_pl, t| {
                    let v = if t.eq_ignore_ascii_case("none") { String::new() } else { t };
                    with(|s| { s.messages.set_str(&path, &v); s.messages.save(); });
                });
            }))
        }).collect();
        menu::paged(&mut page, entries, page_no.clone());
        page
    })
}
