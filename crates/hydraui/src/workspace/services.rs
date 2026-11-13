//! Which services are connected where (core spec §85): a pipeline needs the service its trigger
//! takes work from (Slack, Linear, Gmail) connected in the environment it runs in. hydraui
//! reads that from the stored secrets off the UI thread, at start and every 10 minutes, and
//! notes it at once when Connect Slack or Connect Gmail succeeds. The tool server refuses a
//! run without it; the card says what to connect.

use std::time::{Duration, Instant};

use gpui::Context;

use super::Workspace;
use crate::mcp::server;
use crate::pipeline::Service;

/// How often the stored secrets are read again.
const EVERY: Duration = Duration::from_secs(10 * 60);

/// Notes whether `service` is connected in `env`.
pub(super) fn note(env: &str, service: Service, connected: bool) {
    if let Some(sv) = server::get() {
        let cache = server::lock(sv.registry()).connected.clone();
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((env.to_string(), service), connected);
    }
}

/// Whether `service` is connected in `env`, as last read; `None` before it has been.
pub(super) fn connected(env: &str, service: Service) -> Option<bool> {
    let sv = server::get()?;
    let cache = server::lock(sv.registry()).connected.clone();
    let known = cache.lock().unwrap_or_else(|e| e.into_inner());
    known.get(&(env.to_string(), service)).copied()
}

/// Of `needs` (a pipeline's, `Pipeline::needs`), what isn't connected in `env`. Not read yet
/// counts as connected.
pub(super) fn missing(needs: &[Service], env: &str) -> Vec<Service> {
    needs
        .iter()
        .copied()
        .filter(|s| connected(env, *s) == Some(false))
        .collect()
}

impl Workspace {
    /// At start and every 10 minutes: every environment's services read again.
    pub(super) fn services_tick(&mut self, cx: &mut Context<Self>) {
        if self.services_checked.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.services_checked = Some(Instant::now());
        let hydra = self.hydra.clone();
        cx.spawn(async move |this, cx| {
            let read: Vec<(String, Service, bool)> = cx
                .background_executor()
                .spawn(async move {
                    let mut found = Vec::new();
                    for env in hydra.env_names() {
                        found.push((env.clone(), Service::Slack, hydra.slack_token(&env).is_ok()));
                        found.push((env.clone(), Service::Linear, hydra.linear_key(&env).is_ok()));
                        found.push((
                            env.clone(),
                            Service::Gmail,
                            hydra.gmail_sign_in(&env).is_ok(),
                        ));
                    }
                    found
                })
                .await;
            for (env, service, ok) in read {
                note(&env, service, ok);
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
}
