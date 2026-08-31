//! Host-side mirror of the firmware's SOF-hunt parser, over any byte source.
//!
//! The device-facing framing is the same whether the bytes reach the chip
//! through a serial port or through an intermediary that forwards them: hunt
//! for `B0 07`, read the 4-byte header, validate the declared length, then read
//! payload + CRC. Only the byte source differs, so it is the only thing the
//! transports supply.
//!
//! One semantic note on `OB_UART_INTERBYTE_MS`. For the device's own parser it
//! is normative (PROTOCOL.md section 4.2). Here it is a host-side resync
//! heuristic, because the host measures gaps after USB polling and whatever
//! scheduling sits between it and the wire. It is still the right number: a
//! full 64-byte frame is 5.6 ms of wire time at 115200, so a gap this long
//! genuinely means the rest is not coming.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::proto::consts::{
    OB_FRAME_CRC_LEN, OB_FRAME_HDR_LEN, OB_MAX_PAYLOAD, OB_UART_INTERBYTE_MS, OB_UART_SOF1,
    OB_UART_SOF2,
};

/// A stream of bytes arriving from the device.
pub trait ByteSource {
    /// Block at most `wait` for one byte. `Ok(None)` means nothing arrived
    /// within `wait` — a gap, or an expired budget — never "not yet".
    fn next_byte(&mut self, wait: Duration) -> Result<Option<u8>>;
}

/// `B0 07` + the logical frame. No trailer, no escaping: the protocol is
/// strict ping-pong, so a corrupt lock simply fails the frame CRC.
pub fn encode_sof(frame: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + frame.len());
    out.extend_from_slice(&[OB_UART_SOF1, OB_UART_SOF2]);
    out.extend_from_slice(frame);
    out
}

/// Read one in-frame byte: bounded by both the inter-byte gap and the overall
/// deadline. `Ok(None)` means "gap or deadline" — the caller resyncs, and the
/// SOF hunt then notices an expired deadline.
fn next_frame_byte<S: ByteSource + ?Sized>(src: &mut S, deadline: Instant) -> Result<Option<u8>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(None);
    }
    let interbyte = Duration::from_millis(OB_UART_INTERBYTE_MS);
    src.next_byte(interbyte.min(remaining))
}

