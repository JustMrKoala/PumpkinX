//! The auction house (/ah). Items are stored as text in auctions-pumpkin.yml.

use crate::core::{self, now_millis, send, uuid_of, with, State, Tell};
use crate::items;
use crate::menu::{self, act, Page};
use crate::text;
use crate::yaml::Yaml;
use pumpkin_plugin_api::common::Hand;
use pumpkin_plugin_api::{ItemStack, Player};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone)]
pub struct Listing {
    pub id: String,
    pub seller: String,
    pub seller_name: String,
    pub item: String,
    pub item_name: String,
    pub amount: u32,
    pub price: f64,
    pub created: i64,
    pub expires: i64,
}

impl Listing {
    pub fn expired(&self) -> bool {
        now_millis() >= self.expires
    }
}

#[derive(Default)]
pub struct Auctions {
    pub listings: Vec<Listing>,
    pub collection: HashMap<String, Vec<String>>,
}

fn path(folder: &str) -> String {
    format!("{folder}/auctions-pumpkin.yml")
}

impl Auctions {
    pub fn load(folder: &str) -> Auctions {
        let y = Yaml::load(&path(folder), None);
        let mut a = Auctions::default();
        for id in y.keys("listings") {
            let p = |k: &str| format!("listings.{id}.{k}");
            a.listings.push(Listing {
                id: id.clone(),
                seller: y.str(&p("seller"), ""),
                seller_name: y.str(&p("seller-name"), "?"),
                item: y.str(&p("item"), ""),
                item_name: y.str(&p("item-name"), "?"),
                amount: y.i64(&p("amount"), 1) as u32,
                price: y.f64(&p("price"), 0.0),
                created: y.i64(&p("created"), 0),
                expires: y.i64(&p("expires"), 0),
            });
        }
        for id in y.keys("collection") {
            a.collection.insert(id.clone(), y.list(&format!("collection.{id}")));
        }
        a
    }

    pub fn save(&self, folder: &str) {
        let mut y = Yaml::empty(&path(folder));
        for l in &self.listings {
            let p = |k: &str| format!("listings.{}.{k}", l.id);
            y.set_str(&p("seller"), &l.seller);
            y.set_str(&p("seller-name"), &l.seller_name);
            y.set_str(&p("item"), &l.item);
            y.set_str(&p("item-name"), &l.item_name);
            y.set_i64(&p("amount"), l.amount as i64);
            y.set_f64(&p("price"), l.price);
            y.set_i64(&p("created"), l.created);
            y.set_i64(&p("expires"), l.expires);
        }
        for (id, items) in &self.collection {
            if !items.is_empty() {
                y.set_list(&format!("collection.{id}"), items);
            }
        }
        y.save();
    }
}

impl State {
    fn ah(&self, key: &str, def: f64) -> f64 {
        self.config.f64(&format!("auctions.{key}"), def)
    }
    fn save_auctions(&self) {
        self.auctions.save(&self.folder);
    }
}

pub fn time_left(ms: i64) -> String {
    let m = (ms / 60_000).max(0);
    if m >= 1440 { format!("{}d {}h", m / 1440, m % 1440 / 60) } else if m >= 60 { format!("{}h {}m", m / 60, m % 60) } else { format!("{m}m") }
}

/// keystone.ah.limit.<n> raises the limit. Pumpkin cannot list permissions, so common steps are checked.
fn max_listings(p: &Player) -> usize {
    let base = with(|s| s.ah("max-listings", 5.0)) as usize;
    let mut max = base;
    for n in [10, 15, 20, 25, 30, 40, 50, 75, 100] {
        if n > max && p.has_permission(&format!("keystone.ah.limit.{n}")) {
            max = n;
        }
    }
    max
}

