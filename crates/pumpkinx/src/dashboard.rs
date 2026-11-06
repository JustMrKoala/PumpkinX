//! The web dashboard: a JSON API plus a single page app embedded in the binary.

use std::{
    fs,
    sync::{Arc, LazyLock, Mutex},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pumpkin_config::PumpkinConfig;
use pumpkin_core::{command::CommandSender, server::Server};
use pumpkin_data::packet::CURRENT_MC_VERSION;
use serde::Deserialize;
use serde_json::{Value, json};
use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::{PumpkinX, config::PumpkinXConfig, keystone, multiversion, patchbukkit};

mod auth;
mod files;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const APP_CSS: &str = include_str!("../assets/app.css");
const LOGO_SVG: &str = include_str!("../assets/logo.svg");

/// The page, with its script and stylesheet links tied to their content. Cloudflare replaces
/// our `no-cache` with a browser cache of hours, and a stale script next to a fresh page
/// silently drops settings it doesn't know. A new build now means a new URL.
static INDEX_PAGE: LazyLock<String> = LazyLock::new(|| versioned_page(INDEX_HTML, APP_JS, APP_CSS));

fn content_version(body: &str) -> u64 {
    body.bytes().fold(0xcbf2_9ce4_8422_2325, |hash: u64, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn versioned_page(page: &str, script: &str, style: &str) -> String {
    page.replace(
        "\"/app.js\"",
        &format!("\"/app.js?v={:x}\"", content_version(script)),
    )
    .replace(
        "\"/app.css\"",
        &format!("\"/app.css?v={:x}\"", content_version(style)),
    )
}

const PUMPKIN_TOML: &str = "pumpkin.toml";
const ICON_FILE: &str = "icon.png";

#[derive(Clone)]
struct AppState {
    px: Arc<PumpkinX>,
    server: Arc<Server>,
    system: Arc<Mutex<Option<System>>>,
    auth: Arc<auth::AuthState>,
}

type ApiResult = Result<Json<Value>, (StatusCode, String)>;

fn bad_request(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

/// Binds the dashboard and serves it on the server's runtime until shutdown.
pub async fn spawn(px: Arc<PumpkinX>, server: Arc<Server>) {
    let address = px.running.dashboard.address.clone();
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("Dashboard couldn't listen on {address}: {err}");
            return;
        }
    };
    let state = AppState {
        px: px.clone(),
        server: server.clone(),
        system: Arc::new(Mutex::new(System::new().ok())),
        auth: Arc::default(),
    };

    let api = Router::new()
        .route("/status", get(status))
        .route("/players", get(players))
        .route("/plugins", get(plugins))
        .route("/command", post(command))
        .route("/logs", get(logs))
        .route("/config/pumpkin", get(get_pumpkin_config).put(put_pumpkin_config))
        .route("/config/pumpkinx", get(get_pumpkinx_config).put(put_pumpkinx_config))
        .route("/server/stop", post(stop))
        .route("/server/icon", get(get_icon).put(put_icon))
        .route("/users", get(auth::list_users).post(auth::add_user))
        .route("/users/password", post(auth::set_password))
        .route("/users/delete", post(auth::delete_user))
        .route("/auth/logout", post(auth::logout))
        .merge(files::routes(state.clone()))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ))
        .with_state(state.clone());

    let public = Router::new()
        .route("/auth/state", get(auth::auth_state))
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login))
        .with_state(state);

    let app = Router::new()
        .route("/", get(|| async { asset(INDEX_PAGE.as_str(), "text/html; charset=utf-8") }))
        .route("/app.js", get(|| async { asset(APP_JS, "text/javascript; charset=utf-8") }))
        .route("/app.css", get(|| async { asset(APP_CSS, "text/css; charset=utf-8") }))
        .route("/logo.svg", get(|| async { asset(LOGO_SVG, "image/svg+xml") }))
        .nest("/api", api.merge(public));

    tracing::info!("Dashboard running on http://{address}");
    if px
        .saved
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .dashboard
        .users
        .is_empty()
    {
        tracing::warn!(
            "No dashboard accounts yet. Open the dashboard and use the setup token from {} to create the first one.",
            crate::config::FILE_NAME
        );
    }
    let stop = server.stop_token.clone();
    server.spawn_task(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
        if let Err(err) = result {
            tracing::error!("Dashboard stopped: {err}");
        }
    });
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

