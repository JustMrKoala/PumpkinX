//! `PumpkinX` hooks inside the core.
//!
//! The `PumpkinX` layer (`crates/pumpkinx`) sets these switches at startup. They live in the
//! core because the handshake, the server list ping and the logger need to read them.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use pumpkin_data::packet::{CURRENT_MC_VERSION, LOWEST_SUPPORTED_MC_VERSION};
use pumpkin_util::version::JavaMinecraftVersion;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{Event, Subscriber, field::Field};
use tracing_subscriber::{Layer, layer::Context};

static MULTIVERSION: AtomicBool = AtomicBool::new(false);

/// Lets clients older than [`LOWEST_SUPPORTED_MC_VERSION`] past the handshake. Only turn this
/// on when a translator is registered for the packet events, or old clients get 26.3 packets.
pub fn set_multiversion_enabled(enabled: bool) {
    MULTIVERSION.store(enabled, Ordering::Release);
}

#[must_use]
pub fn multiversion_enabled() -> bool {
    MULTIVERSION.load(Ordering::Acquire)
}

/// Whether a Java client speaking `protocol` may log in.
#[must_use]
pub fn accepts_protocol(protocol: i32) -> bool {
    if protocol > CURRENT_MC_VERSION.protocol_version() {
        return false;
    }
    if protocol >= LOWEST_SUPPORTED_MC_VERSION.protocol_version() {
        return true;
    }
    multiversion_enabled()
        && u32::try_from(protocol)
            .is_ok_and(|p| JavaMinecraftVersion::from_protocol(p) != JavaMinecraftVersion::Unknown)
}

static SERVER_CULLING: AtomicBool = AtomicBool::new(false);

/// Hides buried blocks and out of sight entities from clients.
pub fn set_server_culling(enabled: bool) {
    SERVER_CULLING.store(enabled, Ordering::Release);
}

#[must_use]
pub fn server_culling() -> bool {
    SERVER_CULLING.load(Ordering::Acquire)
}

static CHAIN_MIN_TNT: AtomicU32 = AtomicU32::new(0);
static CHAIN_TICK_BUDGET_MS: AtomicU32 = AtomicU32::new(10);

/// Treats TNT clusters of at least `min_tnt` blocks as one blast that takes up to
/// `tick_budget_ms` of every tick. A `min_tnt` of 0 keeps the vanilla chain reaction.
pub fn set_chain_explosions(min_tnt: u32, tick_budget_ms: u32) {
    CHAIN_MIN_TNT.store(min_tnt, Ordering::Release);
    CHAIN_TICK_BUDGET_MS.store(tick_budget_ms.max(1), Ordering::Release);
}

#[must_use]
pub fn chain_min_tnt() -> u32 {
    CHAIN_MIN_TNT.load(Ordering::Acquire)
}

#[must_use]
pub fn chain_tick_budget_ms() -> u32 {
    CHAIN_TICK_BUDGET_MS.load(Ordering::Acquire)
}

/// One line of server log, as the dashboard shows it.
#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub time: u64,
    pub level: &'static str,
    pub target: String,
    pub message: String,
}

const LOG_CAPACITY: usize = 2000;

struct LogBuffer {
    lines: VecDeque<LogLine>,
    next_seq: u64,
}

static LOG_BUFFER: LazyLock<Mutex<LogBuffer>> = LazyLock::new(|| {
    Mutex::new(LogBuffer {
        lines: VecDeque::with_capacity(LOG_CAPACITY),
        next_seq: 1,
    })
});

static LOG_CHANNEL: LazyLock<broadcast::Sender<LogLine>> =
    LazyLock::new(|| broadcast::channel(1024).0);

/// The buffered log lines with a sequence number above `after`.
#[must_use]
pub fn recent_logs(after: u64) -> Vec<LogLine> {
    let buffer = LOG_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    buffer
        .lines
        .iter()
        .filter(|line| line.seq > after)
        .cloned()
        .collect()
}

/// Live log lines from now on.
#[must_use]
pub fn subscribe_logs() -> broadcast::Receiver<LogLine> {
    LOG_CHANNEL.subscribe()
}

fn push_log(level: &'static str, target: String, message: String) {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let line = {
        let mut buffer = LOG_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let line = LogLine {
            seq: buffer.next_seq,
            time,
            level,
            target,
            message,
        };
        buffer.next_seq += 1;
        if buffer.lines.len() == LOG_CAPACITY {
            buffer.lines.pop_front();
        }
        buffer.lines.push_back(line.clone());
        line
    };
    let _ = LOG_CHANNEL.send(line);
}

/// Removes ANSI escape sequences, which the console formatting puts into messages.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// Tracing layer that copies every log event into the dashboard's log buffer.
pub struct LogTapLayer;

impl<S: Subscriber> Layer<S> for LogTapLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let mut message = strip_ansi(&visitor.message);
        message.push_str(&visitor.fields);
        let level = match *metadata.level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "trace",
        };
        push_log(level, metadata.target().to_string(), message);
    }
}

/// Writes the body of a chunk packet (no packet id) for a client older than
/// [`CURRENT_MC_VERSION`]. Returns `false` when it can't handle that version.
pub type ChunkWriter =
    fn(&pumpkin_world::chunk::ChunkData, JavaMinecraftVersion, &mut Vec<u8>) -> bool;

static CHUNK_WRITER: std::sync::OnceLock<ChunkWriter> = std::sync::OnceLock::new();

/// Chunks are serialized before the packet events run, so translating them needs this hook
/// instead of `PacketSentEvent`.
pub fn set_chunk_writer(writer: ChunkWriter) {
    let _ = CHUNK_WRITER.set(writer);
}

#[must_use]
pub fn chunk_writer() -> Option<ChunkWriter> {
    CHUNK_WRITER.get().copied()
}

static TRUSTED_PLUGIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Marks a file in `plugins/` as shipped with the server: its permissions are granted without
/// the console confirmation, which a service has no way to answer.
pub fn set_trusted_plugin(file_name: &str) {
    let _ = TRUSTED_PLUGIN.set(file_name.to_string());
}

#[must_use]
pub fn is_trusted_plugin(path: &std::path::Path) -> bool {
    TRUSTED_PLUGIN
        .get()
        .is_some_and(|name| path.file_name().is_some_and(|f| f == name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_colors() {
        assert_eq!(strip_ansi("\u{1b}[33mPumpkin\u{1b}[0m ok"), "Pumpkin ok");
    }

    #[test]
    fn protocol_gate_follows_multiversion_switch() {
        let current = CURRENT_MC_VERSION.protocol_version();
        assert!(accepts_protocol(current));
        assert!(!accepts_protocol(current + 1));
        set_multiversion_enabled(false);
        assert!(!accepts_protocol(47));
        set_multiversion_enabled(true);
        assert!(accepts_protocol(47));
        assert!(!accepts_protocol(46));
        set_multiversion_enabled(false);
    }
}
