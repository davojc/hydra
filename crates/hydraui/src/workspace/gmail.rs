//! **Connect Gmail** and **Check Gmail** (email triage spec §3): signing an environment in to
//! the shared mailbox's own Google account, and listing its newest threads to show the
//! sign-in reads. Google is called off the UI thread; `crate::gmail` holds the calls.

use gpui::prelude::*;
use gpui::{Context, Window};

use super::Workspace;
use crate::gmail::{self, Endpoints};
use crate::inbox::{Note, PushNote};

/// How many threads **Check Gmail** lists.
const CHECK_MAX: usize = 10;

impl Workspace {
    /// The team's Google client from config, or a notice saying what to set.
    fn gmail_client(
        &self,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gmail::Client> {
        let (ui, _) = crate::config::load(&self.hydra.paths);
        let client = ui.gmail_client();
        if client.is_none() {
            window.push_note(
                Note::warning(
                    "Set gmail_client_id and gmail_client_secret in hydraui's config.toml to \
                     the team's Desktop-type Google OAuth client first.",
                )
                .title(title.to_string()),
                cx,
            );
        }
        client
    }

    /// **Connect Gmail** for `env`: Google's consent page in the browser, back to a local port,
    /// the code traded for a refresh token, stored as `<env>/gmail`.
    pub(super) fn connect_gmail(
        &mut self,
        env: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = "Connect Gmail";
        let Some(client) = self.gmail_client(title, window, cx) else {
            return;
        };
        let started = (|| {
            let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = server
                .server_addr()
                .to_ip()
                .map(|a| a.port())
                .ok_or("no local port")?;
            let state = crate::slack_connect::nonce()?;
            let pkce = gmail::pkce()?;
            Ok::<_, String>((server, format!("http://127.0.0.1:{port}/"), state, pkce))
        })();
        let (server, redirect, state, pkce) = match started {
            Ok(s) => s,
            Err(e) => {
                window.push_note(Note::error(e).title(title), cx);
                return;
            }
        };
        let ep = Endpoints::google();
        crate::action_log::write(format!("gmail: connecting {env}"));
        cx.open_url(&gmail::auth_url(
            &ep,
            &client.id,
            &redirect,
            &pkce.challenge,
            &state,
            None,
        ));
        window.push_note(
            Note::info(
                "Finish in your browser: sign in as the shared mailbox's account (not your own), \
                 then Allow.",
            )
            .title(title),
            cx,
        );
        let hydra = self.hydra.clone();
        cx.spawn_in(window, async move |_, cx| {
            let done = cx
                .background_spawn(async move {
                    let code = gmail::receive(&server, &state, gmail::WAIT)?;
                    let t = gmail::exchange(&ep, &client, &code, &pkce.verifier, &redirect)?;
                    let refresh = t.refresh.ok_or(
                        "Google sent no refresh token: remove hydraui's access from the \
                         account's Google security page, then connect again",
                    )?;
                    let address = gmail::profile(&ep, &t.access)?;
                    hydra.set_secret(&format!("{env}/gmail"), &refresh)?;
                    Ok::<_, String>((env, address))
                })
                .await;
            let _ = cx.update(|window, cx| match done {
                Ok((env, address)) => {
                    crate::action_log::write(format!("gmail: {env} connected as {address}"));
                    window.push_note(
                        Note::success(format!("Gmail is connected for {env} as {address}."))
                            .title(title),
                        cx,
                    );
                }
                Err(e) => {
                    crate::action_log::write(format!("gmail: connect failed: {e}"));
                    window.push_note(Note::error(e).title(title), cx);
                }
            });
        })
        .detach();
    }

    /// **Check Gmail** for `env`: the newest inbox threads, in a notice and hydraui.log.
    pub(super) fn check_gmail(&mut self, env: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = "Check Gmail";
        let Some(client) = self.gmail_client(title, window, cx) else {
            return;
        };
        let hydra = self.hydra.clone();
        cx.spawn_in(window, async move |_, cx| {
            let done = cx
                .background_spawn(async move {
                    let ep = Endpoints::google();
                    let access = gmail::refresh(&ep, &client, &hydra.gmail_sign_in(&env)?)?;
                    let address = gmail::profile(&ep, &access)?;
                    let threads = gmail::list_threads(&ep, &access, "in:inbox", CHECK_MAX)?;
                    Ok::<_, String>((env, address, threads))
                })
                .await;
            let _ = cx.update(|window, cx| match done {
                Ok((env, address, threads)) => {
                    for t in &threads {
                        crate::action_log::write(format!(
                            "gmail: {address} {} | {} | {} | {} message(s)",
                            t.id, t.from, t.subject, t.messages
                        ));
                    }
                    let lines: Vec<String> = threads
                        .iter()
                        .take(5)
                        .map(|t| format!("{} ({})", t.subject, sender(&t.from)))
                        .collect();
                    let message = if threads.is_empty() {
                        format!("{address} has no threads in its inbox.")
                    } else {
                        format!(
                            "{address}, newest {} inbox threads (all in hydraui.log): {}",
                            threads.len(),
                            lines.join(" · ")
                        )
                    };
                    window.push_note(Note::success(message).title(format!("{title}: {env}")), cx);
                }
                Err(e) => {
                    crate::action_log::write(format!("gmail: check failed: {e}"));
                    window.push_note(Note::error(e).title(title), cx);
                }
            });
        })
        .detach();
    }
}

/// `Ann Lee <ann@x.com>` reads as `Ann Lee`; a bare address stays as it is.
fn sender(from: &str) -> &str {
    from.split_once('<')
        .map(|(name, _)| name.trim().trim_matches('"'))
        .filter(|n| !n.is_empty())
        .unwrap_or(from.trim())
}

#[cfg(test)]
mod tests {
    use super::sender;

    #[test]
    fn a_sender_reads_as_their_name() {
        assert_eq!(sender("Ann Lee <ann@x.com>"), "Ann Lee");
        assert_eq!(sender("\"Lee, Ann\" <ann@x.com>"), "Lee, Ann");
        assert_eq!(sender("<ann@x.com>"), "<ann@x.com>");
        assert_eq!(sender("ann@x.com"), "ann@x.com");
    }
}
