//! Platform-neutral core of hydra: environments, config, secrets and launch resolution.
pub mod config;
pub mod contribution;
pub mod envs;
pub mod lock;
pub mod name;
pub mod paths;
pub mod provider;
pub mod resolve;
pub mod rewrite;
pub mod secret;
