//! UART transport: 115200 8N1, no flow control. TX prefixes each frame with
//! the SOF bytes `B0 07`; there is no trailer and no escaping (the protocol
//! is strict ping-pong, so a corrupt lock simply fails the frame CRC).
//!
//! RX is a host-side mirror of the firmware's SOF-hunt state machine: hunt
//! for `B0 07`, read the 4-byte header, validate the declared length, then
//! read payload + CRC. A mid-frame gap longer than OB_UART_INTERBYTE_MS
//! resets the parser to hunting (resync only — the encompassing command
//! deadline still bounds the total wait).

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use super::{xfer_link, FrameLink, Transport, TransportError};
use crate::proto::consts::{
    OB_FRAME_CRC_LEN, OB_FRAME_HDR_LEN, OB_MAX_PAYLOAD, OB_UART_BAUD, OB_UART_INTERBYTE_MS,
    OB_UART_SOF1, OB_UART_SOF2,
};
use crate::proto::Frame;

/// Bytes per write burst handed to the OS, and see `chunk_drain()` for the
/// pause after each.
///
/// Some links lose bytes when a frame is handed over as one long write.
/// Measured on a WCH-LinkE CDC bridge driving a CH570 over PA2/PA3: 15-byte
/// HELLO frames always land, but a 16 KiB image (342 x 62-byte WRITE frames)
/// failed 3/3 runs, each at a different offset, and all three protocol
/// retries of the failing frame failed too. Writing in small chunks and
/// letting each drain fixes it, reproducibly:
///
///     pacing on (default)     3/3 PASS, 16384 B in 6-7 s
///     OPENBOOT_UART_CHUNK=0   2/2 FAIL at 0x2240, 0x2000
///
/// What is NOT established is where the bytes die. The obvious reading is
/// that the bridge drops long bursts, but the bench evidence does not pin
/// that down: reading the device's UART status after a failure showed LSR
/// 0x60 / RX FIFO 0, and the error bits there are read-to-clear, so a
/// per-byte framing or overrun error would already have been consumed by the
/// firmware's own RBR drain and by the retries that ran before the probe
/// read. A device-side fault that is merely timing-sensitive fits the same
/// data. Chunking also changes two things at once (smaller writes AND idle
/// time between them), so it does not discriminate either. Treat this as a
/// working mitigation with a confirmed A/B, not a root cause.
///
/// Pacing costs ~8 ms on a 62-byte WRITE, so set `OPENBOOT_UART_CHUNK=0` to
/// send each frame in one write when the link is known good.
const UART_WRITE_CHUNK: usize = 16;

/// Parse an `OPENBOOT_UART_CHUNK` value. `None` selects the safe paced default;
/// only an explicit `0` sends each frame in one write. Pure so the table below
/// is testable without touching the ambient environment — a test that reads the
/// real variable fails for whoever has it set, which is exactly the escape hatch
/// this fix documents.
fn parse_chunk(v: Option<&str>) -> Option<usize> {
    match v {
        None => Some(UART_WRITE_CHUNK),
        Some(s) => match s.trim().parse::<usize>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            // Unparsable: keep pacing rather than silently going fast. A
            // dropped byte is invisible until a flash verify fails.
            Err(_) => Some(UART_WRITE_CHUNK),
        },
    }
}

fn write_chunk() -> Option<usize> {
    parse_chunk(std::env::var("OPENBOOT_UART_CHUNK").ok().as_deref())
}

/// The writes `send_frame` will issue for a `total`-byte buffer: `(len, pause
/// after it)`. Split out from the IO so the "no pause after the last chunk"
/// boundary is testable — asserting on arithmetic inline only restates it.
fn write_plan(total: usize, chunk: Option<usize>) -> Vec<(usize, bool)> {
    match chunk {
        None => vec![(total, false)],
        Some(c) => {
            let n = total.div_ceil(c);
            (0..n)
                .map(|i| {
                    let len = if i + 1 == n { total - i * c } else { c };
                    (len, i + 1 < n)
                })
                .collect()
        }
    }
}