/// Pull one complete frame out of `src`, or `Ok(None)` once `deadline` passes.
pub fn recv_sof_frame<S: ByteSource + ?Sized>(
    src: &mut S,
    deadline: Instant,
) -> Result<Option<Vec<u8>>> {
    'resync: loop {
        // --- SOF hunt -----------------------------------------------------
        // A repeated 0xB0 keeps us armed: `B0 B0 07` still locks.
        let mut sof1_seen = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let wait = if sof1_seen {
                Duration::from_millis(OB_UART_INTERBYTE_MS).min(remaining)
            } else {
                remaining
            };
            match src.next_byte(wait)? {
                Some(OB_UART_SOF1) => sof1_seen = true,
                Some(OB_UART_SOF2) if sof1_seen => break,
                Some(_) => sof1_seen = false,
                // Gap after a lone SOF1: drop the arm and keep hunting; gap
                // with no arm means the wait spanned the deadline.
                None => {
                    if !sof1_seen {
                        return Ok(None);
                    }
                    sof1_seen = false;
                }
            }
        }

        // --- header -------------------------------------------------------
        let mut frame = Vec::with_capacity(OB_FRAME_HDR_LEN + OB_MAX_PAYLOAD + OB_FRAME_CRC_LEN);
        for _ in 0..OB_FRAME_HDR_LEN {
            match next_frame_byte(src, deadline)? {
                Some(b) => frame.push(b),
                None => continue 'resync,
            }
        }
        let len = usize::from(frame[2]);
        if len > OB_MAX_PAYLOAD {
            // Desynchronized lock (SOF bytes inside other data): re-hunt.
            continue 'resync;
        }

        // --- payload + CRC ------------------------------------------------
        for _ in 0..len + OB_FRAME_CRC_LEN {
            match next_frame_byte(src, deadline)? {
                Some(b) => frame.push(b),
                None => continue 'resync,
            }
        }
        return Ok(Some(frame));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Normative vectors, shared with the codec and firmware tests.
    const GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../protocol/golden_frames.txt"
    ));

    fn vector(name: &str) -> Vec<u8> {
        for line in GOLDEN.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (n, hex) = line.split_once(':').expect("golden line is `name: hex`");
            if n.trim() == name {
                let hex = hex.trim();
                return (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
                    .collect();
            }
        }
        panic!("missing golden vector {name}");
    }

    /// `None` entries are explicit gaps; an exhausted source keeps returning
    /// `None`, which is what a silent link looks like.
    struct VecSource {
        items: VecDeque<Option<u8>>,
    }

    impl VecSource {
        fn bytes(b: &[u8]) -> VecSource {
            VecSource {
                items: b.iter().map(|x| Some(*x)).collect(),
            }
        }
        fn script(items: Vec<Option<u8>>) -> VecSource {
            VecSource {
                items: items.into(),
            }
        }
    }

    impl ByteSource for VecSource {
        fn next_byte(&mut self, _wait: Duration) -> Result<Option<u8>> {
            Ok(self.items.pop_front().flatten())
        }
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    fn recv(bytes: &[u8]) -> Option<Vec<u8>> {
        recv_sof_frame(&mut VecSource::bytes(bytes), far()).expect("no IO error")
    }

    #[test]
    fn encodes_the_sof_prefix_and_nothing_else() {
        assert_eq!(
            encode_sof(&[1, 2, 3]),
            vec![OB_UART_SOF1, OB_UART_SOF2, 1, 2, 3]
        );
        assert_eq!(encode_sof(&[]), vec![OB_UART_SOF1, OB_UART_SOF2]);
    }

    #[test]
    fn round_trips_every_golden_vector() {
        for name in [
            "hello_req",
            "hello_resp_ch592_usb",
            "write_req_max",
            "min_frame",
        ] {
            let frame = vector(name);
            assert_eq!(
                recv(&encode_sof(&frame)),
                Some(frame.clone()),
                "vector {name} did not survive the framing round trip"
            );
        }
    }

    #[test]
    fn skips_leading_garbage() {
        let frame = vector("hello_req");
        let mut wire = vec![0x00, 0xFF, 0x5B, 0xA6, 0x61];
        wire.extend_from_slice(&encode_sof(&frame));
        assert_eq!(recv(&wire), Some(frame));
    }

    /// A repeated SOF1 must keep the parser armed, or `B0 B0 07` never locks.
    #[test]
    fn a_repeated_sof1_stays_armed() {
        let frame = vector("hello_req");
        let mut wire = vec![OB_UART_SOF1, OB_UART_SOF1];
        wire.extend_from_slice(&encode_sof(&frame)[1..]);
        assert_eq!(recv(&wire), Some(frame));
    }

    /// A false lock on SOF bytes inside other data declares an impossible
    /// length; the parser must re-hunt rather than consume 200 bytes.
    #[test]
    fn an_over_long_declared_length_forces_a_rehunt() {
        let frame = vector("hello_req");
        let mut wire = vec![OB_UART_SOF1, OB_UART_SOF2, 0x01, 0x00, 0xFF, 0x00];
        wire.extend_from_slice(&encode_sof(&frame));
        assert_eq!(recv(&wire), Some(frame));
    }

    /// SOF bytes appearing inside a legitimate payload must not be mistaken
    /// for the start of another frame.
    #[test]
    fn sof_bytes_inside_a_payload_are_payload() {
        let frame = vec![
            0x81,
            0x00,
            0x04,
            0x00,
            OB_UART_SOF1,
            OB_UART_SOF2,
            0xB0,
            0x07,
            1,
            2,
            3,
            4,
        ];
        assert_eq!(recv(&encode_sof(&frame)), Some(frame));
    }

    #[test]
    fn an_expired_deadline_yields_none_without_reading() {
        let mut src = VecSource::bytes(&encode_sof(&vector("hello_req")));
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(recv_sof_frame(&mut src, past).expect("no IO error"), None);
    }

    #[test]
    fn a_silent_source_yields_none() {
        assert_eq!(recv(&[]), None);
        assert_eq!(recv(&[0x11, 0x22, 0x33]), None);
    }

    /// A gap mid-frame abandons the partial frame; the next complete one on
    /// the wire still arrives.
    #[test]
    fn a_mid_frame_gap_resyncs_onto_the_next_frame() {
        let frame = vector("hello_req");
        let mut items: Vec<Option<u8>> =
            vec![Some(OB_UART_SOF1), Some(OB_UART_SOF2), Some(0x01), None];
        items.extend(encode_sof(&frame).iter().map(|b| Some(*b)));

        let mut src = VecSource::script(items);
        assert_eq!(
            recv_sof_frame(&mut src, far()).expect("no IO error"),
            Some(frame)
        );
    }

    /// Two frames back to back: the second must still be readable, which is
    /// what makes a stale response discardable by the layer above.
    #[test]
    fn consecutive_frames_are_read_one_at_a_time() {
        let a = vector("hello_req");
        let b = vector("min_frame");
        let mut wire = encode_sof(&a);
        wire.extend_from_slice(&encode_sof(&b));

        let mut src = VecSource::bytes(&wire);
        assert_eq!(recv_sof_frame(&mut src, far()).unwrap(), Some(a));
        assert_eq!(recv_sof_frame(&mut src, far()).unwrap(), Some(b));
        assert_eq!(recv_sof_frame(&mut src, far()).unwrap(), None);
    }

    /// The frame is delivered whatever the read boundaries are, which is the
    /// property a report-framed byte stream depends on.
    #[test]
    fn arbitrary_gaps_between_bytes_do_not_matter_while_armed() {
        let frame = vector("write_req_max");
        let wire = encode_sof(&frame);
        // A gap after every byte, each shorter than the interbyte limit as far
        // as the source is concerned: the parser only sees `Ok(Some(..))`.
        let items: Vec<Option<u8>> = wire.iter().map(|b| Some(*b)).collect();
        let mut src = VecSource::script(items);
        assert_eq!(recv_sof_frame(&mut src, far()).unwrap(), Some(frame));
    }
}
