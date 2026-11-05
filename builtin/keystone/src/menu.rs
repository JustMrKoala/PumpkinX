//! Chest menus and chat prompts. Every screen is a builder that is run again on refresh,
//! so values always show their current state.

use crate::core::{self, now_millis, uuid_of};
use crate::items;
use crate::text;
use pumpkin_plugin_api::common::ClickType;
use pumpkin_plugin_api::gui::Gui;
use pumpkin_plugin_api::Inventory;
use pumpkin_plugin_api::{ItemStack, Player, Screen};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub type Action = Rc<dyn Fn(&Player, ClickType)>;
pub type Builder = Rc<dyn Fn(&Player) -> Page>;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Nothing can be taken out or put in.
    Locked,
    /// Players may put items in the top slots, except the bottom row (the sell menu).
    Drop,
}

pub struct Page {
    pub title: String,
    pub rows: u32,
    pub items: Vec<(u32, ItemStack, Option<Action>)>,
    pub parent: Option<Builder>,
    pub kind: Kind,
    /// Admin screens close for anyone without keystone.admin.
    pub admin: bool,
    /// Runs when the menu is closed, for the sell menu.
    pub on_close: Option<Rc<dyn Fn(&Player, &Inventory)>>,
    /// Runs after every render, for live values such as the sell total.
    pub on_render: Option<Rc<dyn Fn(&Inventory)>>,
}

impl Page {
    pub fn new(title: &str, rows: u32) -> Page {
        Page { title: title.to_string(), rows: rows.clamp(1, 6), items: Vec::new(), parent: None, kind: Kind::Locked, admin: true, on_close: None, on_render: None }
    }

    pub fn player_menu(mut self) -> Page {
        self.admin = false;
        self
    }

    pub fn parent(mut self, p: Option<Builder>) -> Page {
        self.parent = p;
        self
    }

    pub fn button(&mut self, slot: u32, item: ItemStack, action: impl Fn(&Player, ClickType) + 'static) {
        self.items.push((slot, item, Some(Rc::new(action))));
    }

    pub fn show(&mut self, slot: u32, item: ItemStack) {
        self.items.push((slot, item, None));
    }

    pub fn size(&self) -> u32 {
        self.rows * 9
    }
}

struct Open {
    gui: Inventory,
    builder: Builder,
    actions: HashMap<u32, Action>,
    size: u32,
    kind: Kind,
    admin: bool,
    opened: i64,
    on_close: Option<Rc<dyn Fn(&Player, &Inventory)>>,
}

type Answer = Box<dyn FnOnce(&Player, String)>;

thread_local! {
    static OPEN: RefCell<HashMap<String, Open>> = RefCell::new(HashMap::new());
    static PROMPTS: RefCell<HashMap<String, (Answer, Option<Builder>)>> = RefCell::new(HashMap::new());
}

fn screen(rows: u32) -> Screen {
    match rows {
        1 => Screen::Generic9x1,
        2 => Screen::Generic9x2,
        3 => Screen::Generic9x3,
        4 => Screen::Generic9x4,
        5 => Screen::Generic9x5,
        _ => Screen::Generic9x6,
    }
}

/// Fills the gui and returns the click actions. Adds back, close and filler to the bottom row.
fn render(gui: &Inventory, page: Page, p: &Player) -> (HashMap<u32, Action>, u32, Kind, bool, Option<Rc<dyn Fn(&Player, &Inventory)>>) {
    let size = page.size();
    let mut actions: HashMap<u32, Action> = HashMap::new();
    if page.kind == Kind::Locked {
        gui.clear();
    }
    let on_render = page.on_render.clone();
    let bar = size - 9;
    let mut used = std::collections::HashSet::new();
    for (slot, item, action) in page.items {
        if slot >= size {
            continue;
        }
        used.insert(slot);
        gui.set_item(slot, Some(item));
        if let Some(a) = action {
            actions.insert(slot, a);
        }
    }
    if page.rows > 1 || page.parent.is_some() {
        if let Some(parent) = page.parent.clone() {
            if !used.contains(&bar) {
                used.insert(bar);
                gui.set_item(bar, Some(items::icon("arrow", "<yellow>Back", &[])));
                actions.insert(bar, Rc::new(move |pl: &Player, _| open(pl, parent.clone())));
            }
        }
        if !used.contains(&(size - 1)) {
            used.insert(size - 1);
            gui.set_item(size - 1, Some(items::icon("barrier", "<red>Close", &[])));
            actions.insert(size - 1, Rc::new(|pl: &Player, _| close(pl)));
        }
        for s in bar..size {
            if !used.contains(&s) {
                gui.set_item(s, Some(items::filler()));
            }
        }
    }
    if let Some(f) = on_render {
        f(gui);
    }
    let _ = p;
    (actions, size, page.kind, page.admin, page.on_close)
}