fn fresh_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn list_item(p: &Player, price: Option<f64>) {
    let Some(hand) = p.get_item_in_hand(Hand::Right) else { return send(p, "auction.hold-item", &[]) };
    let id = uuid_of(p);
    let limit = max_listings(p);
    let result = with(|s| {
        let (min, max) = (s.ah("min-price", 1.0), s.ah("max-price", 1e9));
        let Some(price) = price.filter(|v| *v >= min && *v <= max) else {
            return Err(("auction.bad-price", vec![("min", s.format(min)), ("max", s.format(max))]));
        };
        if s.auctions.listings.iter().filter(|l| l.seller == id).count() >= limit {
            return Err(("auction.too-many", vec![("max", limit.to_string())]));
        }
        let fee = s.round(price * s.ah("listing-fee-percent", 0.0).clamp(0.0, 100.0) / 100.0);
        if fee > 0.0 && !s.withdraw(&id, fee) {
            return Err(("auction.cannot-pay-fee", vec![("fee", s.format(fee))]));
        }
        Ok((s.round(price), fee, (s.ah("duration-hours", 48.0) * 3_600_000.0) as i64))
    });
    let (price, fee, duration) = match result {
        Ok(v) => v,
        Err((key, kv)) => {
            let kv: Vec<(&str, &str)> = kv.iter().map(|(k, v)| (*k, v.as_str())).collect();
            return send(p, key, &kv);
        }
    };
    let data = items::serialize(&hand);
    let name = items::display_name(&hand);
    let amount = hand.get_count() as u32;
    p.set_item_in_hand(Hand::Right, None);
    let now = now_millis();
    let pname = p.get_name();
    let (price_s, fee_s) = with(|s| {
        s.auctions.listings.push(Listing { id: fresh_id(), seller: id.clone(), seller_name: pname, item: data, item_name: name.clone(), amount, price, created: now, expires: now + duration });
        s.save_auctions();
        (s.format(price), s.format(fee))
    });
    send(p, "auction.listed", &[("item", &name), ("amount", &amount.to_string()), ("price", &price_s), ("time", &time_left(duration)), ("fee", &fee_s)]);
}

/// Into the inventory, or into the collection when it is full. Items are never dropped.
fn give(p: &Player, data: &str) {
    let Some(it) = items::deserialize(data) else { return };
    let n = it.get_count() as u32;
    let left = items::give(p, &it, n);
    if left > 0 {
        let rest = items::serialize(&items::copy(&it, left as u8));
        let id = uuid_of(p);
        with(|s| {
            s.auctions.collection.entry(id).or_default().push(rest);
            s.save_auctions();
        });
        send(p, "auction.to-collection", &[]);
    }
}

pub fn buy(p: &Player, listing: &str) -> bool {
    let id = uuid_of(p);
    let result = with(|s| {
        let Some(pos) = s.auctions.listings.iter().position(|l| l.id == listing && !l.expired()) else { return Err(("auction.gone", vec![])) };
        let l = s.auctions.listings[pos].clone();
        if l.seller == id {
            return Err(("auction.own-item", vec![]));
        }
        if !s.withdraw(&id, l.price) {
            return Err(("economy.not-enough", vec![("balance", s.format(s.balance(&id)))]));
        }
        let earned = s.round(l.price * (100.0 - s.ah("sales-tax-percent", 5.0).clamp(0.0, 100.0)) / 100.0);
        if !s.deposit(&l.seller, earned) {
            s.deposit(&id, l.price);
            return Err(("auction.seller-full", vec![]));
        }
        s.auctions.listings.remove(pos);
        s.save_auctions();
        Ok((l.clone(), s.format(l.price), s.format(earned)))
    });
    match result {
        Ok((l, price, earned)) => {
            give(p, &l.item);
            send(p, "auction.bought", &[("item", &l.item_name), ("amount", &l.amount.to_string()), ("price", &price), ("seller", &l.seller_name)]);
            if let Some(seller) = core::player_by_uuid(&l.seller) {
                send(&seller, "auction.sold-notify", &[("item", &l.item_name), ("amount", &l.amount.to_string()), ("price", &earned), ("buyer", &p.get_name())]);
            }
            true
        }
        Err((key, kv)) => {
            let kv: Vec<(&str, &str)> = kv.iter().map(|(k, v)| (*k, v.as_str())).collect();
            send(p, key, &kv);
            false
        }
    }
}

/// Takes a listing off the market. It goes to the seller, or their collection.
pub fn cancel(listing: &str, by: Option<&Player>) {
    let l = with(|s| {
        let pos = s.auctions.listings.iter().position(|l| l.id == listing)?;
        let l = s.auctions.listings.remove(pos);
        s.save_auctions();
        Some(l)
    });
    let Some(l) = l else { return };
    match by {
        Some(p) if uuid_of(p) == l.seller => give(p, &l.item),
        _ => with(|s| {
            s.auctions.collection.entry(l.seller.clone()).or_default().push(l.item.clone());
            s.save_auctions();
        }),
    }
    if let Some(p) = by {
        send(p, "auction.cancelled", &[("item", &l.item_name)]);
    }
}

/// Moves expired listings to their seller's collection. Runs every minute.
pub fn expire() {
    let expired = with(|s| {
        let (gone, keep): (Vec<Listing>, Vec<Listing>) = s.auctions.listings.drain(..).partition(|l| l.expired());
        s.auctions.listings = keep;
        for l in &gone {
            s.auctions.collection.entry(l.seller.clone()).or_default().push(l.item.clone());
        }
        if !gone.is_empty() {
            s.save_auctions();
        }
        gone
    });
    for l in expired {
        if let Some(p) = core::player_by_uuid(&l.seller) {
            send(&p, "auction.expired", &[("item", &l.item_name)]);
        }
    }
}

