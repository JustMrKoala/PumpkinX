//! Dashboard accounts: usernames, Argon2 password hashes, sessions and a limit on login attempts.
//!
//! The first account needs the setup token from `pumpkinx.toml`. Without it, the first visitor
//! of a public dashboard could claim the server.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use axum::{
    Extension, Json,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiResult, AppState};
use crate::config::{DashboardUser, generate_token};

const SESSION_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// Failed logins allowed per address, and per username from all addresses together.
const MAX_FAILURES_PER_ADDRESS: u32 = 5;
const MAX_FAILURES_PER_USER: u32 = 20;
const MIN_PASSWORD_LENGTH: usize = 8;
const MAX_PASSWORD_LENGTH: usize = 128;

/// Verified when the username doesn't exist, so a login takes equally long either way.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("no such account").unwrap_or_default());

struct Session {
    username: String,
    expires: Instant,
}

struct Failures {
    count: u32,
    since: Instant,
}

#[derive(Default)]
pub(super) struct AuthState {
    sessions: Mutex<HashMap<String, Session>>,
    failures: Mutex<HashMap<String, Failures>>,
}

impl AuthState {
    fn start_session(&self, username: &str) -> String {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        sessions.retain(|_, s| s.expires > now);
        let id = generate_token();
        sessions.insert(
            id.clone(),
            Session {
                username: username.to_string(),
                expires: now + SESSION_LIFETIME,
            },
        );
        id
    }

    fn user_of(&self, id: &str) -> Option<String> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions
            .get(id)
            .filter(|s| s.expires > Instant::now())
            .map(|s| s.username.clone())
    }

    fn end_session(&self, id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    fn end_sessions_of(&self, username: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, s| !s.username.eq_ignore_ascii_case(username));
    }

    fn is_blocked(&self, key: &str, limit: u32) -> bool {
        let failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        failures
            .get(key)
            .is_some_and(|f| f.count >= limit && f.since.elapsed() < FAILURE_WINDOW)
    }

    fn record_failure(&self, key: &str) {
        let mut failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        if failures.len() > 10_000 {
            failures.retain(|_, f| f.since.elapsed() < FAILURE_WINDOW);
        }
        let entry = failures.entry(key.to_string()).or_insert(Failures {
            count: 0,
            since: Instant::now(),
        });
        if entry.since.elapsed() >= FAILURE_WINDOW {
            entry.count = 0;
            entry.since = Instant::now();
        }
        entry.count += 1;
    }

    fn clear_failures(&self, key: &str) {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }
}

#[derive(Clone)]
pub(super) struct CurrentUser(pub String);

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Argon2 is deliberately slow, so it stays off the async threads.
async fn hash_off_thread(password: String) -> Result<String, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| e.to_string())
        .and_then(|result| result)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn check_username(name: &str) -> Result<(), (StatusCode, String)> {
    let valid = (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
    if valid {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "Usernames are 1 to 32 letters, digits, dots, dashes or underscores".to_string(),
        ))
    }
}

fn check_password(password: &str) -> Result<(), (StatusCode, String)> {
    if (MIN_PASSWORD_LENGTH..=MAX_PASSWORD_LENGTH).contains(&password.chars().count()) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            format!("Passwords are {MIN_PASSWORD_LENGTH} to {MAX_PASSWORD_LENGTH} characters long"),
        ))
    }
}

/// Cloudflare sets this header on everything that comes through the tunnel.
fn address_key(headers: &HeaderMap) -> String {
    let address = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("direct");
    format!("address:{address}")
}

fn too_many_attempts() -> (StatusCode, String) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        "Too many failed attempts. Wait a few minutes and try again.".to_string(),
    )
}

fn edit_users(
    state: &AppState,
    edit: impl FnOnce(&mut Vec<DashboardUser>, &mut String) -> Result<(), (StatusCode, String)>,
) -> Result<(), (StatusCode, String)> {
    let mut saved = state
        .px
        .saved
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    let dashboard = &mut saved.dashboard;
    edit(&mut dashboard.users, &mut dashboard.token)?;
    saved
        .save(&state.px.dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn user_hash(state: &AppState, username: &str) -> Option<String> {
    state
        .px
        .saved
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .dashboard
        .users
        .iter()
        .find(|u| u.username.eq_ignore_ascii_case(username))
        .map(|u| u.password_hash.clone())
}

/// Lets a request through only with a valid session. Changes are logged with the account name.
pub(super) async fn require_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let username = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|id| state.auth.user_of(id));
    let Some(username) = username else {
        return (StatusCode::UNAUTHORIZED, "Please sign in").into_response();
    };
    if request.method() != Method::GET {
        tracing::info!(
            "[dashboard] {username}: {} {}",
            request.method(),
            request.uri().path()
        );
    }
    request.extensions_mut().insert(CurrentUser(username));
    next.run(request).await
}

pub(super) async fn auth_state(State(state): State<AppState>) -> Json<Value> {
    let saved = state
        .px
        .saved
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    Json(json!({ "needs_setup": saved.dashboard.users.is_empty() }))
}

#[derive(Deserialize)]
pub(super) struct SetupBody {
    token: String,
    username: String,
    password: String,
}

