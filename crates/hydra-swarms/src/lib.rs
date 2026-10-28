//! Swarms: folders kept in sync between hydra machines. This crate holds the people (signing
//! keys), the signed membership roster, each swarm's iroh node, and the invites and hydra
//! protocol that bring machines in.
pub mod decide;
pub mod folder;
pub mod ids;
pub mod invites;
pub mod node;
pub mod paths;
pub mod roster;
pub mod service;
pub mod wire;

pub use folder::Attention;
pub use invites::Invite;
pub use roster::Role;
pub use service::{
    Command, DirectPeers, Identity, MachineView, MemberView, Resolution, ServiceConfig,
    ServiceEvent, SwarmView, SwarmsService,
};