// ---- menus ----

fn listing_icon(l: &Listing, extra: &[String]) -> ItemStack {
    let it = items::deserialize(&l.item).unwrap_or_else(|| items::make("paper", 1));
    let (price, left) = (crate::economy::format(l.price), time_left(l.expires - now_millis()));
    let mut lore = vec![
        String::new(),
        core::msg("auction.lore-price", &[("price", &price)]),
        core::msg("auction.lore-seller", &[("seller", &l.seller_name)]),
        core::msg("auction.lore-time", &[("time", &left)]),
    ];
    lore.extend_from_slice(extra);
    items::add_lore(&it, &lore);
    it
}

pub fn browse(filter: String) -> menu::Builder {
    let current = Rc::new(Cell::new(0usize));
    let me: Rc<Cell<Option<menu::Builder>>> = Rc::new(Cell::new(None));
    let builder: menu::Builder = {
        let me = me.clone();
        Rc::new(move |p: &Player| {
            let id = uuid_of(p);
            let f = filter.to_lowercase();
            let list: Vec<Listing> = with(|s| {
                let mut v: Vec<Listing> = s.auctions.listings.iter().filter(|l| !l.expired())
                    .filter(|l| f.is_empty() || l.item_name.to_lowercase().contains(&f) || l.seller_name.to_lowercase().contains(&f) || l.item.to_lowercase().contains(&f))
                    .cloned().collect();
                v.sort_by(|a, b| b.created.cmp(&a.created));
                v
            });
            let title = core::msg("auction.title", &[]) + &if f.is_empty() { String::new() } else { format!(" <dark_gray>\"{}\"", text::escape(&filter)) };
            let mut page = Page::new(&title, 6).player_menu();
            let admin = p.has_permission("keystone.ah.admin");
            let self_builder = me.take();
            me.set(self_builder.clone());
            let entries = list.into_iter().map(|l| {
                let mut extra = vec![core::msg(if l.seller == id { "auction.lore-yours" } else { "auction.lore-click-buy" }, &[])];
                if admin { extra.push(core::msg("auction.lore-admin-remove", &[])); }
                let icon = listing_icon(&l, &extra);
                let (lid, mine, back) = (l.id.clone(), l.seller == id, self_builder.clone());
                (icon, act(move |pl, click| {
                    if admin && click == pumpkin_plugin_api::common::ClickType::ShiftRight {
                        cancel(&lid, Some(pl));
                        menu::refresh(pl);
                    } else if mine {
                        menu::open(pl, mine_menu(back.clone()));
                    } else {
                        menu::open(pl, confirm(lid.clone(), back.clone()));
                    }
                }))
            }).collect();
            menu::paged(&mut page, entries, current.clone());
            let (count, waiting) = with(|s| (s.auctions.listings.iter().filter(|l| l.seller == id).count(), s.auctions.collection.get(&id).map(|c| c.len()).unwrap_or(0)));
            page.show(46, items::icon("book", &core::msg("auction.button-help", &[]), &[core::msg("auction.help-lore", &[])]));
            let max = max_listings(p).to_string();
            let b1 = self_builder.clone();
            page.button(47, items::icon("chest", &core::msg("auction.button-mine", &[]), &[core::msg("auction.mine-lore", &[("count", &count.to_string()), ("max", &max)])]), move |pl, _| menu::open(pl, mine_menu(b1.clone())));
            let has_filter = !f.is_empty();
            page.button(49, items::icon("spyglass", &core::msg("auction.button-search", &[]), &[core::msg(if has_filter { "auction.search-clear-lore" } else { "auction.search-lore" }, &[])]), move |pl, click| {
                if has_filter && menu::is_right(click) {
                    return menu::open(pl, browse(String::new()));
                }
                menu::ask(pl, &core::msg("auction.search-question", &[]), "", None, |pl, text| menu::open(pl, browse(text)));
            });
            let b2 = self_builder.clone();
            page.button(51, items::icon(if waiting > 0 { "ender_chest" } else { "barrel" }, &core::msg("auction.button-collection", &[]), &[core::msg("auction.collection-lore", &[("count", &waiting.to_string())])]), move |pl, _| menu::open(pl, collection_menu(b2.clone())));
            page.button(52, items::icon("clock", &core::msg("auction.button-refresh", &[]), &[]), |pl, _| menu::refresh(pl));
            page
        })
    };
    me.set(Some(builder.clone()));
    builder
}

