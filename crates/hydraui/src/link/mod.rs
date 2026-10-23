//! The phone link's side of hydraui: pure mapping between hydraui's own types and the
//! `hydra_link` wire types, and the board changes the phone may make.

pub mod boards;
pub mod owner;
pub mod store;
pub mod wire;

/// What to do with the phone's request to turn Remote Control on for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcAction {
    AlreadyOn,
    /// Type `/remote-control` into the session.
    Type,
    Refuse(&'static str),
}

/// Remote Control starts only from an idle Claude with nothing in its input: typing a
/// command anywhere else would land in a draft or an answer the user hasn't given.
pub fn remote_control_reply(
    claude: Option<crate::mcp::SessionState>,
    draft: bool,
    on: bool,
) -> RcAction {
    use crate::mcp::SessionState;
    if on {
        return RcAction::AlreadyOn;
    }
    match claude {
        None => RcAction::Refuse("This terminal isn't running Claude."),
        Some(SessionState::Idle) if !draft => RcAction::Type,
        Some(_) => {
            RcAction::Refuse("Remote Control is off for this session. Answer it at your PC.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::SessionState::*;

    const BUSY: RcAction =
        RcAction::Refuse("Remote Control is off for this session. Answer it at your PC.");

    #[test]
    fn remote_control_is_typed_only_into_an_idle_session_without_a_draft() {
        assert_eq!(remote_control_reply(Some(Waiting), false, false), BUSY);
        assert_eq!(remote_control_reply(Some(Idle), true, false), BUSY);
        assert_eq!(remote_control_reply(Some(Working), false, false), BUSY);
        assert_eq!(remote_control_reply(Some(Ended), false, false), BUSY);
        assert_eq!(
            remote_control_reply(Some(Idle), false, false),
            RcAction::Type
        );
        assert_eq!(
            remote_control_reply(Some(Waiting), true, true),
            RcAction::AlreadyOn
        );
        assert_eq!(
            remote_control_reply(None, false, false),
            RcAction::Refuse("This terminal isn't running Claude.")
        );
    }
}
