//! File manager API. Everything is confined to the server directory: paths are resolved
//! through the real filesystem, so `..` and symbolic links can't reach anything outside it.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use futures::StreamExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;

use super::AppState;

/// Secrets. Hidden from listings and refused by every operation.
const PROTECTED: &[&str] = &["pumpkinx.toml", "nethernet-key.der"];
/// Readable, but changed through the settings page so its validation can't be skipped.
const READ_ONLY: &[&str] = &["pumpkin.toml"];
/// Deleted files land here. Deleting inside it is permanent.
const TRASH: &str = ".pumpkinx-trash";
/// Largest file the text editor opens.
const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;

type ApiError = (StatusCode, String);
type ApiResult = Result<Json<Value>, ApiError>;

fn error(status: StatusCode, message: &str) -> ApiError {
    (status, message.to_string())
}

fn io_error(err: &io::Error) -> ApiError {
    let status = match err.kind() {
        io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, err.to_string())
}

pub(super) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/files", get(list).delete(delete))
        .route("/files/read", get(read))
        .route(
            "/files/write",
            put(write).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/files/upload",
            put(upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/files/download", get(download))
        .route("/files/mkdir", post(mkdir))
        .route("/files/rename", post(rename))
        .route_layer(middleware::from_fn_with_state(state, require_enabled))
}

async fn require_enabled(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if state.px.running.dashboard.file_manager {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, "The file manager is turned off").into_response()
    }
}

/// A path that has been checked to be inside the server directory.
#[derive(Debug)]
struct Target {
    full: PathBuf,
    /// Relative to the server directory, with `/` separators. Empty for the directory itself.
    rel: String,
    is_symlink: bool,
}

impl Target {
    fn file_name(&self) -> String {
        self.rel.rsplit('/').next().unwrap_or_default().to_string()
    }

    fn matches(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.rel.eq_ignore_ascii_case(n))
    }

    fn in_trash(&self) -> bool {
        let lower = self.rel.to_ascii_lowercase();
        lower == TRASH || lower.starts_with(&format!("{TRASH}/"))
    }

    /// Refuses anything that must not be changed from the dashboard.
    fn ensure_mutable(&self) -> Result<(), ApiError> {
        if self.rel.is_empty() {
            return Err(error(StatusCode::BAD_REQUEST, "That is the server folder itself"));
        }
        if self.is_symlink {
            return Err(error(StatusCode::FORBIDDEN, "Symbolic links can't be changed here"));
        }
        if self.matches(READ_ONLY) {
            return Err(error(StatusCode::FORBIDDEN, "Change this file on the settings page"));
        }
        Ok(())
    }
}

/// Splits a client path into plain names, refusing anything that could step outside.
fn clean_components(rel: &str) -> Result<Vec<String>, ApiError> {
    let invalid = || error(StatusCode::BAD_REQUEST, "Invalid path");
    let normalized = rel.replace('\\', "/");
    let mut names = Vec::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(invalid)?;
                // `:` opens Windows alternate data streams, and Windows ignores trailing dots
                // and spaces, so `pumpkinx.toml.` would otherwise be the protected file.
                if name.contains(':')
                    || name.chars().any(char::is_control)
                    || name.ends_with('.')
                    || name.ends_with(' ')
                {
                    return Err(invalid());
                }
                names.push(name.to_string());
            }
            _ => return Err(invalid()),
        }
    }
    Ok(names)
}