async fn status(State(state): State<AppState>) -> ApiResult {
    let server = &state.server;
    let avg_nanos = server.get_average_tick_time_nanos();
    #[expect(clippy::cast_precision_loss)]
    let mspt = avg_nanos as f64 / 1_000_000.0;
    let tps = if mspt <= 50.0 { 20.0 } else { 1000.0 / mspt };

    let (rss, cpu, total_memory) = {
        let mut system = state
            .system
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        system.as_mut().map_or((0, 0.0, 0), |system| {
            let pid = Pid::from_u32(std::process::id());
            system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            system.refresh_memory();
            let total = system.total_memory();
            system
                .process(pid)
                .map_or((0, 0.0, total), |p| (p.memory(), p.cpu_usage(), total))
        })
    };

    let worlds: Vec<Value> = server
        .worlds
        .load()
        .iter()
        .map(|world| {
            json!({
                "name": world.dimension.minecraft_name,
                "players": world.players.load().len(),
            })
        })
        .collect();

    let java = patchbukkit::detect_java();
    let saved = state
        .px
        .saved
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    Ok(Json(json!({
        "pumpkinx_version": crate::VERSION,
        "minecraft_version": CURRENT_MC_VERSION.to_string(),
        "protocol": CURRENT_MC_VERSION.protocol_version(),
        "motd": server.advanced_config.networking.java.motd,
        "uptime_secs": state.px.started.elapsed().as_secs(),
        "tps": tps,
        "mspt": mspt,
        "players_online": server.get_player_count(),
        "max_players": server.advanced_config.networking.java.max_players,
        "memory_bytes": rss,
        "total_memory_bytes": total_memory,
        "cpu_percent": cpu,
        "worlds": worlds,
        "multiversion": {
            "running": state.px.running.multiversion.enabled,
            "saved": saved.multiversion.enabled,
            "range": multiversion::SUPPORTED_RANGE,
        },
        "keystone": {
            "embedded": keystone::embedded(),
            "running": state.px.running.keystone.enabled && keystone::embedded(),
            "saved": saved.keystone.enabled,
        },
        "patchbukkit": {
            "compiled_in": patchbukkit::compiled_in(),
            "running": state.px.running.patchbukkit.enabled,
            "saved": saved.patchbukkit.enabled,
            "java": java,
            "plugin_dir": patchbukkit::PLUGIN_DIR,
            "jars": patchbukkit::plugin_jars(),
        },
    })))
}

async fn players(State(state): State<AppState>) -> ApiResult {
    let list: Vec<Value> = state
        .server
        .get_all_players()
        .iter()
        .map(|player| {
            let position = player.position();
            json!({
                "name": player.gameprofile.name,
                "uuid": player.gameprofile.id.to_string(),
                "ping": player.ping.load(std::sync::atomic::Ordering::Relaxed),
                "version": player.client.java_version().to_string(),
                "world": player.world().dimension.minecraft_name,
                "gamemode": format!("{:?}", player.gamemode.load()).to_lowercase(),
                "position": [position.x.round(), position.y.round(), position.z.round()],
            })
        })
        .collect();
    Ok(Json(Value::Array(list)))
}

async fn plugins(State(state): State<AppState>) -> ApiResult {
    let manager = &state.server.plugin_manager;
    let loaded: Vec<Value> = manager
        .loaded_plugins()
        .into_iter()
        // Built-ins are switched on the PumpkinX features page, not listed as plugins.
        .filter(|m| m.name != "patchbukkit" && m.name != keystone::PLUGIN_NAME)
        .map(|m| {
            json!({
                "name": m.name,
                "version": m.version,
                "authors": m.authors,
                "description": m.description,
                "active": manager.is_plugin_active(&m.name),
            })
        })
        .collect();
    let failed: Vec<Value> = manager
        .get_failed_plugins()
        .await
        .into_iter()
        .map(|(name, error)| json!({ "name": name, "error": error }))
        .collect();
    Ok(Json(json!({ "loaded": loaded, "failed": failed })))
}

