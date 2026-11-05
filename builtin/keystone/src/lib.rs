//! Keystone for Pumpkin. The same plugin as the Java version, rewritten against Pumpkin's
//! WebAssembly plugin API and reading the same config files.

mod admin;
mod auction;
mod auth;
mod core;
mod data;
mod economy;
mod items;
mod menu;
mod placeholders;
mod rewards;
mod roles;
mod shop;
mod sidebar;
mod skins;
mod text;
mod yaml;

use crate::core::{send, uuid_of, with, Tell};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType, Arg};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::events::*;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::scheduler::schedule_repeating_task;
use pumpkin_plugin_api::{permissions, register_plugin, Context, Player, Plugin, PluginMetadata, Server};

pub use crate::core::log;

struct Keystone;

const CONFIG_FILES: [(&str, &str); 4] = [
    ("config.yml", core::DEFAULT_CONFIG),
    ("messages.yml", core::DEFAULT_MESSAGES),
    ("sidebar.yml", core::DEFAULT_SIDEBAR),
    ("shop.yml", core::DEFAULT_SHOP),
];

impl Plugin for Keystone {
    fn new() -> Self {
        Keystone
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "Keystone".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Agathe".into()],
            description: "One plugin for the whole server: economy, shop, auctions, roles, sidebar, login and skins, all editable in game.".into(),
            dependencies: vec![],
            permissions: vec![permissions::FS_READ_DATA.into(), permissions::FS_WRITE_DATA.into(), permissions::HTTP_OUTBOUND.into(), permissions::NETWORK_DNS.into()],
        }
    }

    fn on_load(&self, ctx: Context) -> pumpkin_plugin_api::Result<()> {
        let folder = ctx.get_data_folder();
        let _ = std::fs::create_dir_all(&folder);
        for (file, default) in CONFIG_FILES {
            yaml::Yaml::load(&format!("{folder}/{file}"), Some(default));
        }
        let state = load_state(&folder);
        core::install(state, ctx.get_server());
        let command = with(|s| {
            s.admin_command = s.brand_command();
            s.admin_command.clone()
        });

        register_permissions(&ctx);
        register_commands(&ctx, &command);
        register_events(&ctx)?;

        schedule_repeating_task(5, 5, |_| sidebar::tick(5));
        schedule_repeating_task(100, 100, |_| auth::tick());
        schedule_repeating_task(1200, 1200, |_| {
            rewards::playtime();
            auction::expire();
        });
        let autosave = with(|s| s.config.i64("data.autosave-minutes", 5)).max(1) as u64 * 1200;
        schedule_repeating_task(autosave, autosave, |_| save_all());

        for p in core::online_players() {
            on_join(&p);
        }
        log(&format!("Keystone is ready. Admin menu: /{command} (or /keystone)."));
        Ok(())
    }

    fn on_unload(&self, _ctx: Context) -> pumpkin_plugin_api::Result<()> {
        for p in core::online_players() {
            menu::on_quit(&p);
        }
        save_all();
        Ok(())
    }
}

register_plugin!(Keystone);

fn load_state(folder: &str) -> core::State {
    core::State {
        folder: folder.to_string(),
        config: yaml::Yaml::load(&format!("{folder}/config.yml"), Some(core::DEFAULT_CONFIG)),
        messages: yaml::Yaml::load(&format!("{folder}/messages.yml"), Some(core::DEFAULT_MESSAGES)),
        players: data::load(folder),
        players_dirty: false,
        online: Default::default(),
        roles: roles::Roles::load(folder),
        shop: shop::Shop::load(folder),
        sidebar: sidebar::Sidebar::load(folder),
        auctions: auction::Auctions::load(folder),
        auth: Default::default(),
        skins: skins::SkinCache::load(folder),
        rewards: Default::default(),
        admin_command: String::new(),
    }
}