/// Timeout restored before every send. Generous: it bounds a pathological
/// blocked port, not normal operation, where a full 62-byte frame is 5.4 ms
/// of wire time.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long `n` bytes occupy the wire at the fixed OBP line settings, plus a
/// margin, so the next burst starts against a drained bridge buffer. 8N1 is
/// 10 bits per byte including start and stop.
fn chunk_drain(n: usize) -> Duration {
    let micros = (n as u64 * 10 * 1_000_000) / u64::from(OB_UART_BAUD);
    Duration::from_micros(micros + micros / 2 + 200)
}

pub struct UartTransport {
    port: Box<dyn SerialPort>,
    /// Port path for logging.
    pub path: String,
}

impl UartTransport {
    /// Open `path` at the fixed OBP settings and drain stale input.
    pub fn open(path: &str) -> Result<UartTransport> {
        let port = serialport::new(path, OB_UART_BAUD)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(Duration::from_millis(OB_UART_INTERBYTE_MS))
            .open()
            .with_context(|| format!("open serial port {path}"))?;
        port.clear(ClearBuffer::Input)
            .with_context(|| format!("drain serial input on {path}"))?;
        Ok(UartTransport {
            port,
            path: path.to_string(),
        })
    }

    /// Read one byte, waiting at most `wait`. `Ok(None)` on timeout.
    fn read_byte(&mut self, wait: Duration) -> Result<Option<u8>> {
        self.port
            .set_timeout(wait.max(Duration::from_millis(1)))
            .context("set serial timeout")?;
        let mut byte = [0u8; 1];
        match self.port.read(&mut byte) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(byte[0])),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(e).context("serial read"),
        }
    }

    /// Read one in-frame byte: bounded by both the inter-byte gap and the
    /// overall deadline. `Ok(None)` means "gap or deadline" — the caller
    /// resyncs, and the SOF hunt then notices an expired deadline.
    fn read_frame_byte(&mut self, deadline: Instant) -> Result<Option<u8>> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let interbyte = Duration::from_millis(OB_UART_INTERBYTE_MS);
        self.read_byte(interbyte.min(remaining))
    }
}

impl FrameLink for UartTransport {
    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        // One contiguous buffer so the SOF and the frame are chunked on the
        // same boundaries a single write would have used; splitting them
        // separately would put a pause after 2 bytes on every frame.
        let mut out = Vec::with_capacity(2 + frame.len());
        out.extend_from_slice(&[OB_UART_SOF1, OB_UART_SOF2]);
        out.extend_from_slice(frame);

        // serialport's write() and flush() wait on the SAME timeout field that
        // set_timeout() sets, and read_byte() drives that down to as little as
        // 1 ms as a command deadline closes. Without restoring it here, a send
        // that follows a timed-out read inherits the 1 ms budget and can fail
        // as an IO error, which the retry layer does not treat as retryable.
        self.port
            .set_timeout(WRITE_TIMEOUT)
            .context("set serial write timeout")?;

        let mut sent = 0usize;
        for (len, pause) in write_plan(out.len(), write_chunk()) {
            self.port
                .write_all(&out[sent..sent + len])
                .context("serial write frame")?;
            self.port.flush().context("serial flush")?;
            sent += len;
            // No pause after the final chunk: the response wait that follows
            // already gives the bridge time to drain.
            if pause {
                std::thread::sleep(chunk_drain(len));
            }
        }
        Ok(())
    }

    fn recv_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        'resync: loop {
            // --- SOF hunt -------------------------------------------------
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
                match self.read_byte(wait)? {
                    Some(OB_UART_SOF1) => sof1_seen = true,
                    Some(OB_UART_SOF2) if sof1_seen => break,
                    Some(_) => sof1_seen = false,
                    // Gap after a lone SOF1: drop the arm and keep hunting;
                    // gap with no arm means the wait spanned the deadline.
                    None => {
                        if !sof1_seen {
                            return Ok(None);
                        }
                        sof1_seen = false;
                    }
                }
            }

            // --- header ---------------------------------------------------
            let mut frame =
                Vec::with_capacity(OB_FRAME_HDR_LEN + OB_MAX_PAYLOAD + OB_FRAME_CRC_LEN);
            for _ in 0..OB_FRAME_HDR_LEN {
                match self.read_frame_byte(deadline)? {
                    Some(b) => frame.push(b),
                    None => continue 'resync,
                }
            }
            let len = usize::from(frame[2]);
            if len > OB_MAX_PAYLOAD {
                // Desynchronized lock (SOF bytes inside other data): re-hunt.
                continue 'resync;
            }

            // --- payload + CRC --------------------------------------------
            for _ in 0..len + OB_FRAME_CRC_LEN {
                match self.read_frame_byte(deadline)? {
                    Some(b) => frame.push(b),
                    None => continue 'resync,
                }
            }
            return Ok(Some(frame));
        }
    }
}

