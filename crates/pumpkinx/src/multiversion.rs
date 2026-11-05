//! Native glue for the multiversion translator from `pumpkin-java-multiversion`.
//!
//! The upstream project ships the translator as a Wasm plugin, but the Wasm API only sees
//! play packets. Older clients have to get through login and configuration first, which
//! only the native connection events expose. Running the translator natively covers every
//! phase and skips the Wasm boundary on each packet.

use std::sync::Arc;

use pumpkin_core::{
    plugin::{
        BoxFuture, EventHandler, EventPriority,
        server::packet::{
            ConnectionPacketReceivedEvent, ConnectionPacketSentEvent, PacketReceivedEvent,
            PacketSentEvent,
        },
    },
    server::Server,
};
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_java_multiversion::packet::translator::{PacketTranslator, Phase};
use pumpkin_protocol::ConnectionState;
use pumpkin_util::version::JavaMinecraftVersion;
use pumpkin_world::chunk::ChunkData;

/// Older clients that have completed a join in testing, for display. The handshake admits
/// versions down to 1.7.2, but those can't finish joining yet (see PUMPKINX.md).
pub const SUPPORTED_RANGE: &str = "1.21.11 and 26.1";

fn needs_translation(version: JavaMinecraftVersion) -> bool {
    version != JavaMinecraftVersion::Unknown && version != CURRENT_MC_VERSION
}

const fn phase(state: ConnectionState) -> Option<Phase> {
    match state {
        ConnectionState::Status => Some(Phase::Status),
        ConnectionState::Login | ConnectionState::Transfer => Some(Phase::Login),
        ConnectionState::Config => Some(Phase::Config),
        ConnectionState::Play => Some(Phase::Play),
        ConnectionState::HandShake => None,
    }
}

struct PlayIncoming;

impl EventHandler<PacketReceivedEvent> for PlayIncoming {
    fn handle_blocking<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a mut PacketReceivedEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let version = event.player.client.java_version();
            if !needs_translation(version) {
                return;
            }
            if let Some((id, payload)) =
                PacketTranslator::translate_incoming_packet(event.packet_id, &event.payload, version)
            {
                event.packet_id = id;
                event.payload = payload.into();
            }
        })
    }
}

struct PlayOutgoing;

impl EventHandler<PacketSentEvent> for PlayOutgoing {
    fn handle_blocking<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a mut PacketSentEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let version = event.player.client.java_version();
            if !needs_translation(version) {
                return;
            }
            if let Some((id, payload)) =
                PacketTranslator::translate_outgoing_packet(event.packet_id, &event.payload, version)
            {
                event.packet_id = id;
                event.payload = payload.into();
            }
        })
    }
}

struct ConnectionIncoming;

impl EventHandler<ConnectionPacketReceivedEvent> for ConnectionIncoming {
    fn handle_blocking<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a mut ConnectionPacketReceivedEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(phase) = phase(event.state) else {
                return;
            };
            if !needs_translation(event.version) {
                return;
            }
            if let Some((id, payload)) = PacketTranslator::translate_incoming_in_phase(
                phase,
                event.packet_id,
                &event.payload,
                event.version,
            ) {
                event.packet_id = id;
                event.payload = payload.into();
            }
        })
    }
}

struct ConnectionOutgoing;

impl EventHandler<ConnectionPacketSentEvent> for ConnectionOutgoing {
    fn handle_blocking<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a mut ConnectionPacketSentEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(phase) = phase(event.state) else {
                return;
            };
            if !needs_translation(event.version) {
                return;
            }
            if let Some((id, payload)) = PacketTranslator::translate_outgoing_in_phase(
                phase,
                event.packet_id,
                &event.payload,
                event.version,
            ) {
                event.packet_id = id;
                event.payload = payload.into();
            }
        })
    }
}

/// Chunk packets in the format of `version`, using the per-era writers of the multiversion crate.
fn write_chunk(chunk: &ChunkData, version: JavaMinecraftVersion, out: &mut Vec<u8>) -> bool {
    use pumpkin_java_multiversion::chunk::{v1_7, v1_8, v1_9, v1_18};
    let result = if version >= JavaMinecraftVersion::V_1_18 {
        v1_18::write_chunk_data(chunk, &mut *out, &version)
    } else if version >= JavaMinecraftVersion::V_1_9 {
        v1_9::write_chunk_data(chunk, &mut *out, &version)
    } else if version >= JavaMinecraftVersion::V_1_8 {
        v1_8::write_chunk_data(chunk, &mut *out, &version)
    } else {
        v1_7::write_chunk_data(chunk, &mut *out, &version)
    };
    if let Err(err) = &result {
        tracing::debug!("Multiversion chunk write failed for {version}: {err:?}");
    }
    result.is_ok()
}

/// Registers the translator on every packet event and opens the handshake to older clients.
pub fn enable(server: &Arc<Server>) {
    pumpkin_core::pumpkinx::set_chunk_writer(write_chunk);
    let manager = &server.plugin_manager;
    // Incoming packets are translated before anything else sees them, outgoing ones last.
    manager.register::<PacketReceivedEvent, _>(Arc::new(PlayIncoming), EventPriority::Highest, true);
    manager.register::<PacketSentEvent, _>(Arc::new(PlayOutgoing), EventPriority::Lowest, true);
    manager.register::<ConnectionPacketReceivedEvent, _>(
        Arc::new(ConnectionIncoming),
        EventPriority::Highest,
        true,
    );
    manager.register::<ConnectionPacketSentEvent, _>(
        Arc::new(ConnectionOutgoing),
        EventPriority::Lowest,
        true,
    );
    pumpkin_core::pumpkinx::set_multiversion_enabled(true);
    tracing::info!("Multiversion is on. Tested older clients: {SUPPORTED_RANGE}");
}