#[derive(Deserialize)]
struct CommandBody {
    command: String,
}

async fn command(State(state): State<AppState>, Json(body): Json<CommandBody>) -> ApiResult {
    let line = body.command.trim().trim_start_matches('/').to_string();
    if line.is_empty() {
        return Err(bad_request("empty command"));
    }
    tracing::info!("[dashboard] > {line}");
    let server = state.server.clone();
    let mut event = pumpkin_core::plugin::server::server_command::ServerCommandEvent::new(line.clone());
    server.plugin_manager.fire(&server, &mut event).await;
    if !event.cancelled {
        server
            .command_dispatcher
            .load()
            .handle_command(&CommandSender::Console.into_source(&server), &line);
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    after: u64,
}

async fn logs(Query(query): Query<LogQuery>) -> ApiResult {
    Ok(Json(json!(pumpkin_core::pumpkinx::recent_logs(query.after))))
}

fn read_toml(path: &std::path::Path) -> Result<toml::Value, (StatusCode, String)> {
    let text = fs::read_to_string(path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", path.display())))?;
    toml::from_str(&text).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn get_pumpkin_config(State(state): State<AppState>) -> ApiResult {
    let value = read_toml(&state.px.dir.join(PUMPKIN_TOML))?;
    Ok(Json(serde_json::to_value(value).map_err(|e| bad_request(e.to_string()))?))
}

/// JSON has one number type, TOML two. Keeps floats as floats where the file had a float.
fn coerce_like(new: Value, old: Option<&toml::Value>) -> Result<toml::Value, String> {
    Ok(match (new, old) {
        (Value::Object(map), old) => {
            let old_table = old.and_then(toml::Value::as_table);
            let mut table = toml::Table::new();
            for (key, value) in map {
                let old_value = old_table.and_then(|t| t.get(&key));
                table.insert(key, coerce_like(value, old_value)?);
            }
            toml::Value::Table(table)
        }
        (Value::Array(items), old) => {
            let old_first = old.and_then(toml::Value::as_array).and_then(|a| a.first());
            toml::Value::Array(
                items
                    .into_iter()
                    .map(|v| coerce_like(v, old_first))
                    .collect::<Result<_, _>>()?,
            )
        }
        (Value::Number(n), Some(toml::Value::Float(_))) => {
            toml::Value::Float(n.as_f64().ok_or("number out of range")?)
        }
        (Value::Number(n), _) => n.as_i64().map_or_else(
            || n.as_f64().map(toml::Value::Float).ok_or("number out of range"),
            |i| Ok(toml::Value::Integer(i)),
        )?,
        (Value::Bool(b), _) => toml::Value::Boolean(b),
        (Value::String(s), _) => toml::Value::String(s),
        (Value::Null, _) => return Err("null values are not allowed".to_string()),
    })
}

/// Checks that Pumpkin would accept the config, so a bad save can't stop the next start.
fn check_pumpkin_config(text: &str) -> Result<(), String> {
    let config: PumpkinConfig = toml::from_str(text).map_err(|e| e.to_string())?;
    let java = &config.advanced.networking.java;
    if java.keep_alive_time == 0 {
        return Err("Java keep alive time must be greater than 0".into());
    }
    if !(2..=64).contains(&java.view_distance.get()) {
        return Err("Java view distance must be between 2 and 64".into());
    }
    if java.online_mode && !java.encryption {
        return Err("Online mode needs encryption turned on".into());
    }
    Ok(())
}

async fn put_pumpkin_config(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult {
    let path = state.px.dir.join(PUMPKIN_TOML);
    let old = read_toml(&path)?;
    let new = coerce_like(body, Some(&old)).map_err(bad_request)?;
    let text = toml::to_string(&new).map_err(|e| bad_request(e.to_string()))?;
    check_pumpkin_config(&text).map_err(bad_request)?;
    let backup = state.px.dir.join("pumpkin.toml.bak");
    let _ = fs::copy(&path, backup);
    fs::write(&path, text).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!("[dashboard] pumpkin.toml saved; restart to apply");
    Ok(Json(json!({ "ok": true, "restart_required": true })))
}

async fn get_pumpkinx_config(State(state): State<AppState>) -> ApiResult {
    let mut config = state
        .px
        .saved
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    // The setup token and password hashes never leave the server through the API.
    config.dashboard.token = String::new();
    config.dashboard.users = Vec::new();
    Ok(Json(serde_json::to_value(config).map_err(|e| bad_request(e.to_string()))?))
}

async fn put_pumpkinx_config(
    State(state): State<AppState>,
    Json(mut body): Json<PumpkinXConfig>,
) -> ApiResult {
    let mut saved = state
        .px
        .saved
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    body.dashboard.token.clone_from(&saved.dashboard.token);
    body.dashboard.users.clone_from(&saved.dashboard.users);
    if body.dashboard.address.parse::<std::net::SocketAddr>().is_err() {
        return Err(bad_request("dashboard address must look like 0.0.0.0:8080"));
    }
    body.save(&state.px.dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    *saved = body;
    drop(saved);
    tracing::info!("[dashboard] pumpkinx.toml saved; restart to apply");
    Ok(Json(json!({ "ok": true, "restart_required": true })))
}

async fn get_icon(State(state): State<AppState>) -> Result<Response, (StatusCode, String)> {
    let bytes = fs::read(state.px.dir.join(ICON_FILE))
        .map_err(|_| (StatusCode::NOT_FOUND, "No custom icon".to_string()))?;
    Ok((
        [(header::CONTENT_TYPE, HeaderValue::from_static("image/png"))],
        bytes,
    )
        .into_response())
}

fn is_64px_png(bytes: &[u8]) -> bool {
    bytes.len() >= 24
        && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes[16..20] == 64u32.to_be_bytes()
        && bytes[20..24] == 64u32.to_be_bytes()
}

/// Stores the server list icon and points `pumpkin.toml` at it.
async fn put_icon(State(state): State<AppState>, body: Bytes) -> ApiResult {
    if !is_64px_png(&body) {
        return Err(bad_request("The icon must be a 64 by 64 PNG"));
    }
    let path = state.px.dir.join(PUMPKIN_TOML);
    let mut config = read_toml(&path)?;
    let table = config
        .as_table_mut()
        .ok_or_else(|| bad_request("pumpkin.toml has an unexpected shape"))?;
    table.insert("use_favicon".to_string(), toml::Value::Boolean(true));
    table.insert(
        "favicon_path".to_string(),
        toml::Value::String(ICON_FILE.to_string()),
    );
    let text = toml::to_string(&config).map_err(|e| bad_request(e.to_string()))?;
    fs::write(state.px.dir.join(ICON_FILE), &body)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    fs::write(&path, text).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .server
        .get_status()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_favicon(&body);
    Ok(Json(json!({ "ok": true, "restart_required": false })))
}

async fn stop() -> ApiResult {
    tracing::warn!("[dashboard] stop requested");
    pumpkin_core::stop_server();
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod asset_tests {
    use super::{APP_CSS, APP_JS, INDEX_HTML, content_version, versioned_page};

    #[test]
    fn page_links_change_with_their_content() {
        let page = versioned_page(INDEX_HTML, APP_JS, APP_CSS);
        assert!(page.contains(&format!("/app.js?v={:x}", content_version(APP_JS))));
        assert!(page.contains(&format!("/app.css?v={:x}", content_version(APP_CSS))));
        assert_ne!(page, versioned_page(INDEX_HTML, "changed", APP_CSS));
    }
}