/// Reads every file again and applies the result to everyone online.
pub fn reload_all() {
    with(|s| {
        let f = s.folder.clone();
        s.config = yaml::Yaml::load(&format!("{f}/config.yml"), Some(core::DEFAULT_CONFIG));
        s.messages = yaml::Yaml::load(&format!("{f}/messages.yml"), Some(core::DEFAULT_MESSAGES));
        let applied = std::mem::take(&mut s.roles.applied);
        s.roles = roles::Roles::load(&f);
        s.roles.applied = applied;
        s.shop = shop::Shop::load(&f);
        s.sidebar.reload(&f);
    });
    auth::reload();
    roles::refresh_all();
    for p in core::online_players() {
        sidebar::update(&p);
    }
}

fn save_all() {
    with(|s| {
        data::save(s);
        s.players_dirty = false;
        let f = s.folder.clone();
        s.skins.save(&f);
        s.auctions.save(&f);
    });
}

// ---- permissions ----

fn register_permissions(ctx: &Context) {
    let op = PermissionDefault::Op(PermissionLevel::Two);
    let all = PermissionDefault::Allow;
    let list: [(&str, &str, &PermissionDefault); 19] = [
        ("keystone.use", "Basic commands like /role, /sidebar and /login.", &all),
        ("keystone.balance", "See your balance.", &all),
        ("keystone.balance.others", "See someone else's balance.", &all),
        ("keystone.pay", "Send money.", &all),
        ("keystone.baltop", "See the richest players.", &all),
        ("keystone.shop", "Use the shop.", &all),
        ("keystone.sell", "Sell items.", &all),
        ("keystone.skin", "Change your own skin.", &all),
        ("keystone.ah", "Use the auction house.", &all),
        ("keystone.ah.sell", "Sell in the auction house.", &all),
        ("keystone.admin", "Every admin feature, including the admin menu.", &op),
        ("keystone.eco", "Change balances.", &op),
        ("keystone.shop.admin", "Edit the shop.", &op),
        ("keystone.role.admin", "Edit roles.", &op),
        ("keystone.sidebar.admin", "Edit the sidebar.", &op),
        ("keystone.auth.admin", "Manage accounts.", &op),
        ("keystone.skin.others", "Change other players' skins.", &op),
        ("keystone.chat.color", "Use colors in chat.", &op),
        ("keystone.ah.admin", "Remove any auction listing.", &op),
    ];
    for (node, desc, default) in list {
        let children = if node == "keystone.admin" {
            core::KNOWN_PERMISSIONS.iter().filter(|n| **n != "keystone.admin").map(|n| pumpkin_plugin_api::permission::PermissionChild { node: n.to_string(), value: true }).collect()
        } else {
            vec![]
        };
        let res = ctx.register_permission(&Permission { node: node.into(), description: desc.into(), default: *default, children });
        if let Err(e) = res {
            core::warn(&format!("Could not register {node}: {e}"));
        }
    }
}

// ---- commands ----

type Handler = fn(&str, &dyn Tell, &[String]);

struct Cmd {
    name: &'static str,
    run: Handler,
    has_args: bool,
}

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let list: Vec<String> = if self.has_args {
            match args.get_value("args") {
                Arg::Simple(s) | Arg::Msg(s) => s.split_whitespace().map(String::from).collect(),
                _ => vec![],
            }
        } else {
            vec![]
        };
        if let Some(p) = sender.as_player() {
            // Before logging in, only the login commands work.
            if auth::is_locked(&p) && !auth::allowed_command(self.name) {
                send(&p, "auth.login-first", &[]);
                return Ok(0);
            }
        }
        (self.run)(self.name, &sender, &list);
        Ok(1)
    }
}

fn add(ctx: &Context, names: &[&str], desc: &str, permission: &str, name: &'static str, run: Handler) {
    let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    let cmd = Command::new(&names, desc)
        .execute(Cmd { name, run, has_args: false })
        .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd { name, run, has_args: true }));
    ctx.register_command(cmd, permission);
}

