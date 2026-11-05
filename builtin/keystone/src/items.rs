//! Item helpers: names, comparing, saving to text and giving to players.

use crate::text;
use base64::Engine;
use pumpkin_plugin_api::data_components::DataComponent;
use pumpkin_plugin_api::{ItemStack, Player};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// "STONE", "stone" and "minecraft:stone" all become "minecraft:stone".
pub fn key_of(material: &str) -> String {
    let m = material.trim().to_lowercase();
    if m.contains(':') { m } else { format!("minecraft:{m}") }
}

/// "minecraft:diamond_ore" becomes "DIAMOND_ORE", the style used in the config files.
pub fn material_of(key: &str) -> String {
    key.rsplit(':').next().unwrap_or(key).to_uppercase()
}

pub fn make(material: &str, count: u8) -> ItemStack {
    ItemStack::new(&key_of(material), count.max(1))
}

/// An icon with a MiniMessage name and lore.
pub fn icon(material: &str, name: &str, lore: &[String]) -> ItemStack {
    let it = make(material, 1);
    set_name_lore(&it, Some(name), lore);
    it
}

pub fn set_name_lore(it: &ItemStack, name: Option<&str>, lore: &[String]) {
    if let Some(n) = name {
        it.set_custom_name(Some(text::item(n)));
    }
    if !lore.is_empty() {
        it.set_lore(lore.iter().map(|l| text::item(l)).collect::<Vec<_>>());
    }
}

pub fn add_lore(it: &ItemStack, lines: &[String]) {
    for l in lines {
        it.add_lore(text::item(l));
    }
}

pub fn filler() -> ItemStack {
    let it = make("gray_stained_glass_pane", 1);
    it.set_custom_name(Some(text::item(" ")));
    it
}

/// The name a player sees: the custom name, or the item type made readable.
pub fn display_name(it: &ItemStack) -> String {
    if let Some(n) = it.get_custom_name() {
        let p = text::plain_json(&n.to_json());
        if !p.trim().is_empty() {
            return p;
        }
    }
    let k = material_of(&it.get_registry_key()).to_lowercase().replace('_', " ");
    let mut c = k.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => k,
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    key: String,
    count: u8,
    components: Vec<(u32, String)>,
}

fn components(it: &ItemStack) -> Vec<(u32, String)> {
    let mut v: Vec<(u32, String)> = it
        .get_components()
        .into_iter()
        .map(|c| (c.component as u32, B64.encode(&c.value)))
        .collect();
    v.sort();
    v
}

/// Saves everything about an item (enchantments, names, damage) as text.
pub fn serialize(it: &ItemStack) -> String {
    serde_json::to_string(&Stored { key: it.get_registry_key(), count: it.get_count(), components: components(it) }).unwrap_or_default()
}

pub fn deserialize(s: &str) -> Option<ItemStack> {
    let st: Stored = serde_json::from_str(s).ok()?;
    let it = ItemStack::new(&st.key, st.count.max(1));
    for (id, data) in st.components {
        let Ok(bytes) = B64.decode(data) else { continue };
        if let Some(c) = component_from(id) {
            it.set_component(c, &bytes);
        }
    }
    Some(it)
}

fn component_from(id: u32) -> Option<DataComponent> {
    // Only ids that came out of get-components are ever stored, so they are valid variants.
    let raw = u8::try_from(id).ok()?;
    Some(unsafe { DataComponent::_lift(raw) })
}

pub fn copy(it: &ItemStack, count: u8) -> ItemStack {
    let s: Stored = serde_json::from_str(&serialize(it)).unwrap_or(Stored { key: it.get_registry_key(), count, components: vec![] });
    let c = ItemStack::new(&s.key, count.max(1));
    for (id, data) in s.components {
        if let (Ok(bytes), Some(comp)) = (B64.decode(data), component_from(id)) {
            c.set_component(comp, &bytes);
        }
    }
    c
}

/// Same item apart from the amount.
pub fn similar(a: &ItemStack, b: &ItemStack) -> bool {
    a.get_registry_key() == b.get_registry_key() && components(a) == components(b)
}

/// True when the item has nothing special: no name, lore or enchantments.
pub fn is_plain(it: &ItemStack) -> bool {
    let fresh = ItemStack::new(&it.get_registry_key(), 1);
    components(it) == components(&fresh)
}

const STORAGE: u32 = 36;

/// Puts items into the main inventory, stacking where possible. Returns what did not fit.
pub fn give(p: &Player, it: &ItemStack, mut amount: u32) -> u32 {
    let inv = p.get_inventory().as_inventory();
    let max = it.get_max_count().max(1) as u32;
    for slot in 0..STORAGE.min(inv.get_size()) {
        if amount == 0 {
            break;
        }
        if let Some(s) = inv.get_item(slot) {
            if similar(&s, it) && (s.get_count() as u32) < max {
                let add = (max - s.get_count() as u32).min(amount);
                s.set_count((s.get_count() as u32 + add) as u8);
                inv.set_item(slot, Some(s));
                amount -= add;
            }
        }
    }
    for slot in 0..STORAGE.min(inv.get_size()) {
        if amount == 0 {
            break;
        }
        if inv.get_item(slot).is_none() {
            let put = max.min(amount);
            inv.set_item(slot, Some(copy(it, put as u8)));
            amount -= put;
        }
    }
    amount
}

pub fn count(p: &Player, template: &ItemStack) -> u32 {
    let inv = p.get_inventory().as_inventory();
    (0..STORAGE.min(inv.get_size()))
        .filter_map(|s| inv.get_item(s))
        .filter(|s| similar(s, template))
        .map(|s| s.get_count() as u32)
        .sum()
}

pub fn remove(p: &Player, template: &ItemStack, mut amount: u32) {
    let inv = p.get_inventory().as_inventory();
    for slot in 0..STORAGE.min(inv.get_size()) {
        if amount == 0 {
            break;
        }
        if let Some(s) = inv.get_item(slot) {
            if similar(&s, template) {
                let c = s.get_count() as u32;
                if c <= amount {
                    inv.set_item(slot, None);
                    amount -= c;
                } else {
                    s.set_count((c - amount) as u8);
                    inv.set_item(slot, Some(s));
                    amount = 0;
                }
            }
        }
    }
}

pub fn has_free_slot(p: &Player) -> bool {
    let inv = p.get_inventory().as_inventory();
    (0..STORAGE.min(inv.get_size())).any(|s| inv.get_item(s).is_none())
}