fn resolve(root: &Path, rel: &str) -> Result<Target, ApiError> {
    let root = root.canonicalize().map_err(|e| io_error(&e))?;
    let names = clean_components(rel)?;
    let mut probe = root.clone();
    probe.extend(&names);
    let is_symlink = fs::symlink_metadata(&probe).is_ok_and(|m| m.file_type().is_symlink());

    // Canonicalize the deepest part that exists, which follows every symbolic link in it.
    let mut missing = Vec::new();
    let real = loop {
        match probe.canonicalize() {
            Ok(real) => break real,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // A dangling link: writing through it would create a file outside the jail.
                if fs::symlink_metadata(&probe).is_ok() {
                    return Err(error(StatusCode::FORBIDDEN, "Path is outside the server folder"));
                }
                let Some(name) = probe.file_name() else {
                    return Err(io_error(&err));
                };
                missing.push(name.to_os_string());
                probe.pop();
            }
            Err(err) => return Err(io_error(&err)),
        }
    };
    if !real.starts_with(&root) {
        return Err(error(StatusCode::FORBIDDEN, "Path is outside the server folder"));
    }
    let mut full = real;
    full.extend(missing.iter().rev());

    let rel = full
        .strip_prefix(&root)
        .map_err(|_| error(StatusCode::FORBIDDEN, "Path is outside the server folder"))?
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let target = Target {
        full,
        rel,
        is_symlink,
    };
    if target.matches(PROTECTED) {
        return Err(error(StatusCode::FORBIDDEN, "This file is protected"));
    }
    Ok(target)
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Writes through a temporary file, so a failed write can't leave half a file behind.
fn write_atomic(target: &Target, data: &[u8]) -> Result<(), ApiError> {
    let tmp = target
        .full
        .with_file_name(format!(".{}.pumpkinx-tmp", target.file_name()));
    let existing = fs::metadata(&target.full).ok();
    fs::write(&tmp, data).map_err(|e| io_error(&e))?;
    if let Some(metadata) = existing {
        let _ = fs::set_permissions(&tmp, metadata.permissions());
    }
    fs::rename(&tmp, &target.full).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_error(&e)
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

async fn list(State(state): State<AppState>, Query(query): Query<PathQuery>) -> ApiResult {
    let root = state.px.dir.clone();
    blocking(move || {
        let target = resolve(&root, &query.path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(&target.full).map_err(|e| io_error(&e))? {
            let entry = entry.map_err(|e| io_error(&e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if target.rel.is_empty() && PROTECTED.iter().any(|p| name.eq_ignore_ascii_case(p)) {
                continue;
            }
            let Ok(link) = entry.metadata() else { continue };
            let metadata = fs::metadata(entry.path()).unwrap_or_else(|_| link.clone());
            entries.push(json!({
                "name": name,
                "is_dir": metadata.is_dir(),
                "is_symlink": link.file_type().is_symlink(),
                "size": metadata.len(),
                "modified": modified_ms(&metadata),
                "read_only": target.rel.is_empty()
                    && READ_ONLY.iter().any(|p| name.eq_ignore_ascii_case(p)),
            }));
        }
        entries.sort_by_key(|e| {
            (
                !e["is_dir"].as_bool().unwrap_or(false),
                e["name"].as_str().unwrap_or_default().to_lowercase(),
            )
        });
        Ok(Json(json!({
            "path": target.rel,
            "in_trash": target.in_trash(),
            "entries": entries,
        })))
    })
    .await
}

async fn read(State(state): State<AppState>, Query(query): Query<PathQuery>) -> ApiResult {
    let root = state.px.dir.clone();
    blocking(move || {
        let target = resolve(&root, &query.path)?;
        let metadata = fs::metadata(&target.full).map_err(|e| io_error(&e))?;
        if metadata.is_dir() {
            return Err(error(StatusCode::BAD_REQUEST, "That is a folder"));
        }
        if metadata.len() > MAX_EDIT_BYTES {
            return Err(error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Too large for the editor, download it instead",
            ));
        }
        let bytes = fs::read(&target.full).map_err(|e| io_error(&e))?;
        let not_text = || {
            error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Not a text file, download it instead",
            )
        };
        // NUL bytes are valid UTF-8, but no text file has them.
        if bytes.contains(&0) {
            return Err(not_text());
        }
        let content = String::from_utf8(bytes).map_err(|_| not_text())?;
        Ok(Json(json!({
            "content": content,
            "modified": modified_ms(&metadata),
            "read_only": target.matches(READ_ONLY) || target.is_symlink,
        })))
    })
    .await
}

#[derive(Deserialize)]
struct WriteBody {
    path: String,
    content: String,
    /// The `modified` value from the read. A mismatch means someone else saved in between.
    modified: Option<u64>,
}

async fn write(State(state): State<AppState>, Json(body): Json<WriteBody>) -> ApiResult {
    let root = state.px.dir.clone();
    blocking(move || {
        let target = resolve(&root, &body.path)?;
        target.ensure_mutable()?;
        if let (Some(expected), Ok(metadata)) = (body.modified, fs::metadata(&target.full))
            && modified_ms(&metadata) != expected
        {
            return Err(error(
                StatusCode::CONFLICT,
                "The file changed since you opened it. Reopen it to see the new version.",
            ));
        }
        write_atomic(&target, body.content.as_bytes())?;
        let modified = fs::metadata(&target.full).map_or(0, |m| modified_ms(&m));
        tracing::info!("[dashboard] file saved: {}", target.rel);
        Ok(Json(json!({ "ok": true, "modified": modified })))
    })
    .await
}

#[derive(Deserialize)]
struct UploadQuery {
    path: String,
    #[serde(default)]
    overwrite: bool,
    /// Where this piece starts. Files arrive in pieces because Cloudflare limits request size.
    #[serde(default)]
    offset: u64,
    /// Size of the whole file. Without it, this request is the whole file.
    total: Option<u64>,
}

/// Streams a piece to a temporary file, which takes the real name once it is complete.
async fn upload(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> ApiResult {
    let root = state.px.dir.clone();
    let path = query.path.clone();
    let (overwrite, offset) = (query.overwrite, query.offset);
    let target = blocking(move || {
        let target = resolve(&root, &path)?;
        target.ensure_mutable()?;
        if target.full.is_dir() {
            return Err(error(StatusCode::CONFLICT, "A folder has that name"));
        }
        if offset == 0 && target.full.exists() && !overwrite {
            return Err(error(StatusCode::CONFLICT, "A file with that name already exists"));
        }
        Ok(target)
    })
    .await?;

    let tmp = target
        .full
        .with_file_name(format!(".{}.pumpkinx-upload", target.file_name()));
    let mut file = if offset == 0 {
        tokio::fs::File::create(&tmp).await
    } else {
        tokio::fs::OpenOptions::new().append(true).open(&tmp).await
    }
    .map_err(|e| io_error(&e))?;
    let received = file.metadata().await.map_err(|e| io_error(&e))?.len();
    if received != offset {
        return Err(error(StatusCode::CONFLICT, "Upload out of step, start it again"));
    }

    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        file.write_all(&chunk).await.map_err(|e| io_error(&e))?;
    }
    file.flush().await.map_err(|e| io_error(&e))?;
    let length = file.metadata().await.map_err(|e| io_error(&e))?.len();
    drop(file);

    let total = query.total.unwrap_or(length);
    if length < total {
        return Ok(Json(json!({ "ok": true, "done": false, "received": length })));
    }
    if length > total {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error(StatusCode::BAD_REQUEST, "Upload is larger than announced"));
    }
    if let Ok(metadata) = tokio::fs::metadata(&target.full).await {
        let _ = tokio::fs::set_permissions(&tmp, metadata.permissions()).await;
    }
    tokio::fs::rename(&tmp, &target.full)
        .await
        .map_err(|e| io_error(&e))?;
    tracing::info!("[dashboard] file uploaded: {} ({length} bytes)", target.rel);
    Ok(Json(json!({ "ok": true, "done": true })))
}

async fn download(
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Response, ApiError> {
    let root = state.px.dir.clone();
    let target = blocking(move || resolve(&root, &query.path)).await?;
    // Checked before opening: Windows reports a folder as access denied.
    let metadata = tokio::fs::metadata(&target.full)
        .await
        .map_err(|e| io_error(&e))?;
    if metadata.is_dir() {
        return Err(error(StatusCode::BAD_REQUEST, "Folders can't be downloaded yet"));
    }
    let file = tokio::fs::File::open(&target.full)
        .await
        .map_err(|e| io_error(&e))?;
    let safe_name: String = target
        .file_name()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
        .collect();
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\""))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, HeaderValue::from(metadata.len())),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