fn register_commands(ctx: &Context, admin_name: &str) {
    add(ctx, &["balance", "bal", "money"], "Show a balance.", "keystone.balance", "balance", |_, s, a| economy::balance_cmd(s, a));
    add(ctx, &["pay"], "Send money to a player.", "keystone.pay", "pay", |_, s, a| economy::pay_cmd(s, a));
    add(ctx, &["baltop", "moneytop", "balancetop"], "Richest players.", "keystone.baltop", "baltop", |_, s, a| economy::baltop_cmd(s, a));
    add(ctx, &["eco", "economy"], "Give, take, set or reset balances.", "keystone.eco", "eco", |_, s, a| economy::eco_cmd(s, a));
    add(ctx, &["shop"], "Open or edit the shop.", "keystone.shop", "shop", |_, s, a| shop::shop_cmd(s, a));
    add(ctx, &["sell"], "Drop items in a chest to sell them, or /sell hand and /sell all.", "keystone.sell", "sell", |_, s, a| shop::sell_cmd(s, a));
    add(ctx, &["ah", "auction", "auctions", "auctionhouse"], "The auction house.", "keystone.ah", "ah", |_, s, a| auction::ah_cmd(s, a));
    add(ctx, &["role", "rank", "roles"], "Roles, prefixes and permissions.", "keystone.use", "role", |_, s, a| roles::role_cmd(s, a));
    add(ctx, &["sidebar", "sb"], "Toggle or edit the sidebar.", "keystone.use", "sidebar", |_, s, a| sidebar::sidebar_cmd(s, a));
    add(ctx, &["register", "reg"], "Create a password.", "keystone.use", "register", auth::auth_cmd);
    add(ctx, &["login", "l", "log"], "Log in.", "keystone.use", "login", auth::auth_cmd);
    add(ctx, &["logout"], "Log out without leaving.", "keystone.use", "logout", auth::auth_cmd);
    add(ctx, &["changepassword", "changepw"], "Change your password.", "keystone.use", "changepassword", auth::auth_cmd);
    add(ctx, &["authadmin"], "Manage player accounts.", "keystone.auth.admin", "authadmin", auth::auth_cmd);
    add(ctx, &["skin"], "Wear the skin of any premium account.", "keystone.skin", "skin", |_, s, a| skins::skin_cmd(s, a));

    // The admin command uses the server's own name. /keystone always works as well.
    let mut names = vec!["keystone".to_string()];
    if admin_name != "keystone" {
        names.insert(0, admin_name.to_string());
    }
    for alias in with(|s| s.config.list("branding.command-aliases")) {
        let a: String = alias.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !a.is_empty() && !names.contains(&a) {
            names.push(a);
        }
    }
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    add(ctx, &refs, "Opens the admin menu, or shows help.", "keystone.use", "admin", admin_cmd);
}

fn admin_cmd(_: &str, sender: &dyn Tell, args: &[String]) {
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    if sub == "reload" || sub == "save" {
        if !sender.has("keystone.admin") {
            return send(sender, "general.no-permission", &[]);
        }
        if sub == "reload" {
            reload_all();
            return send(sender, "general.reloaded", &[]);
        }
        save_all();
        return send(sender, "general.saved", &[]);
    }
    if let Some(p) = sender.player() {
        if p.has_permission("keystone.admin") {
            return admin::open(&p);
        }
    }
    send(sender, "general.help", &[("version", env!("CARGO_PKG_VERSION"))]);
    if sender.player().is_some() {
        send(sender, "general.no-admin", &[]);
    }
}

// ---- events ----

fn on_join(p: &Player) {
    let id = uuid_of(p);
    let name = p.get_name();
    let entity = p.as_entity().get_id();
    with(|s| {
        data::ensure(s, &id, &name);
        if let Some(d) = s.players.get_mut(&id) {
            d.name = name.clone();
        }
        s.online.insert(id.clone(), core::Online { name: name.clone(), entity_id: entity });
    });
    roles::apply(p);
    auth::on_join(p);
    skins::on_join(p);
    sidebar::update(p);
    sidebar::refresh_teams();
}