/// Opens a new screen for the player.
pub fn open(p: &Player, builder: Builder) {
    let page = builder(p);
    let title = core::with(|s| s.brand(&page.title));
    let gui = Gui::new(screen(page.rows), text::parse(&title));
    if page.kind == Kind::Drop {
        gui.set_allow_put_items(true);
        gui.set_allow_grab_items(true);
    } else {
        gui.set_allow_put_items(false);
        gui.set_allow_grab_items(false);
    }
    let inv = gui.get_inventory();
    let (actions, size, kind, admin, on_close) = render(&inv, page, p);
    // A drop menu that is being replaced must hand its items back first.
    settle_drop_menu(p);
    let id = uuid_of(p);
    let opened = now_millis();
    p.open_gui(gui);
    OPEN.with(|o| {
        o.borrow_mut().insert(id, Open { gui: inv, builder, actions, size, kind, admin, opened, on_close });
    });
}

/// Rebuilds the current screen in place.
pub fn refresh(p: &Player) {
    let id = uuid_of(p);
    let builder = OPEN.with(|o| o.borrow().get(&id).map(|m| m.builder.clone()));
    let Some(builder) = builder else { return };
    let page = builder(p);
    let gui = OPEN.with(|o| o.borrow_mut().remove(&id));
    let Some(mut open_menu) = gui else { return };
    let (actions, size, kind, admin, on_close) = render(&open_menu.gui, page, p);
    open_menu.actions = actions;
    open_menu.size = size;
    open_menu.kind = kind;
    open_menu.admin = admin;
    open_menu.on_close = on_close;
    OPEN.with(|o| {
        o.borrow_mut().insert(id, open_menu);
    });
}

pub fn close(p: &Player) {
    settle_drop_menu(p);
    let id = uuid_of(p);
    OPEN.with(|o| o.borrow_mut().remove(&id));
    close_screen(p);
}

/// Pumpkin has no call for this, so it sends the close packet itself.
pub fn close_screen(p: &Player) {
    use pumpkin_plugin_api::java_packets::{CCloseContainer, ClientboundPacket};
    if let Some(j) = p.as_java() {
        j.send_packet(&ClientboundPacket::CCloseContainer(CCloseContainer { sync_id: 0 }));
    }
}

fn settle_drop_menu(p: &Player) {
    let id = uuid_of(p);
    let pending = OPEN.with(|o| {
        let mut map = o.borrow_mut();
        match map.get(&id) {
            Some(m) if m.kind == Kind::Drop => map.remove(&id),
            _ => None,
        }
    });
    if let Some(m) = pending {
        if let Some(f) = m.on_close {
            f(p, &m.gui);
        }
    }
}

/// Returns true when the click must be cancelled.
pub fn on_click(p: &Player, raw_slot: i16, click: ClickType) -> bool {
    let id = uuid_of(p);
    let info = OPEN.with(|o| o.borrow().get(&id).map(|m| (m.size, m.kind, m.admin, m.actions.get(&(raw_slot as u32)).cloned())));
    let Some((size, kind, admin, action)) = info else { return false };
    if admin && !p.has_permission("keystone.admin") {
        close(p);
        return true;
    }
    let in_top = raw_slot >= 0 && (raw_slot as u32) < size;
    let cancel = match kind {
        Kind::Locked => true,
        // Items may go in and out of the top area. The bottom row holds the buttons.
        Kind::Drop => (in_top && raw_slot as u32 >= size - 9) || click == ClickType::DoubleClick,
    };
    if in_top {
        if let Some(a) = action {
            a(p, click);
        } else if kind == Kind::Drop {
            refresh_later(p);
        }
    } else if kind == Kind::Drop {
        refresh_later(p);
    }
    cancel
}

fn refresh_later(p: &Player) {
    let id = uuid_of(p);
    pumpkin_plugin_api::scheduler::schedule_delayed_task(1, move |srv| {
        let player = srv.get_all_players().into_iter().find(|pl| uuid_of(pl) == id);
        if let Some(pl) = player {
            refresh(&pl);
        }
    });
}