fn confirm(listing: String, back: Option<menu::Builder>) -> menu::Builder {
    Rc::new(move |p: &Player| {
        let mut page = Page::new(&core::msg("auction.confirm-title", &[]), 3).player_menu().parent(back.clone());
        let l = with(|s| s.auctions.listings.iter().find(|l| l.id == listing && !l.expired()).cloned());
        let Some(l) = l else {
            page.show(13, items::icon("barrier", &core::msg("auction.gone", &[]), &[]));
            return page;
        };
        page.show(13, listing_icon(&l, &[]));
        let id = uuid_of(p);
        let (price, bal) = with(|s| (s.format(l.price), s.format(s.balance(&id))));
        let (lid, b) = (l.id.clone(), back.clone());
        page.button(11, items::icon("lime_concrete", &core::msg("auction.button-confirm", &[("price", &price)]), &[core::msg("auction.balance-lore", &[("balance", &bal)])]), move |pl, _| {
            if buy(pl, &lid) {
                if let Some(b) = b.clone() { menu::open(pl, b) } else { menu::close(pl) }
            } else {
                menu::refresh(pl);
            }
        });
        let b = back.clone();
        page.button(15, items::icon("red_concrete", &core::msg("auction.button-cancel", &[]), &[]), move |pl, _| {
            if let Some(b) = b.clone() { menu::open(pl, b) } else { menu::close(pl) }
        });
        page
    })
}

fn mine_menu(back: Option<menu::Builder>) -> menu::Builder {
    let current = Rc::new(Cell::new(0usize));
    Rc::new(move |p: &Player| {
        let id = uuid_of(p);
        let mut page = Page::new(&core::msg("auction.mine-title", &[]), 6).player_menu().parent(back.clone().or_else(|| Some(browse(String::new()))));
        let list: Vec<Listing> = with(|s| s.auctions.listings.iter().filter(|l| l.seller == id).cloned().collect());
        let entries = list.into_iter().map(|l| {
            let icon = listing_icon(&l, &[core::msg("auction.lore-click-cancel", &[])]);
            let lid = l.id.clone();
            (icon, act(move |pl, _| { cancel(&lid, Some(pl)); menu::refresh(pl); }))
        }).collect();
        menu::paged(&mut page, entries, current.clone());
        page
    })
}

fn collection_menu(back: Option<menu::Builder>) -> menu::Builder {
    let current = Rc::new(Cell::new(0usize));
    Rc::new(move |p: &Player| {
        let id = uuid_of(p);
        let mut page = Page::new(&core::msg("auction.collection-title", &[]), 6).player_menu().parent(back.clone().or_else(|| Some(browse(String::new()))));
        let list: Vec<String> = with(|s| s.auctions.collection.get(&id).cloned().unwrap_or_default());
        let empty = list.is_empty();
        let entries = list.into_iter().enumerate().map(|(i, data)| {
            let it = items::deserialize(&data).unwrap_or_else(|| items::make("paper", 1));
            items::add_lore(&it, &[core::msg("auction.lore-click-claim", &[])]);
            (it, act(move |pl, _| { claim(pl, Some(i)); menu::refresh(pl); }))
        }).collect();
        menu::paged(&mut page, entries, current.clone());
        if !empty {
            page.button(49, items::icon("hopper", &core::msg("auction.button-claim-all", &[]), &[]), |pl, _| { claim(pl, None); menu::refresh(pl); });
        }
        page
    })
}

/// Claims one item, or as many as fit when `index` is None.
fn claim(p: &Player, index: Option<usize>) {
    let id = uuid_of(p);
    loop {
        if !items::has_free_slot(p) {
            return send(p, "auction.inventory-full", &[]);
        }
        let next = with(|s| {
            let list = s.auctions.collection.get_mut(&id)?;
            let i = index.unwrap_or(0);
            if i >= list.len() { return None; }
            let data = list.remove(i);
            if list.is_empty() { s.auctions.collection.remove(&id); }
            s.save_auctions();
            Some(data)
        });
        let Some(data) = next else { return };
        give(p, &data);
        if index.is_some() {
            return;
        }
    }
}

pub fn ah_cmd(sender: &dyn Tell, args: &[String]) {
    if !core::module("auctions") {
        return send(sender, "general.module-disabled", &[("module", "auctions")]);
    }
    let Some(p) = sender.player() else { return send(sender, "general.players-only", &[]) };
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "" => menu::open(&p, browse(String::new())),
        "sell" | "list" => {
            if !p.has_permission("keystone.ah.sell") {
                return send(&p, "general.no-permission", &[]);
            }
            match args.get(1) {
                Some(a) => list_item(&p, with(|s| s.parse_amount(a))),
                None => send(&p, "auction.usage", &[]),
            }
        }
        "search" => menu::open(&p, browse(args[1..].join(" "))),
        "mine" | "listings" => menu::open(&p, mine_menu(None)),
        "collect" | "collection" => menu::open(&p, collection_menu(None)),
        _ => send(&p, "auction.usage", &[]),
    }
}