impl Transport for UartTransport {
    fn xfer(&mut self, req: &Frame, timeout: Duration) -> Result<Frame, TransportError> {
        xfer_link(self, req, timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_override_parses() {
        // Unset keeps the safe default; only an explicit 0 disables pacing.
        assert_eq!(parse_chunk(None), Some(UART_WRITE_CHUNK));
        assert_eq!(parse_chunk(Some("0")), None);
        assert_eq!(parse_chunk(Some("1")), Some(1));
        assert_eq!(parse_chunk(Some("  16  ")), Some(16));
        // Garbage and negatives must NOT silently disable pacing: dropped
        // bytes are invisible until a verify fails, so fail safe, not fast.
        assert_eq!(parse_chunk(Some("abc")), Some(UART_WRITE_CHUNK));
        assert_eq!(parse_chunk(Some("-1")), Some(UART_WRITE_CHUNK));
        assert_eq!(parse_chunk(Some("")), Some(UART_WRITE_CHUNK));
    }

    /// The real boundary: every byte written exactly once, and the last chunk
    /// never followed by a pause.
    #[test]
    fn write_plan_covers_every_byte_and_never_trails_a_pause() {
        for total in 1..=200usize {
            for chunk in [None, Some(1), Some(7), Some(16), Some(64)] {
                let plan = write_plan(total, chunk);
                assert_eq!(
                    plan.iter().map(|(l, _)| l).sum::<usize>(),
                    total,
                    "total={total} chunk={chunk:?} does not cover the buffer"
                );
                assert!(plan.iter().all(|(l, _)| *l > 0), "zero-length write");
                assert!(
                    !plan.last().unwrap().1,
                    "total={total} chunk={chunk:?} pauses after the last chunk"
                );
                assert_eq!(
                    plan.iter().filter(|(_, p)| *p).count(),
                    plan.len() - 1,
                    "every chunk but the last should pause"
                );
            }
        }
    }

    /// A 62-byte WRITE frame plus its 2-byte SOF is 4 writes of 16, and the
    /// exact-multiple case must not emit a trailing empty chunk.
    #[test]
    fn write_plan_shapes() {
        assert_eq!(
            write_plan(64, Some(16)),
            vec![(16, true), (16, true), (16, true), (16, false)]
        );
        assert_eq!(write_plan(20, Some(16)), vec![(16, true), (4, false)]);
        assert_eq!(write_plan(10, Some(16)), vec![(10, false)]);
        assert_eq!(write_plan(64, None), vec![(64, false)]);
    }

    /// The pause has to cover at least the wire time of the bytes just
    /// written, computed independently of chunk_drain's own expression.
    #[test]
    fn drain_covers_wire_time() {
        // 115200 8N1 => 10 bits/byte => ~86.8 us per byte.
        for (n, min_us) in [(1usize, 86u128), (16, 1388), (48, 4166), (62, 5381)] {
            assert!(
                chunk_drain(n).as_micros() >= min_us,
                "drain for {n} B is shorter than its wire time"
            );
        }
    }
}
