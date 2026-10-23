//! The link between hydraui and the phone app: message types, framing, pairing, and the
//! host (desktop) and client (phone) ends over iroh.

pub mod client;
pub mod framing;
pub mod host;
pub mod pairing;
pub mod protocol;

pub use protocol::*;

use iroh::endpoint::{Builder, PortmapperConfig, presets};
use iroh::{Endpoint, RelayMode, SecretKey};

/// QUIC application close codes, so the phone can tell a refusal from a hang-up.
pub(crate) mod close {
    pub const BYE: u32 = 0;
    /// Not paired, a wrong or expired pairing secret, or a broken handshake.
    pub const REFUSED: u32 = 1;
    /// The app is older than `PROTOCOL_VERSION`.
    pub const TOO_OLD: u32 = 2;
    /// A newer connection from the same phone took over.
    pub const REPLACED: u32 = 3;
    /// The desktop forgot this phone.
    pub const UNPAIRED: u32 = 4;
}

/// An endpoint builder for either end. With relays off it uses only direct addresses:
/// no relays, no address lookup, no port mapping.
pub(crate) fn endpoint_builder(secret_key: &[u8; 32], relays: bool) -> Builder {
    let builder = if relays {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
    };
    builder.secret_key(SecretKey::from_bytes(secret_key))
}