fn on_quit(p: &Player) {
    let id = uuid_of(p);
    menu::on_quit(p);
    auth::on_quit(p);
    sidebar::quit(p);
    rewards::quit(&id);
    with(|s| {
        s.online.remove(&id);
        s.roles.applied.remove(&id);
    });
}

fn locked_entity(entity_id: i32) -> bool {
    core::online_players().iter().any(|p| p.as_entity().get_id() as i32 == entity_id && auth::is_locked(p))
}

macro_rules! handler {
    ($name:ident, $event:ty, |$server:ident, $data:ident| $body:block) => {
        struct $name;
        #[allow(unused_mut)]
        impl EventHandler<$event> for $name {
            fn handle(&self, $server: Server, mut $data: <$event as FromIntoEvent>::Data) -> <$event as FromIntoEvent>::Data {
                let _ = &$server;
                $body
                $data
            }
        }
    };
}

handler!(JoinH, PlayerJoinEvent, |_s, e| { on_join(&e.player); });
handler!(LeaveH, PlayerLeaveEvent, |_s, e| {
    on_quit(&e.player);
    // Players leaving changes the name tag teams of everyone else.
    pumpkin_plugin_api::scheduler::schedule_delayed_task(1, |_| sidebar::refresh_teams());
});
handler!(PreLoginH, AsyncPlayerPreLoginEvent, |_s, e| {
    if !e.cancelled {
        if let Some((key, kv)) = auth::pre_login(&e.player_name) {
            let kv: Vec<(&str, &str)> = kv.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            e.cancelled = true;
            e.kick_message = text::parse(&core::msg(&key, &kv));
        } else {
            skins::prefetch(&e.player_name, &e.player_uuid);
        }
    }
});
handler!(ChatH, PlayerChatEvent, |_s, e| {
    if menu::on_chat(&e.player, &e.message) {
        e.cancelled = true;
    } else if auth::is_locked(&e.player) {
        e.cancelled = true;
        send(&e.player, "auth.login-first", &[]);
    } else if !e.cancelled && core::module("chat") {
        let line = roles::chat_line(&e.player);
        let message = if e.player.has_permission("keystone.chat.color") { e.message.clone() } else { text::escape(&e.message) };
        let component_line = line.clone();
        e.cancelled = true;
        for r in &e.recipients {
            r.send_system_message(text::parse_with(&component_line, &[("message", &message)]), false);
        }
        log(&text::plain(&line.replace("<message>", &e.message)));
    }
});
handler!(CommandH, PlayerCommandPreprocessEvent, |_s, e| {
    if auth::is_locked(&e.player) && !auth::allowed_command(&e.command) {
        e.cancelled = true;
        send(&e.player, "auth.login-first", &[]);
    }
});
handler!(BreakH, BlockBreakEvent, |_s, e| {
    if let Some(p) = &e.player {
        if auth::is_locked(p) {
            e.cancelled = true;
        } else if !e.cancelled {
            rewards::on_break(p, &e.block, &e.block_pos);
        }
    }
});
handler!(PlaceH, BlockPlaceEvent, |_s, e| {
    if auth::is_locked(&e.player) {
        e.cancelled = true;
    } else if !e.cancelled {
        rewards::on_place(&e.block_pos);
    }
});
handler!(EntityDeathH, EntityDeathEvent, |_s, e| { rewards::on_entity_death(e.entity_id); });
handler!(PlayerDeathH, PlayerDeathEvent, |_s, e| { rewards::on_player_death(&e.player); });
handler!(ClickH, InventoryClickEvent, |_s, e| {
    if auth::is_locked(&e.player) || menu::on_click(&e.player, e.raw_slot, e.click_type) {
        e.cancelled = true;
    }
});
handler!(CloseH, InventoryCloseEvent, |_s, e| { menu::on_close(&e.player); });
handler!(DragH, InventoryDragEvent, |_s, e| {
    if auth::is_locked(&e.player) || (menu::is_open(&e.player) && !menu::is_drop_menu(&e.player)) {
        e.cancelled = true;
    }
});
handler!(OpenH, InventoryOpenEvent, |_s, e| { if auth::is_locked(&e.player) { e.cancelled = true; } });
handler!(MoveH, PlayerMoveEvent, |_s, e| { if auth::is_locked(&e.player) { e.cancelled = true; } });
handler!(InteractH, PlayerInteractEvent, |_s, e| { if auth::is_locked(&e.player) { e.cancelled = true; } });
handler!(DropH, PlayerDropItemEvent, |_s, e| { if auth::is_locked(&e.player) { e.cancelled = true; } });
handler!(SwapH, PlayerSwapHandsEvent, |_s, e| { if auth::is_locked(&e.player) { e.cancelled = true; } });
handler!(DamageH, EntityDamageEvent, |_s, e| { if locked_entity(e.entity_id) { e.cancelled = true; } });
handler!(HitH, EntityDamageByEntityEvent, |_s, e| {
    if locked_entity(e.damager_id) || locked_entity(e.entity_id) {
        e.cancelled = true;
    } else if !e.cancelled {
        rewards::on_hit(e.entity_id, e.damager_id);
    }
});
handler!(PickupH, EntityPickupItemEvent, |_s, e| { if locked_entity(e.entity_id) { e.cancelled = true; } });
handler!(WorldH, PlayerChangedWorldEvent, |_s, e| { sidebar::update(&e.player); });