#[derive(Deserialize)]
struct MkdirBody {
    path: String,
}

async fn mkdir(State(state): State<AppState>, Json(body): Json<MkdirBody>) -> ApiResult {
    let root = state.px.dir.clone();
    blocking(move || {
        let target = resolve(&root, &body.path)?;
        target.ensure_mutable()?;
        fs::create_dir(&target.full).map_err(|e| io_error(&e))?;
        tracing::info!("[dashboard] folder created: {}", target.rel);
        Ok(Json(json!({ "ok": true })))
    })
    .await
}

#[derive(Deserialize)]
struct RenameBody {
    from: String,
    to: String,
}

async fn rename(State(state): State<AppState>, Json(body): Json<RenameBody>) -> ApiResult {
    let root = state.px.dir.clone();
    blocking(move || {
        let from = resolve(&root, &body.from)?;
        let to = resolve(&root, &body.to)?;
        from.ensure_mutable()?;
        to.ensure_mutable()?;
        // A case-only rename on Windows sees the file itself as the destination.
        let same_file = from.rel.eq_ignore_ascii_case(&to.rel);
        if to.full.exists() && !same_file {
            return Err(error(StatusCode::CONFLICT, "That name is already in use"));
        }
        fs::rename(&from.full, &to.full).map_err(|e| io_error(&e))?;
        tracing::info!("[dashboard] renamed: {} -> {}", from.rel, to.rel);
        Ok(Json(json!({ "ok": true })))
    })
    .await
}

