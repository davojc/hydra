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

    #[tokio::test]
    async fn round_trips_every_message() {
        let (mut a, mut b) = duplex(4096);
        let req = Request {
            id: 7,
            msg: ClientMsg::AddCard {
                project: "E:/p".into(),
                text: "x".into(),
                to: Column::Pickup,
            },
        };
        let srv = ServerMsg::Session(session());
        write_msg(&mut a, &req).await.unwrap();
        write_msg(&mut a, &srv).await.unwrap();
        assert_eq!(read_msg::<_, Request>(&mut b).await.unwrap(), Some(req));
        assert_eq!(read_msg::<_, ServerMsg>(&mut b).await.unwrap(), Some(srv));
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
