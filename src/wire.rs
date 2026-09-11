//! Length-prefixed binary framing for the Racli wire protocol: a single request frame followed by
//! a single response frame per connection, reusing the same prost message types as before.
//!
//! Frame layout: `[1 byte tag][4 bytes little-endian u32 payload_len][payload_len bytes payload]`.

use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

/// Upper bound on a single frame's payload length, guarding against a corrupt or hostile length header.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// Request method tag, one per `RacliSession` RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `RacliSession::get_version`.
    GetVersion = 0,
    /// `RacliSession::search`.
    Search = 1,
    /// `RacliSession::find_definition`.
    FindDefinition = 2,
}

impl TryFrom<u8> for Method {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Method::GetVersion),
            1 => Ok(Method::Search),
            2 => Ok(Method::FindDefinition),
            other => Err(WireError::UnknownMethod(other)),
        }
    }
}

/// Response status tag, mirroring [`crate::racli_session::RacliRpcError`] plus a success case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Payload is the prost-encoded response message.
    Ok = 0,
    /// Payload is UTF-8 error text (`RacliRpcError::InvalidArgument`).
    InvalidArgument = 1,
    /// Payload is UTF-8 error text (`RacliRpcError::Internal`).
    Internal = 2,
}

impl TryFrom<u8> for Status {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Status::Ok),
            1 => Ok(Status::InvalidArgument),
            2 => Ok(Status::Internal),
            other => Err(WireError::UnknownStatus(other)),
        }
    }
}

/// Failures framing or parsing a wire message.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// Failed reading or writing the underlying stream.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Failed decoding a protobuf payload.
    #[error(transparent)]
    Decode(#[from] prost::DecodeError),
    /// The tag byte on a request frame did not match a known [`Method`].
    #[error("unknown method tag {0}")]
    UnknownMethod(u8),
    /// The tag byte on a response frame did not match a known [`Status`].
    #[error("unknown status tag {0}")]
    UnknownStatus(u8),
    /// The frame's declared payload length exceeded [`MAX_FRAME_LEN`].
    #[error("frame length {len} exceeds maximum {max}")]
    FrameTooLarge {
        /// The declared payload length.
        len: u32,
        /// The configured maximum ([`MAX_FRAME_LEN`]).
        max: u32,
    },
}

/// Writes one frame (`tag` followed by `payload`, length-prefixed) to `io`.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    io: &mut W,
    tag: u8,
    payload: &[u8],
) -> Result<(), WireError> {
    io.write_u8(tag).await?;
    io.write_u32_le(payload.len() as u32).await?;
    io.write_all(payload).await?;
    io.flush().await?;
    Ok(())
}

/// Reads one frame (tag byte plus length-prefixed payload) from `io`.
pub async fn read_frame<R: AsyncRead + Unpin>(io: &mut R) -> Result<(u8, Vec<u8>), WireError> {
    let tag = io.read_u8().await?;
    let len = io.read_u32_le().await?;
    if len > MAX_FRAME_LEN {
        return Err(WireError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        });
    }
    let mut payload = vec![0u8; len as usize];
    io.read_exact(&mut payload).await?;
    Ok((tag, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_a_frame() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_frame(&mut a, Method::Search as u8, b"hello")
            .await
            .expect("write");
        let (tag, payload) = read_frame(&mut b).await.expect("read");
        assert_eq!(tag, Method::Search as u8);
        assert_eq!(payload, b"hello");
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = tokio::io::duplex(16);
        a.write_u8(0).await.expect("tag");
        a.write_u32_le(MAX_FRAME_LEN + 1).await.expect("len");
        let err = read_frame(&mut b).await.unwrap_err();
        assert!(matches!(err, WireError::FrameTooLarge { .. }));
    }

    #[test]
    fn method_try_from_rejects_unknown_tag() {
        assert!(matches!(Method::try_from(99), Err(WireError::UnknownMethod(99))));
    }

    #[test]
    fn status_try_from_rejects_unknown_tag() {
        assert!(matches!(Status::try_from(99), Err(WireError::UnknownStatus(99))));
    }
}
