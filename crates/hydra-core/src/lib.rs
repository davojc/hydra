//! Platform-neutral core of hydra: environments, config, secrets and launch resolution.
pub mod bindedit;
pub mod bindings;
pub mod borrow;
pub mod config;
pub mod contribution;
pub mod envedit;
pub mod envs;
pub mod guard;
pub mod lock;
pub mod name;
pub mod paths;
pub mod provider;
pub mod resolve;
pub mod rewrite;
pub mod secret;
