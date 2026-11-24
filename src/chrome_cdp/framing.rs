//! Bounded NUL-delimited Chrome remote-debugging-pipe framing.
//! No JSON, process, session, UI or permission policy belongs here.
use std::io::{self, BufRead, Read};

/// Read one complete frame, excluding its NUL delimiter.
/// Reuse the caller's buffer and bulk-copy through BufRead instead of pushing
/// individual bytes. Limit the read before allocating, including hostile
/// streams that never send a delimiter. A truncated EOF is not a frame.
pub(super) fn read_message(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    max_message: usize,
) -> io::Result<bool> {
    buffer.clear();
    let limit = (max_message as u64).saturating_add(1);
    (&mut *reader).take(limit).read_until(0, buffer)?;
    match buffer.last() {
        Some(&0) => {
            buffer.pop();
            Ok(true)
        }
        _ if buffer.len() > max_message => Err(io::Error::new(
            io::ErrorKind::InvalidData, "CDP message exceeds size limit",
        )),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn preserves_order_across_fragmented_and_coalesced_reads() {
        for capacity in [1, 2, 3, 32] {
            let mut reader = BufReader::with_capacity(capacity, Cursor::new(b"abc\0de\0\0f\0"));
            let mut buffer = Vec::new();
            for expected in [b"abc".as_slice(), b"de", b"", b"f"] {
                assert!(read_message(&mut reader, &mut buffer, 3).unwrap());
                assert_eq!(buffer, expected);
            }
            assert!(!read_message(&mut reader, &mut buffer, 3).unwrap());
        }
    }

    #[test]
    fn exact_limit_is_valid_but_oversized_frame_is_rejected() {
        let mut reader = Cursor::new(b"1234\012345\0");
        let mut buffer = Vec::new();
        assert!(read_message(&mut reader, &mut buffer, 4).unwrap());
        assert_eq!(buffer, b"1234");
        let err = read_message(&mut reader, &mut buffer, 4).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(buffer.len(), 5);
    }

    #[test]
    fn missing_delimiter_cannot_allocate_past_limit_plus_one() {
        let input = vec![b'x'; 1024 * 1024];
        let mut reader = Cursor::new(input);
        let mut buffer = Vec::new();
        assert_eq!(read_message(&mut reader, &mut buffer, 1024).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(reader.position(), 1025);
        assert_eq!(buffer.len(), 1025);
    }

    #[test]
    fn incomplete_eof_never_delivers_a_message() {
        let mut reader = Cursor::new(b"complete\0partial");
        let mut buffer = Vec::new();
        assert!(read_message(&mut reader, &mut buffer, 8).unwrap());
        assert_eq!(buffer, b"complete");
        assert!(!read_message(&mut reader, &mut buffer, 8).unwrap());
        assert!(!read_message(&mut reader, &mut buffer, 8).unwrap());
    }

    #[test]
    fn malformed_json_is_a_frame_and_does_not_consume_next_frame() {
        let mut reader = Cursor::new(b"bad\0{\"id\":7}\0");
        let mut buffer = Vec::new();
        assert!(read_message(&mut reader, &mut buffer, 64).unwrap());
        assert!(serde_json::from_slice::<serde_json::Value>(&buffer).is_err());
        assert!(read_message(&mut reader, &mut buffer, 64).unwrap());
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&buffer).unwrap()["id"], 7);
    }

    #[test]
    fn large_frame_round_trips_with_reused_buffer() {
        let mut payload = vec![b'x'; 1024 * 1024];
        payload.push(0);
        payload.extend_from_slice(b"next\0");
        let mut reader = BufReader::with_capacity(32768, Cursor::new(payload));
        let mut buffer = Vec::new();
        assert!(read_message(&mut reader, &mut buffer, 1024 * 1024).unwrap());
        assert_eq!(buffer.len(), 1024 * 1024);
        assert!(buffer.iter().all(|b| *b == b'x'));
        let capacity = buffer.capacity();
        assert!(read_message(&mut reader, &mut buffer, 1024 * 1024).unwrap());
        assert_eq!(buffer, b"next");
        assert_eq!(buffer.capacity(), capacity);
    }
}
