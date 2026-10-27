//! The hydra protocol between swarm machines: joining with an invite, readers' own changes passed
//! to an owner, and an owner's notices. One bi-stream per request, each way one length-prefixed
//! JSON frame: a big-endian u32 length, then that many bytes of JSON.
//!
//! The framing is a copy of the phone link's rather than a dependency on it, so the two protocols
//! can change separately.
use std::io;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::roster::{MAX_CHANGE_BYTES, Member, SignedChange};

pub const ALPN: &[u8] = b"hydra/swarm/1";

/// The longest frame either side accepts: room for the longest change, which a leaving passed
/// on or a removal notice carries.
pub const MAX_FRAME: u32 = MAX_CHANGE_BYTES as u32 + (64 << 10);

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Spends an invite. `member`'s role is ignored: the invite's is used. `proof` is
    /// `member.person`'s signature of the token, in hex.
    Join {
        token: String,
        member: Member,
        proof: String,
    },
    /// A reader's own change, for an owner to write.
    Relay { change: SignedChange },
    /// An owner telling this machine it was removed, with the change that removed it.
    Removed { change: SignedChange },
    /// An owner handing over the write capability (hex) after promoting this machine's person.
    Promoted { write_secret: String },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Joined {
        ns: String,
        anchor: String,
        name: String,
        /// Hex; writers and owners only.
        write_secret: Option<String>,
    },
    Ok,
    Refused {
        reason: String,
    },
}

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
    // Grown as bytes arrive, so a frame that only claims to be long costs nothing.
    let mut body = Vec::new();
    r.take(u64::from(len)).read_to_end(&mut body).await?;
    if body.len() != len as usize {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::PersonKey;
    use crate::roster::{Change, Machine, Role};
    use serde_json::json;
    use tokio::io::duplex;

    fn requests() -> Vec<Request> {
        let key = PersonKey::generate();
        let member = Member {
            person: key.id(),
            name: "rae".into(),
            role: Role::Reader,
            machines: vec![Machine {
                endpoint: "ep".into(),
                name: "rae-pc".into(),
                author: "au".into(),
            }],
        };
        let leave = Change::Leave {
            person: key.id(),
            kept: Default::default(),
        };
        let change = SignedChange::sign("ns", leave, &key, 1);
        vec![
            Request::Join {
                token: "t".into(),
                member,
                proof: "p".into(),
            },
            Request::Relay {
                change: change.clone(),
            },
            Request::Removed { change },
            Request::Promoted {
                write_secret: "s".into(),
            },
        ]
    }

    fn replies() -> Vec<Reply> {
        vec![
            Reply::Joined {
                ns: "ns".into(),
                anchor: "a".into(),
                name: "Notes".into(),
                write_secret: None,
            },
            Reply::Ok,
            Reply::Refused {
                reason: "no".into(),
            },
        ]
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
        round_trip(requests()).await;
        round_trip(replies()).await;
    }

    #[test]
    fn json_shape_is_stable() {
        assert_eq!(
            serde_json::to_value(Reply::Ok).unwrap(),
            json!({"type":"ok"})
        );
        assert_eq!(
            serde_json::to_value(Reply::Refused { reason: "x".into() }).unwrap(),
            json!({"type":"refused","reason":"x"})
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
}