pub(super) async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetupBody>,
) -> ApiResult {
    let address = address_key(&headers);
    if state.auth.is_blocked(&address, MAX_FAILURES_PER_ADDRESS) {
        return Err(too_many_attempts());
    }
    let expected = {
        let saved = state
            .px
            .saved
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        if !saved.dashboard.users.is_empty() {
            return Err((StatusCode::CONFLICT, "Setup is already done".to_string()));
        }
        saved.dashboard.token.clone()
    };
    if expected.is_empty() || !constant_time_eq(body.token.trim().as_bytes(), expected.as_bytes()) {
        state.auth.record_failure(&address);
        return Err((StatusCode::UNAUTHORIZED, "Wrong setup token".to_string()));
    }
    check_username(&body.username)?;
    check_password(&body.password)?;
    let password_hash = hash_off_thread(body.password).await?;
    let username = body.username.clone();
    edit_users(&state, move |users, token| {
        if !users.is_empty() {
            return Err((StatusCode::CONFLICT, "Setup is already done".to_string()));
        }
        users.push(DashboardUser {
            username,
            password_hash,
        });
        // The setup token has done its job.
        token.clear();
        Ok(())
    })?;
    state.auth.clear_failures(&address);
    tracing::info!("[dashboard] first account created: {}", body.username);
    let session = state.auth.start_session(&body.username);
    Ok(Json(json!({ "session": session, "username": body.username })))
}

#[derive(Deserialize)]
pub(super) struct LoginBody {
    username: String,
    password: String,
}

pub(super) async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> ApiResult {
    let address = address_key(&headers);
    let user_key = format!("user:{}", body.username.to_lowercase());
    if state.auth.is_blocked(&address, MAX_FAILURES_PER_ADDRESS)
        || state.auth.is_blocked(&user_key, MAX_FAILURES_PER_USER)
    {
        return Err(too_many_attempts());
    }
    let hash = user_hash(&state, &body.username);
    let known = hash.is_some();
    let hash = hash.unwrap_or_else(|| DUMMY_HASH.clone());
    let password = body.password;
    let matches = tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false);
    if !known || !matches {
        state.auth.record_failure(&address);
        state.auth.record_failure(&user_key);
        tracing::warn!("[dashboard] failed login for {:?}", body.username);
        return Err((
            StatusCode::UNAUTHORIZED,
            "Wrong username or password".to_string(),
        ));
    }
    state.auth.clear_failures(&address);
    state.auth.clear_failures(&user_key);
    let session = state.auth.start_session(&body.username);
    Ok(Json(json!({ "session": session, "username": body.username })))
}

pub(super) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    if let Some(id) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        state.auth.end_session(id);
    }
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_users(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentUser>,
) -> ApiResult {
    let saved = state
        .px
        .saved
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    let users: Vec<&str> = saved
        .dashboard
        .users
        .iter()
        .map(|u| u.username.as_str())
        .collect();
    Ok(Json(json!({ "users": users, "current": current.0 })))
}

#[derive(Deserialize)]
pub(super) struct UserBody {
    username: String,
    #[serde(default)]
    password: String,
}

pub(super) async fn add_user(State(state): State<AppState>, Json(body): Json<UserBody>) -> ApiResult {
    check_username(&body.username)?;
    check_password(&body.password)?;
    let password_hash = hash_off_thread(body.password).await?;
    edit_users(&state, move |users, _| {
        if users
            .iter()
            .any(|u| u.username.eq_ignore_ascii_case(&body.username))
        {
            return Err((StatusCode::CONFLICT, "That username is taken".to_string()));
        }
        users.push(DashboardUser {
            username: body.username,
            password_hash,
        });
        Ok(())
    })?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn set_password(
    State(state): State<AppState>,
    Json(body): Json<UserBody>,
) -> ApiResult {
    check_password(&body.password)?;
    let password_hash = hash_off_thread(body.password).await?;
    let username = body.username.clone();
    edit_users(&state, move |users, _| {
        let user = users
            .iter_mut()
            .find(|u| u.username.eq_ignore_ascii_case(&username))
            .ok_or((StatusCode::NOT_FOUND, "No such account".to_string()))?;
        user.password_hash = password_hash;
        Ok(())
    })?;
    // Whoever knew the old password must not stay signed in.
    state.auth.end_sessions_of(&body.username);
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_user(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentUser>,
    Json(body): Json<UserBody>,
) -> ApiResult {
    if body.username.eq_ignore_ascii_case(&current.0) {
        return Err((
            StatusCode::BAD_REQUEST,
            "You can't remove your own account".to_string(),
        ));
    }
    let username = body.username.clone();
    edit_users(&state, move |users, _| {
        let before = users.len();
        users.retain(|u| !u.username.eq_ignore_ascii_case(&username));
        if users.len() == before {
            return Err((StatusCode::NOT_FOUND, "No such account".to_string()));
        }
        Ok(())
    })?;
    state.auth.end_sessions_of(&body.username);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_round_trip() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong horse", &hash));
        assert!(!verify_password("correct horse", "not a hash"));
    }

    #[test]
    fn failures_block_after_the_limit() {
        let auth = AuthState::default();
        for _ in 0..MAX_FAILURES_PER_ADDRESS {
            assert!(!auth.is_blocked("a", MAX_FAILURES_PER_ADDRESS));
            auth.record_failure("a");
        }
        assert!(auth.is_blocked("a", MAX_FAILURES_PER_ADDRESS));
        assert!(!auth.is_blocked("b", MAX_FAILURES_PER_ADDRESS));
        auth.clear_failures("a");
        assert!(!auth.is_blocked("a", MAX_FAILURES_PER_ADDRESS));
    }

    #[test]
    fn sessions_end() {
        let auth = AuthState::default();
        let id = auth.start_session("alice");
        assert_eq!(auth.user_of(&id).as_deref(), Some("alice"));
        auth.end_sessions_of("ALICE");
        assert_eq!(auth.user_of(&id), None);
        assert_eq!(auth.user_of("made up"), None);
    }

    #[test]
    fn username_and_password_rules() {
        assert!(check_username("agathe_1").is_ok());
        assert!(check_username("").is_err());
        assert!(check_username("a b").is_err());
        assert!(check_username("a\nb").is_err());
        assert!(check_password("short").is_err());
        assert!(check_password("long enough").is_ok());
    }
}
