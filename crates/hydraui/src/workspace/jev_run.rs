//! A run's Jev stage in the app (email triage spec §4): no terminal opens. Jev is asked about
//! the run's input off the UI thread, and its answer is filed as the stage's report
//! (`jev_stage::file`), which moves the run on or asks a person, as any stage's report does.

use gpui::prelude::*;
use gpui::{Context, Window};

use super::Workspace;
use crate::jev::{JEV_URL, JevKey};
use crate::jev_stage::{self, JevStage};
use crate::mcp::server;
use crate::runs::StartStage;

impl Workspace {
    pub(super) fn run_jev_stage(
        &mut self,
        s: StartStage,
        stage: JevStage,
        bar: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.hydra.paths.root().to_path_buf();
        crate::action_log::write(format!(
            "run {}: stage {} asks Jev about {}",
            s.run, s.title, stage.input
        ));
        cx.spawn_in(window, async move |_, cx| {
            let args = cx
                .background_spawn(async move {
                    let settings = stage.settings(bar);
                    let asked = (|| {
                        let key = JevKey::app()
                            .get()?
                            .ok_or("no Jev API key: set it with Set Jev API key…")?;
                        let text = jev_stage::input_text(&root, &s.run, &stage)?;
                        crate::jev::ask(JEV_URL, &key, &text, &settings).map_err(|f| {
                            if let Some(body) = &f.body {
                                crate::action_log::write(format!("jev: {} {body}", f.reason));
                            }
                            f.reason
                        })
                    })();
                    let args = match asked {
                        Ok(reply) => {
                            crate::action_log::write(format!(
                                "run {}: Jev picked {} ({:.2}): {}",
                                s.run, reply.verdict.choice, reply.verdict.confidence, reply.body
                            ));
                            jev_stage::report(&reply.verdict, &settings.model)
                        }
                        Err(why) => {
                            crate::action_log::write(format!("run {}: Jev failed: {why}", s.run));
                            jev_stage::failed(&why)
                        }
                    };
                    (s, args)
                })
                .await;
            let (s, args) = args;
            let _ = cx.update(|_, _| {
                let Some(sv) = server::get() else {
                    return;
                };
                let filed = jev_stage::file(&mut server::lock(sv.registry()), &s, &args);
                match filed {
                    Ok(Some(event)) => sv.broadcast(event),
                    Ok(None) => {}
                    Err(e) => crate::action_log::write(format!(
                        "run {}: Jev's report for {} wasn't taken: {e}",
                        s.run, s.title
                    )),
                }
            });
        })
        .detach();
    }
}