/// The builder of the screen the player has open, to come back to after a prompt.
pub fn current(p: &Player) -> Option<Builder> {
    let id = uuid_of(p);
    OPEN.with(|o| o.borrow().get(&id).map(|m| m.builder.clone()))
}

pub fn is_open(p: &Player) -> bool {
    let id = uuid_of(p);
    OPEN.with(|o| o.borrow().contains_key(&id))
}

pub fn is_drop_menu(p: &Player) -> bool {
    let id = uuid_of(p);
    OPEN.with(|o| o.borrow().get(&id).map(|m| m.kind == Kind::Drop).unwrap_or(false))
}

pub fn on_close(p: &Player) {
    let id = uuid_of(p);
    let drop = OPEN.with(|o| o.borrow().get(&id).map(|m| (m.kind == Kind::Drop, m.opened)));
    match drop {
        Some((true, _)) => settle_drop_menu(p),
        // Switching screens can send a close for the previous one; ignore those.
        Some((false, opened)) if now_millis() - opened > 250 => {
            OPEN.with(|o| o.borrow_mut().remove(&id));
        }
        _ => {}
    }
}

pub fn on_quit(p: &Player) {
    settle_drop_menu(p);
    let id = uuid_of(p);
    OPEN.with(|o| o.borrow_mut().remove(&id));
    PROMPTS.with(|m| m.borrow_mut().remove(&id));
}

// ---- chat prompts ----

/// Asks a question in chat. The answer never reaches chat. "cancel" goes back.
pub fn ask(p: &Player, question: &str, current: &str, back: Option<Builder>, answer: impl FnOnce(&Player, String) + 'static) {
    let id = uuid_of(p);
    PROMPTS.with(|m| m.borrow_mut().insert(id.clone(), (Box::new(answer), back)));
    settle_drop_menu(p);
    OPEN.with(|o| o.borrow_mut().remove(&id));
    close_screen(p);
    core::say(p, &format!("<yellow>{question}"));
    if !current.is_empty() {
        let c = text::parse("<gray>Current: ").add_child(
            pumpkin_plugin_api::text::TextComponent::text(current)
                .click_suggest_command(current)
                .hover_show_text(text::parse("<gray>Click to paste it into chat")),
        );
        p.send_system_message(c, false);
    }
    p.send_system_message(text::parse("<dark_gray>Type your answer in chat, or <white>cancel</white> to go back."), false);
}

/// Returns true when the chat message was an answer and must not be shown.
pub fn on_chat(p: &Player, message: &str) -> bool {
    let id = uuid_of(p);
    let pending = PROMPTS.with(|m| m.borrow_mut().remove(&id));
    let Some((answer, back)) = pending else { return false };
    let text = message.trim().to_string();
    if text.eq_ignore_ascii_case("cancel") {
        if let Some(b) = back {
            open(p, b);
        }
    } else {
        answer(p, text);
        if let Some(b) = back {
            open(p, b);
        }
    }
    true
}

// ---- paging ----

/// Lays out a list over pages of 45 slots, with previous and next buttons.
pub fn paged(page: &mut Page, entries: Vec<(ItemStack, Option<Action>)>, current: Rc<Cell<usize>>) {
    let per = 45usize;
    let pages = entries.len().div_ceil(per).max(1);
    let cur = current.get().min(pages - 1);
    current.set(cur);
    for (i, (item, action)) in entries.into_iter().enumerate().skip(cur * per).take(per) {
        let slot = (i - cur * per) as u32;
        page.items.push((slot, item, action));
    }
    if cur > 0 {
        let c = current.clone();
        page.button(48, items::icon("spectral_arrow", "<yellow>Previous page", &[]), move |pl, _| {
            c.set(c.get().saturating_sub(1));
            refresh(pl);
        });
    }
    if cur + 1 < pages {
        let c = current.clone();
        page.button(50, items::icon("spectral_arrow", "<yellow>Next page", &[]), move |pl, _| {
            c.set(c.get() + 1);
            refresh(pl);
        });
    }
}

pub fn act(f: impl Fn(&Player, ClickType) + 'static) -> Option<Action> {
    Some(Rc::new(f))
}

pub fn is_shift(c: ClickType) -> bool {
    matches!(c, ClickType::ShiftLeft | ClickType::ShiftRight)
}

pub fn is_right(c: ClickType) -> bool {
    matches!(c, ClickType::Right | ClickType::ShiftRight)
}