fn register_events(ctx: &Context) -> pumpkin_plugin_api::Result<()> {
    use pumpkin_plugin_api::events::EventPriority::{High, Highest, Normal};
    ctx.register_event_handler::<PlayerJoinEvent, _>(JoinH, Highest, true)?;
    ctx.register_event_handler::<PlayerLeaveEvent, _>(LeaveH, Normal, true)?;
    ctx.register_event_handler::<AsyncPlayerPreLoginEvent, _>(PreLoginH, High, true)?;
    ctx.register_event_handler::<PlayerChatEvent, _>(ChatH, Highest, true)?;
    ctx.register_event_handler::<PlayerCommandPreprocessEvent, _>(CommandH, Highest, true)?;
    ctx.register_event_handler::<BlockBreakEvent, _>(BreakH, Normal, true)?;
    ctx.register_event_handler::<BlockPlaceEvent, _>(PlaceH, Normal, true)?;
    ctx.register_event_handler::<EntityDeathEvent, _>(EntityDeathH, Normal, true)?;
    ctx.register_event_handler::<PlayerDeathEvent, _>(PlayerDeathH, Normal, true)?;
    ctx.register_event_handler::<InventoryClickEvent, _>(ClickH, Highest, true)?;
    ctx.register_event_handler::<InventoryCloseEvent, _>(CloseH, Normal, true)?;
    ctx.register_event_handler::<InventoryDragEvent, _>(DragH, Highest, true)?;
    ctx.register_event_handler::<InventoryOpenEvent, _>(OpenH, Highest, true)?;
    ctx.register_event_handler::<PlayerMoveEvent, _>(MoveH, Highest, true)?;
    ctx.register_event_handler::<PlayerInteractEvent, _>(InteractH, Highest, true)?;
    ctx.register_event_handler::<PlayerDropItemEvent, _>(DropH, Highest, true)?;
    ctx.register_event_handler::<PlayerSwapHandsEvent, _>(SwapH, Highest, true)?;
    ctx.register_event_handler::<EntityDamageEvent, _>(DamageH, Highest, true)?;
    ctx.register_event_handler::<EntityDamageByEntityEvent, _>(HitH, Highest, true)?;
    ctx.register_event_handler::<EntityPickupItemEvent, _>(PickupH, Highest, true)?;
    ctx.register_event_handler::<PlayerChangedWorldEvent, _>(WorldH, Normal, true)?;
    Ok(())
}
