//! Length-prefixed JSON frames: a big-endian u32 length, then that many bytes of JSON.

use std::io;

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The longest frame either side accepts.
pub const MAX_FRAME: u32 = 1 << 20;

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|len| *len <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame too long"))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame).await?;
    w.flush().await
}

/// Reads one frame. `None` means the stream ended cleanly between frames.
pub async fn read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
) -> io::Result<Option<T>> {
    let mut header = [0u8; 4];
    let mut got = 0;
    while got < header.len() {
        match r.read(&mut header[got..]).await? {
            0 if got == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    let len = u32::from_be_bytes(header);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::*;
    use serde_json::json;
    use tokio::io::duplex;

    fn session() -> SessionInfo {
        SessionInfo {
            id: 1,
            env: "work".into(),
            env_color: "#112233".into(),
            label: "claude".into(),
            cwd: "E:/p".into(),
            project: Some("E:/p".into()),
            kind: Kind::Claude,
            status: Status::NeedsYou,
            card: None,
            remote_control: false,
            changed_at: 1_700_000_000,
        }
    }

    fn board() -> BoardInfo {
        BoardInfo {
            project: "E:/p".into(),
            name: "p".into(),
            columns: vec![
                vec![WireCard {
                    text: "a".into(),
                    taker: None,
                    commit: None,
                }],
                vec![],
                vec![WireCard {
                    text: "b".into(),
                    taker: Some("s".into()),
                    commit: None,
                }],
                vec![WireCard {
                    text: "c".into(),
                    taker: None,
                    commit: Some("abc".into()),
                }],
                vec![],
            ],
        }
    }

    fn client_msgs() -> Vec<ClientMsg> {
        vec![
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
            },
            ClientMsg::Pair { secret: "s".into() },
            ClientMsg::Snapshot,
            ClientMsg::Board {
                project: "E:/p".into(),
            },
            ClientMsg::AddCard {
                project: "E:/p".into(),
                text: "x".into(),
                to: Column::Pickup,
            },
            ClientMsg::MoveCard {
                project: "E:/p".into(),
                card: CardRef {
                    column: Column::Verify,
                    text: "x".into(),
                    nth: 1,
                },
                to: Column::Done,
            },
            ClientMsg::RemoteControl { session: 2 },
            ClientMsg::Seen { session: 3 },
        ]
    }

    fn outcomes() -> Vec<Outcome> {
        vec![
            Outcome::Ok,
            Outcome::Snapshot {
                sessions: vec![session()],
                projects: vec!["E:/p".into()],
            },
            Outcome::Board(board()),
            Outcome::Refused {
                reason: "no".into(),
            },
            Outcome::Missing,
            Outcome::Paired { word: "w".into() },
        ]
    }

    fn server_msgs() -> Vec<ServerMsg> {
        let mut v = vec![
            ServerMsg::Welcome {
                version: 1,
                window: "w".into(),
            },
            ServerMsg::Session(session()),
            ServerMsg::SessionGone { id: 4 },
            ServerMsg::Board(board()),
            ServerMsg::Alert {
                session: 5,
                kind: AlertKind::Finished,
            },
            ServerMsg::Notice { text: "n".into() },
            ServerMsg::TooOld { need: 2 },
        ];
        v.extend(
            outcomes()
                .into_iter()
                .enumerate()
                .map(|(i, outcome)| ServerMsg::Reply {
                    id: i as u64,
                    outcome,
                }),
        );
        v
    }

    async fn round_trip<T>(items: Vec<T>)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let (mut a, mut b) = duplex(1 << 16);
        for item in &items {
            write_msg(&mut a, item).await.unwrap();
        }
        for item in items {
            assert_eq!(read_msg::<_, T>(&mut b).await.unwrap(), Some(item));
        }
    }

    #[tokio::test]
    async fn round_trips_every_message() {
        round_trip(client_msgs()).await;
        round_trip(server_msgs()).await;
        round_trip(vec![Request {
            id: 7,
            msg: ClientMsg::AddCard {
                project: "E:/p".into(),
                text: "x".into(),
                to: Column::Pickup,
            },
        }])
        .await;
    }

    #[test]
    fn refusals_and_notices_have_a_stable_shape() {
        assert_eq!(
            serde_json::to_value(Outcome::Refused { reason: "x".into() }).unwrap(),
            json!({"type":"refused","reason":"x"})
        );
        assert_eq!(
            serde_json::to_value(ServerMsg::Notice { text: "x".into() }).unwrap(),
            json!({"type":"notice","text":"x"})
        );
    }

    #[tokio::test]
    async fn eof_between_frames_is_none() {
        let (a, mut b) = duplex(64);
        drop(a);
        assert_eq!(read_msg::<_, Request>(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_frame_is_refused() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&(MAX_FRAME + 1).to_be_bytes()).await.unwrap();
        let err = read_msg::<_, Request>(&mut b).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn json_shape_is_stable() {
        assert_eq!(
            serde_json::to_value(ClientMsg::Seen { session: 3 }).unwrap(),
            json!({"type":"seen","session":3})
        );
    }
}