fn remove_permanently(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

async fn delete(State(state): State<AppState>, Query(query): Query<PathQuery>) -> ApiResult {
    let root = state.px.dir.clone();
    blocking(move || {
        let target = resolve(&root, &query.path)?;
        target.ensure_mutable()?;
        if target.in_trash() {
            remove_permanently(&target.full).map_err(|e| io_error(&e))?;
            tracing::warn!("[dashboard] permanently deleted: {}", target.rel);
            return Ok(Json(json!({ "ok": true, "trashed": false })));
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let trash = resolve(&root, TRASH)?;
        fs::create_dir_all(&trash.full).map_err(|e| io_error(&e))?;
        let destination = trash.full.join(format!("{stamp}-{}", target.file_name()));
        fs::rename(&target.full, &destination).map_err(|e| io_error(&e))?;
        tracing::info!("[dashboard] moved to trash: {}", target.rel);
        Ok(Json(json!({ "ok": true, "trashed": true })))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jail() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("plugins")).unwrap();
        fs::write(dir.path().join("plugins/a.txt"), "a").unwrap();
        fs::write(dir.path().join("pumpkinx.toml"), "token = 'secret'").unwrap();
        fs::write(dir.path().join("pumpkin.toml"), "").unwrap();
        dir
    }

    #[test]
    fn resolves_inside_paths() {
        let dir = jail();
        assert_eq!(resolve(dir.path(), "plugins/a.txt").unwrap().rel, "plugins/a.txt");
        assert_eq!(resolve(dir.path(), "plugins/new.txt").unwrap().rel, "plugins/new.txt");
        assert_eq!(resolve(dir.path(), "").unwrap().rel, "");
        assert_eq!(resolve(dir.path(), "./plugins").unwrap().rel, "plugins");
    }

    #[test]
    fn refuses_escapes() {
        let dir = jail();
        for path in [
            "..",
            "../x",
            "plugins/../../x",
            "..\\x",
            "plugins\\..\\..\\x",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "C:/Windows/win.ini",
            "\\\\server\\share\\x",
            "pumpkin.toml:stream",
            "a\nb",
        ] {
            assert!(resolve(dir.path(), path).is_err(), "{path:?} must be refused");
        }
    }

    #[test]
    fn protects_secrets_in_every_spelling() {
        let dir = jail();
        for path in [
            "pumpkinx.toml",
            "PumpkinX.TOML",
            "./pumpkinx.toml",
            "plugins/../pumpkinx.toml",
            "pumpkinx.toml.",
            "pumpkinx.toml ",
            "nethernet-key.der",
        ] {
            assert!(resolve(dir.path(), path).is_err(), "{path:?} must be refused");
        }
    }

    #[test]
    fn root_and_settings_file_are_not_mutable() {
        let dir = jail();
        assert!(resolve(dir.path(), "").unwrap().ensure_mutable().is_err());
        assert!(resolve(dir.path(), "PUMPKIN.toml").unwrap().ensure_mutable().is_err());
        assert!(resolve(dir.path(), "plugins/a.txt").unwrap().ensure_mutable().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cant_leave_the_jail() {
        use std::os::unix::fs::symlink;
        let dir = jail();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "x").unwrap();
        symlink(outside.path(), dir.path().join("out")).unwrap();
        symlink(outside.path().join("nope"), dir.path().join("dangling")).unwrap();
        symlink(dir.path().join("pumpkinx.toml"), dir.path().join("alias")).unwrap();
        assert!(resolve(dir.path(), "out/secret").is_err());
        assert!(resolve(dir.path(), "out/new").is_err());
        assert!(resolve(dir.path(), "dangling").is_err());
        assert!(resolve(dir.path(), "alias").is_err());
    }
}
