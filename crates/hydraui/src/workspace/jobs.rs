//! Jobs taken through the swarm (core spec §87): a trigger asks the swarm's claims service for
//! each job before it marks it, keeps the claim while the job's run is open, and finishes it
//! (done) or gives it back (released) when the run ends. The claims client renews a held claim
//! by itself; a claim this hydra stops holding (a quit, a crash) lapses, and the source's own
//! mark (the 👀, the label, the assignee) still keeps teammates off it.

use std::sync::Arc;

use hydra_swarms::claims::{Holder, HolderKind};

use super::Workspace;
use crate::jobs::{Taken, says};
use crate::mcp::claims_tools::ClaimsApi;
use crate::mcp::server;
use crate::runs::{Change, Decided};

/// The swarms' claims, when this hydraui has them.
pub(super) fn claims_api() -> Option<Arc<dyn ClaimsApi>> {
    server::get()?.claims().map(|c| c.api())
}

/// Takes `key` in `context` of `swarm` for pipeline `label`, blocking: call it off the UI
/// thread. Without the swarms, the source's own check decides.
pub(super) fn take(
    api: Option<&Arc<dyn ClaimsApi>>,
    swarm: &str,
    context: &str,
    key: &str,
    label: &str,
) -> Taken {
    let Some(api) = api else {
        return Taken::Unsettled("the swarms aren't running in this hydraui".into());
    };
    let holder = Holder {
        kind: HolderKind::PipelineRun,
        label: label.to_string(),
    };
    says(api.take(Some(swarm), context, key, holder), context)
}

impl Workspace {
    /// Keeps the claims a trigger took, by the ticket of each run they cover.
    pub(super) fn hold_claims(&mut self, claims: Vec<(String, u64)>) {
        for (ticket, holder) in claims {
            self.job_claims.insert(ticket.to_lowercase(), holder);
        }
    }

    /// A run that ended finishes its job's claim (done: nobody takes it again) or, stopped,
    /// gives it back (released: someone else may). A claim several runs share ends with the
    /// last of them.
    pub(super) fn settle_claims(&mut self, changes: &[Change]) {
        let Some(api) = claims_api() else {
            return;
        };
        let ended: Vec<(String, bool)> = server::get()
            .map(|sv| {
                let reg = server::lock(sv.registry());
                changes
                    .iter()
                    .filter_map(|c| {
                        let done = match &c.what {
                            Decided::RunFinished | Decided::RunSettled(_) => true,
                            Decided::RunStopped(_) => false,
                            _ => return None,
                        };
                        Some((reg.runs.run(&c.run)?.ticket.to_lowercase(), done))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (ticket, done) in ended {
            let Some(holder) = self.job_claims.remove(&ticket) else {
                continue;
            };
            if self.job_claims.values().any(|h| *h == holder) {
                continue;
            }
            let result = if done {
                api.done(holder)
            } else {
                api.release(holder)
            };
            crate::action_log::write(match result {
                Ok(()) if done => format!("claims: {ticket} done"),
                Ok(()) => format!("claims: {ticket} given back"),
                Err(e) => format!("claims: {ticket}: {e}"),
            });
        }
    }
}
