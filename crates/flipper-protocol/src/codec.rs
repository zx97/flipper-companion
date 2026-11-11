//! Length-delimited protobuf framing.
//!
//! Flipper uses a varint length prefix in front of every `PB_Main` message,
//! which corresponds to protobuf's "delimited" encoding.  The codec below
//! implements incremental decoding, so partial reads from a serial port are
//! handled without buffering outside the codec.

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio_util::codec::{Decoder, Encoder};

use crate::{error::ProtocolError, pb};

/// Upper bound on a single frame.  `Storage_read` responses are the largest
/// messages the firmware sends; 16 MiB leaves ample headroom while protecting
/// against a corrupted length prefix.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Encodes and decodes `PB_Main` frames.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = pb::Main;
    type Error = ProtocolError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let Some((length, prefix_length)) = read_length(src)? else {
            return Ok(None);
        };

        if length > MAX_FRAME_SIZE {
            return Err(ProtocolError::FrameTooLarge(length));
        }

        let total = prefix_length + length;
        if src.len() < total {
            src.reserve(total - src.len());
            return Ok(None);
        }

        src.advance(prefix_length);
        let body = src.split_to(length);

        Ok(Some(pb::Main::decode(body)?))
    }
}

impl Encoder<pb::Main> for FrameCodec {
    type Error = ProtocolError;

    fn encode(&mut self, message: pb::Main, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let length = message.encoded_len();
        dst.reserve(varint_length(length) + length);
        write_varint(length as u64, dst);
        message.encode(dst)?;
        Ok(())
    }
}

fn read_length(src: &BytesMut) -> Result<Option<(usize, usize)>, ProtocolError> {
    let mut value = 0u64;

    for (index, &byte) in src.iter().take(5).enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * index);

        if byte & 0x80 == 0 {
            let length = usize::try_from(value).map_err(|_| ProtocolError::InvalidLengthPrefix)?;
            return Ok(Some((length, index + 1)));
        }
    }

    if src.len() >= 5 {
        Err(ProtocolError::InvalidLengthPrefix)
    } else {
        Ok(None)
    }
}

fn varint_length(mut value: usize) -> usize {
    let mut length = 1;

    while value >= 0x80 {
        value >>= 7;
        length += 1;
    }

    length
}

fn write_varint(mut value: u64, dst: &mut BytesMut) {
    while value >= 0x80 {
        dst.put_u8((value as u8) | 0x80);
        value >>= 7;
    }

    dst.put_u8(value as u8);
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn message(id: u32) -> pb::Main {
        pb::Main {
            command_id: id,
            command_status: 0,
            has_next: false,
            content: Some(pb::main::Content::SystemPingRequest(
                crate::pb_system::PingRequest {
                    data: vec![1, 2, 3],
                },
            )),
        }
    }

    fn encode(message: &pb::Main) -> BytesMut {
        let mut buffer = BytesMut::new();
        FrameCodec.encode(message.clone(), &mut buffer).unwrap();
        buffer
    }

    #[test]
    fn round_trips_a_message() {
        let original = message(42);
        let mut buffer = encode(&original);

        let decoded = FrameCodec.decode(&mut buffer).unwrap().unwrap();

        assert_eq!(decoded.command_id, 42);
        assert!(matches!(
            decoded.content,
            Some(pb::main::Content::SystemPingRequest(_))
        ));
        assert!(buffer.is_empty());
    }

    #[test]
    fn waits_for_a_fragmented_frame() {
        let bytes = encode(&message(7));
        let mut buffer = BytesMut::new();

        for byte in bytes.iter().take(bytes.len() - 1) {
            buffer.put_u8(*byte);
            assert!(FrameCodec.decode(&mut buffer).unwrap().is_none());
        }

        buffer.put_u8(*bytes.last().unwrap());
        assert_eq!(
            FrameCodec.decode(&mut buffer).unwrap().unwrap().command_id,
            7
        );
    }

    #[test]
    fn decodes_several_frames_from_one_buffer() {
        let mut buffer = encode(&message(1));
        buffer.extend_from_slice(&encode(&message(2)));

        assert_eq!(
            FrameCodec.decode(&mut buffer).unwrap().unwrap().command_id,
            1
        );
        assert_eq!(
            FrameCodec.decode(&mut buffer).unwrap().unwrap().command_id,
            2
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn rejects_oversized_frames() {
        let mut buffer = BytesMut::new();
        write_varint((MAX_FRAME_SIZE + 1) as u64, &mut buffer);

        assert!(matches!(
            FrameCodec.decode(&mut buffer),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn rejects_endless_varints() {
        let mut buffer = BytesMut::from(&[0xFFu8; 5][..]);
        assert!(matches!(
            FrameCodec.decode(&mut buffer),
            Err(ProtocolError::InvalidLengthPrefix)
        ));
    }

    #[test]
    fn uses_the_same_encoding_as_protobuf_delimited() {
        let original = message(9);
        let mut expected = Vec::new();
        original.encode_length_delimited(&mut expected).unwrap();

        let actual = encode(&original).to_vec();

        assert_eq!(actual, expected);
    }
}
