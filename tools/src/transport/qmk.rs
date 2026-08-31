//! QMK tunnel transport: the bootloader's UART reached through a keyboard's
//! vendor HID interface, so a CH592 module soldered to a keyboard can be
//! updated without wiring up its serial pins.
//!
//! The keyboard is a dumb byte bridge. It parses none of this protocol; it
//! moves bytes and answers a handful of control reports. So the device-facing
//! framing here is exactly the UART framing (see `framing.rs`), and everything
//! above `Transport` is unaware the tunnel exists.
//!
//! Report layout, both directions, 64 bytes:
//!
//!     byte 0   tag   0x00 control, 0x01 byte stream
//!     byte 1   len   payload length, 0..62
//!     byte 2.. payload
//!
//! The tag byte is what lets a control response interleave into the stream on
//! the same endpoint without in-band escaping.
//!
//! ## The deadline this transport exists to beat
//!
//! Telling the module to enter the bootloader reboots it, and the bootloader
//! boots back into the application about ten seconds later unless a HELLO
//! succeeds first — no other traffic resets that deadline. Its UART is also
//! unreliable for the first couple of seconds after the reset. So instead of
//! sleeping a fixed settle, this transport asks the keyboard how long it has
//! been since the module acknowledged (the keyboard holds the only usable
//! anchor: the host cannot see the reset) and probes HELLO across the window.
//! The probe that succeeds is what disables the deadline, so it is worth
//! landing early.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use hidapi::HidDevice;

use super::framing::{self, ByteSource};
use super::hidsel::{self, UsageFilter};
use super::{Transport, TransportError};
use crate::proto::consts::{
    OB_BOOT_APP, OB_CMD_BOOT, OB_CMD_HELLO, OB_PROTO_MAJOR, OB_PROTO_MINOR,
};
use crate::proto::{self, Frame};

/// The tunnel's HID interface. NOT a protocol constant: this is the keyboard's
/// contract, defined by the QMK bridge source, and overridable on the command
/// line for a product that picks a different pair.
pub const TUNNEL_USAGE_PAGE: u16 = 0xFF61;
pub const TUNNEL_USAGE: u16 = 0x0062;

const REPORT_LEN: usize = 64;
const STREAM_MAX: usize = REPORT_LEN - 2;

const TAG_CONTROL: u8 = 0x00;
const TAG_STREAM: u8 = 0x01;

// The whole opcode and state sets are spelled out, whether or not the host
// exercises every value, so this file stays readable against the bridge source.
#[allow(dead_code)]
const OP_IDENTIFY: u8 = 0x00;
const OP_ENTER: u8 = 0x01;
const OP_STATUS: u8 = 0x02;
const OP_EXIT: u8 = 0x03;

/// Guards against a stray report costing the wireless link ten seconds.
const ENTER_MAGIC: u32 = 0x4F42_4231;
/// Enter passthrough without waiting for an acknowledgement, for when the
/// module is already sitting in the bootloader and can never send one.
const ENTER_FLAG_FORCE: u8 = 0x01;

#[allow(dead_code)]
const ST_IDLE: u8 = 0;
const ST_SETTLING: u8 = 3;
const ST_PASSTHRU: u8 = 4;
const ST_ERROR: u8 = 5;

const OBB_OK: u8 = 0;
const OBB_E_MAGIC: u8 = 1;
const OBB_E_STATE: u8 = 2;
const OBB_E_ARG: u8 = 3;
const OBB_E_BUSY: u8 = 4;
const OBB_E_NO_ACK: u8 = 5;

fn describe_bridge_status(status: u8) -> &'static str {
    match status {
        OBB_OK => "ok",
        OBB_E_MAGIC => "bad magic",
        OBB_E_STATE => "wrong state (already bridging?)",
        OBB_E_ARG => "bad argument",
        OBB_E_BUSY => "busy",
        OBB_E_NO_ACK => "the module never acknowledged",
        _ => "unknown error",
    }
}

/// Tunable so tests need no fake clock, and so a bench can chase the real
/// post-reset settle without reflashing the keyboard.
#[derive(Clone, Debug)]
pub struct QmkOpts {
    /// Settle the keyboard is asked to apply. Deliberately below the observed
    /// clean-from figure: a failed probe costs one short timeout, so probing
    /// early is nearly free and finds the real edge.
    pub settle_hint_ms: u16,
    /// Earliest probe, measured from the acknowledgement.
    pub settle_min: Duration,
    /// One probe's budget. Short: a frame lost in the noisy window should cost
    /// this, not a full command timeout.
    pub probe_timeout: Duration,
    pub probe_gap: Duration,
    /// Hard stop, measured from the acknowledgement. Leaves enough of the
    /// bootloader's budget that the error prints while it is still up.
    pub enter_deadline: Duration,
    /// How long the keyboard has to answer ENTER and reach settling.
    pub enter_ack_timeout: Duration,
    pub status_poll: Duration,
}

impl Default for QmkOpts {
    fn default() -> QmkOpts {
        QmkOpts {
            settle_hint_ms: 1500,
            settle_min: Duration::from_millis(1500),
            probe_timeout: Duration::from_millis(300),
            probe_gap: Duration::from_millis(100),
            enter_deadline: Duration::from_millis(7000),
            enter_ack_timeout: Duration::from_millis(1500),
            status_poll: Duration::from_millis(50),
        }
    }
}

/// The report pipe, so the handshake and framing are testable without hidapi.
pub(crate) trait ReportLink {
    fn write_report(&mut self, report: &[u8]) -> Result<()>;
    /// Bytes read, or 0 on timeout.
    fn read_report(&mut self, buf: &mut [u8], timeout_ms: i32) -> Result<usize>;
}

struct HidLink(HidDevice);

impl ReportLink for HidLink {
    fn write_report(&mut self, report: &[u8]) -> Result<()> {
        // hidapi wants a leading report-ID byte; the interface uses none.
        let mut out = [0u8; 1 + REPORT_LEN];
        out[1..].copy_from_slice(report);
        self.0.write(&out).context("QMK HID write")?;
        Ok(())
    }

    fn read_report(&mut self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        self.0.read_timeout(buf, timeout_ms).context("QMK HID read")
    }
}

#[derive(Debug, Clone, Copy)]
struct BridgeStatus {
    state: u8,
    elapsed_ms: u32,
    last_error: u8,
}

fn parse_status(payload: &[u8]) -> Option<BridgeStatus> {
    if payload.len() < 9 || payload[0] != OP_STATUS {
        return None;
    }
    Some(BridgeStatus {
        state: payload[2],
        elapsed_ms: u32::from_le_bytes([payload[3], payload[4], payload[5], payload[6]]),
        last_error: payload[8],
    })
}

/// The instant the module acknowledged, `elapsed_ms` ago. The value is device
/// reported, so a malformed or absurd one (a stuck `0xFFFF_FFFF`, ~49 days)
/// must not underflow the host's monotonic clock — that panics on some
/// platforms. Clamp such a value to "now": at worst the settle window is
/// measured from a hair late, never from a time before the host booted.
fn anchor_from_elapsed(elapsed_ms: u32) -> Instant {
    Instant::now()
        .checked_sub(Duration::from_millis(u64::from(elapsed_ms)))
        .unwrap_or_else(Instant::now)
}

/// One demultiplexed report. `Ignored` is distinct from "nothing arrived":
/// mistaking a control report for silence would abandon a half-read frame.
enum Rx {
    Stream(usize),
    Control(Vec<u8>),
    Ignored,
}

enum EnterError {
    /// The module never answered, which is also what "already in the
    /// bootloader" looks like from here.
    NoAck,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for EnterError {
    fn from(e: anyhow::Error) -> EnterError {
        EnterError::Other(e)
    }
}

pub struct QmkTransport {
    link: Box<dyn ReportLink>,
    rx: VecDeque<u8>,
    /// Human-readable device path for logging.
    pub path: String,
    /// How long the bootloader took to answer, measured from the acknowledgement.
    pub settled: Duration,
    /// The bridge is in passthrough (module taken off air) — set the moment
    /// ENTER succeeds or an open tunnel is adopted, BEFORE the HELLO probe. It
    /// is what teardown keys off: even a construction that fails at the probe
    /// has to send OP_EXIT, or the keyboard stays off air until the bridge's
    /// own ten second watchdog fires.
    passthrough: bool,
    /// A HELLO succeeded, so the module is confirmed in the bootloader with its
    /// idle auto-boot disabled — teardown must launch the app to restore it.
    entered: bool,
    booted: bool,
}

impl QmkTransport {
    /// Open the keyboard's tunnel interface and take the module into the
    /// bootloader, leaving a link that is ready for an ordinary HELLO.
    pub fn open(
        vid: u16,
        pid: u16,
        serial: Option<&str>,
        filter: UsageFilter,
        opts: &QmkOpts,
    ) -> Result<QmkTransport> {
        let (dev, path) =
            hidsel::open_hid(vid, pid, serial, filter, "QMK OpenBoot tunnel interface")
                .context("does this keyboard's firmware include the OpenBoot bridge?")?;
        QmkTransport::with_link(Box::new(HidLink(dev)), path, opts)
    }

    pub(crate) fn with_link(
        link: Box<dyn ReportLink>,
        path: String,
        opts: &QmkOpts,
    ) -> Result<QmkTransport> {
        let mut transport = QmkTransport {
            link,
            rx: VecDeque::new(),
            path,
            settled: Duration::ZERO,
            passthrough: false,
            entered: false,
            booted: false,
        };
        transport.handshake(opts)?;
        Ok(transport)
    }

    /* --- control channel ------------------------------------------------ */

    fn write_control(&mut self, op: u8, args: &[u8]) -> Result<()> {
        let mut report = [0u8; REPORT_LEN];
        report[0] = TAG_CONTROL;
        report[1] = (1 + args.len()) as u8;
        report[2] = op;
        report[3..3 + args.len()].copy_from_slice(args);
        self.link.write_report(&report)
    }

    /// Read exactly one report, at most until `until`. `Ok(None)` means, and
    /// only means, that the wall clock passed the deadline.
    fn pump(&mut self, until: Instant) -> Result<Option<Rx>> {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
        let mut buf = [0u8; REPORT_LEN];
        let n = self.link.read_report(&mut buf, ms)?;
        // A short or empty report is malformed, never a silent link.
        if n < 2 {
            return Ok(Some(Rx::Ignored));
        }
        match buf[0] {
            TAG_STREAM => {
                let count = usize::from(buf[1]).min(STREAM_MAX).min(n - 2);
                self.rx.extend(&buf[2..2 + count]);
                Ok(Some(Rx::Stream(count)))
            }
            TAG_CONTROL => {
                let count = usize::from(buf[1]).min(STREAM_MAX).min(n - 2);
                Ok(Some(Rx::Control(buf[2..2 + count].to_vec())))
            }
            _ => Ok(Some(Rx::Ignored)),
        }
    }

    /// Next control payload, or `Ok(None)` at the deadline. Stream bytes seen
    /// on the way are buffered, not dropped.
    fn read_control(&mut self, until: Instant) -> Result<Option<Vec<u8>>> {
        loop {
            match self.pump(until)? {
                None => return Ok(None),
                Some(Rx::Control(payload)) if !payload.is_empty() => return Ok(Some(payload)),
                _ => continue,
            }
        }
    }

    /// One byte of the device stream. The only `Ok(None)` is the deadline.
    fn pump_byte(&mut self, wait: Duration) -> Result<Option<u8>> {
        if let Some(b) = self.rx.pop_front() {
            return Ok(Some(b));
        }
        let until = Instant::now() + wait;
        loop {
            match self.pump(until)? {
                None => return Ok(None),
                Some(Rx::Stream(0)) | Some(Rx::Control(_)) | Some(Rx::Ignored) => continue,
                Some(Rx::Stream(_)) => return Ok(self.rx.pop_front()),
            }
        }
    }

    /* --- handshake ------------------------------------------------------ */

    fn handshake(&mut self, opts: &QmkOpts) -> Result<()> {
        let control_deadline = Instant::now() + opts.enter_ack_timeout;
        self.write_control(OP_STATUS, &[])?;
        let initial = self
            .read_control(control_deadline)?
            .and_then(|p| parse_status(&p))
            .ok_or_else(|| {
                anyhow!(
                    "the keyboard did not answer the tunnel status request; is \
                     this the right interface, and does its firmware include \
                     the OpenBoot bridge?"
                )
            })?;

        let anchor = if initial.state == ST_PASSTHRU || initial.state == ST_SETTLING {
            // A previous run left the tunnel open; adopt its clock rather than
            // bouncing the module again.
            anchor_from_elapsed(initial.elapsed_ms)
        } else {
            match self.enter(false, opts) {
                Ok(t) => t,
                Err(EnterError::NoAck) => {
                    eprintln!(
                        "the module did not acknowledge; retrying on the \
                         assumption that it is already in the bootloader"
                    );
                    self.enter(true, opts).map_err(|e| match e {
                        EnterError::NoAck => anyhow!(
                            "the module never acknowledged, even when forced; \
                             is the keyboard's UART wired to it?"
                        ),
                        EnterError::Other(e) => e,
                    })?
                }
                Err(EnterError::Other(e)) => return Err(e),
            }
        };

        // The module is now off air behind the bridge. From here, every exit —
        // including a probe that never lands — must send OP_EXIT.
        self.passthrough = true;
        self.probe_until_alive(anchor, opts)
    }

    /// Ask the keyboard to take the module into the bootloader, and return the
    /// instant the module acknowledged.
    fn enter(&mut self, force: bool, opts: &QmkOpts) -> Result<Instant, EnterError> {
        let mut args = Vec::with_capacity(7);
        args.extend_from_slice(&ENTER_MAGIC.to_le_bytes());
        args.extend_from_slice(&opts.settle_hint_ms.to_le_bytes());
        args.push(if force { ENTER_FLAG_FORCE } else { 0 });
        self.write_control(OP_ENTER, &args)?;

        let give_up = Instant::now() + opts.enter_ack_timeout;
        let mut next_poll = Instant::now();
        loop {
            let now = Instant::now();
            if now >= give_up {
                return Err(EnterError::NoAck);
            }
            if now >= next_poll {
                self.write_control(OP_STATUS, &[])?;
                next_poll = now + opts.status_poll;
            }
            let Some(payload) = self.read_control(next_poll.min(give_up))? else {
                continue;
            };
            match payload[0] {
                // The keyboard defers this answer until the module replies, so
                // a failure here is authoritative.
                OP_ENTER if payload.len() >= 2 && payload[1] != OBB_OK => {
                    return Err(match payload[1] {
                        OBB_E_NO_ACK => EnterError::NoAck,
                        status => EnterError::Other(anyhow!(
                            "the keyboard refused to open the tunnel: {}",
                            describe_bridge_status(status)
                        )),
                    });
                }
                OP_STATUS => {
                    let Some(status) = parse_status(&payload) else {
                        continue;
                    };
                    if status.state == ST_SETTLING || status.state == ST_PASSTHRU {
                        return Ok(anchor_from_elapsed(status.elapsed_ms));
                    }
                    if status.state == ST_ERROR {
                        return Err(match status.last_error {
                            OBB_E_NO_ACK => EnterError::NoAck,
                            other => EnterError::Other(anyhow!(
                                "the keyboard's tunnel is in an error state: {}",
                                describe_bridge_status(other)
                            )),
                        });
                    }
                }
                _ => continue,
            }
        }
    }

    /// Probe HELLO across the post-reset window. The probe that succeeds is
    /// what disables the bootloader's idle deadline, so everything downstream
    /// is unconstrained once this returns.
    fn probe_until_alive(&mut self, anchor: Instant, opts: &QmkOpts) -> Result<()> {
        let settle_until = anchor + opts.settle_min;
        let give_up = anchor + opts.enter_deadline;

        let now = Instant::now();
        if now < settle_until {
            std::thread::sleep(settle_until - now);
        }

        // Counts down, while the real client counts up from 0, so a straggling
        // probe answer can never be mistaken for the answer to a real HELLO.
        let mut seq = 0xFFu8;
        loop {
            if Instant::now() >= give_up {
                bail!(
                    "the bootloader did not answer within {:?} of the module \
                     resetting; it auto-boots back into the application after \
                     about ten seconds, so try again, or use --transport uart \
                     against the module's serial pins",
                    opts.enter_deadline
                );
            }

            self.rx.clear();
            let req = Frame::new(
                OB_CMD_HELLO,
                seq,
                proto::hello_req_payload(OB_PROTO_MAJOR, OB_PROTO_MINOR),
            );
            seq = seq.wrapping_sub(1);

            match self.xfer(&req, opts.probe_timeout) {
                // A frame-error report proves the bootloader is up but says the
                // link is still noisy, and it does NOT disable the deadline.
                Ok(frame) if frame.is_frame_error() => {}
                Ok(_) => {
                    self.settled = anchor.elapsed();
                    self.rx.clear();
                    self.entered = true;
                    return Ok(());
                }
                Err(TransportError::Timeout) => {}
                Err(TransportError::Io(e)) => return Err(e),
            }
            std::thread::sleep(opts.probe_gap);
        }
    }
}

impl ByteSource for QmkTransport {
    fn next_byte(&mut self, wait: Duration) -> Result<Option<u8>> {
        self.pump_byte(wait)
    }
}

impl Transport for QmkTransport {
    // The tunnel disabled the module's idle auto-boot to hold the session, so a
    // module left in the bootloader would strand the keyboard off air rather
    // than wait harmlessly. flash --no-boot keys off this to park it safely.
    fn holds_in_bootloader(&self) -> bool {
        false
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        // Remember an explicit BOOT so the teardown below never overrides it;
        // `boot --stay` and `flash --no-boot` park the module deliberately.
        if frame.first() == Some(&OB_CMD_BOOT) {
            self.booted = true;
        }

        // Every frame the tool sends today fits one report, but the protocol
        // permits a 66-byte wire frame, so chunk rather than assume.
        //
        // No write pacing, unlike the UART backend: there is no CDC bridge to
        // outrun here, and the protocol is strict ping-pong, so the host cannot
        // issue a second request before the previous response arrives and can
        // never get ahead of the 115200 link.
        let out = framing::encode_sof(frame);
        for chunk in out.chunks(STREAM_MAX) {
            let mut report = [0u8; REPORT_LEN];
            report[0] = TAG_STREAM;
            report[1] = chunk.len() as u8;
            report[2..2 + chunk.len()].copy_from_slice(chunk);
            self.link.write_report(&report)?;
        }
        Ok(())
    }

    fn recv_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        framing::recv_sof_frame(self, deadline)
    }
}

impl Drop for QmkTransport {
    /// Opening this transport takes the keyboard off air, and a successful
    /// HELLO has by then disabled the bootloader's ten second auto-boot. So
    /// every exit path — including a read-only probe, a refused dry run, or an
    /// error anywhere in a flow — has to put the module back.
    fn drop(&mut self) {
        // Never opened the tunnel: nothing was taken off air, nothing to undo.
        if !self.passthrough {
            return;
        }

        // Launch the app only when a HELLO confirmed the module is in the
        // bootloader with its idle auto-boot disabled — then it will not return
        // on its own. If the probe never landed (entered == false) the module
        // still has a live idle timer, so leave it: OP_EXIT below hands the
        // keyboard back, and the module auto-boots itself.
        if self.entered && !self.booted {
            let frame = Frame::new(OB_CMD_BOOT, 0xF0, proto::boot_req_payload(OB_BOOT_APP));
            let _ = self.send_frame(&frame.encode());
            // BOOT's answer may legitimately never arrive: the device resets.
            let _ = self.recv_frame(Instant::now() + Duration::from_millis(400));
        }

        // The keyboard answers this from its own state machine, so it does not
        // depend on a module that is mid-reset and emitting noise. Reaching it
        // on every passthrough exit — success or a failed probe — is what keeps
        // the keyboard from waiting out the bridge's watchdog to come back.
        let _ = self.write_control(OP_EXIT, &[]);
        let _ = self.read_control(Instant::now() + Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::proto::consts::{OB_CMD_RESP_BIT, OB_OK};

    fn fast_opts() -> QmkOpts {
        QmkOpts {
            settle_hint_ms: 10,
            settle_min: Duration::ZERO,
            probe_timeout: Duration::from_millis(20),
            probe_gap: Duration::ZERO,
            enter_deadline: Duration::from_millis(300),
            enter_ack_timeout: Duration::from_millis(300),
            status_poll: Duration::from_millis(1),
        }
    }

    fn control(op: u8, args: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; REPORT_LEN];
        r[0] = TAG_CONTROL;
        r[1] = (1 + args.len()) as u8;
        r[2] = op;
        r[3..3 + args.len()].copy_from_slice(args);
        r
    }

    fn status_report(state: u8, elapsed_ms: u32, last_error: u8) -> Vec<u8> {
        let mut args = vec![OBB_OK, state];
        args.extend_from_slice(&elapsed_ms.to_le_bytes());
        args.push(0); // ocp link
        args.push(last_error);
        args.extend_from_slice(&0u16.to_le_bytes());
        args.extend_from_slice(&0u16.to_le_bytes());
        control(OP_STATUS, &args)
    }

    fn stream_reports(bytes: &[u8]) -> Vec<Vec<u8>> {
        bytes
            .chunks(STREAM_MAX)
            .map(|c| {
                let mut r = vec![0u8; REPORT_LEN];
                r[0] = TAG_STREAM;
                r[1] = c.len() as u8;
                r[2..2 + c.len()].copy_from_slice(c);
                r
            })
            .collect()
    }

    fn hello_response(seq: u8) -> Vec<u8> {
        Frame::new(
            OB_CMD_HELLO | OB_CMD_RESP_BIT,
            seq,
            crate::testutil::info_payload(0, 0x0007_0000),
        )
        .encode()
    }

    /// A scripted keyboard's reaction to one written report: the reports it
    /// hands back.
    type React = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>> + Send>;

    #[derive(Default)]
    struct Script {
        /// Reports handed back, in order. Empty = read timeout.
        rx: VecDeque<Vec<u8>>,
        /// Every report the host wrote.
        written: Vec<Vec<u8>>,
        /// Called after each write so a test can react to what was sent.
        react: Option<React>,
    }

    #[derive(Clone)]
    struct ScriptedLink(Arc<Mutex<Script>>);

    impl ScriptedLink {
        fn new() -> ScriptedLink {
            ScriptedLink(Arc::new(Mutex::new(Script::default())))
        }
        fn on_write(&self, f: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) {
            self.0.lock().unwrap().react = Some(Box::new(f));
        }
        fn written(&self) -> Vec<Vec<u8>> {
            self.0.lock().unwrap().written.clone()
        }
        /// Control opcodes the host sent, in order.
        fn control_ops(&self) -> Vec<u8> {
            self.written()
                .iter()
                .filter(|r| r[0] == TAG_CONTROL)
                .map(|r| r[2])
                .collect()
        }
        /// The logical frames the host sent, de-framed from the stream reports.
        fn sent_frames(&self) -> Vec<Vec<u8>> {
            let mut bytes = Vec::new();
            for r in self.written().iter().filter(|r| r[0] == TAG_STREAM) {
                bytes.extend_from_slice(&r[2..2 + usize::from(r[1])]);
            }
            let mut src = SliceSource(bytes.into());
            let mut out = Vec::new();
            let far = Instant::now() + Duration::from_secs(30);
            while let Ok(Some(f)) = framing::recv_sof_frame(&mut src, far) {
                out.push(f);
            }
            out
        }
    }

    struct SliceSource(VecDeque<u8>);
    impl ByteSource for SliceSource {
        fn next_byte(&mut self, _wait: Duration) -> Result<Option<u8>> {
            Ok(self.0.pop_front())
        }
    }

    impl ReportLink for ScriptedLink {
        fn write_report(&mut self, report: &[u8]) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            s.written.push(report.to_vec());
            if let Some(mut react) = s.react.take() {
                let more = react(report);
                s.rx.extend(more);
                s.react = Some(react);
            }
            Ok(())
        }

        fn read_report(&mut self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize> {
            match self.0.lock().unwrap().rx.pop_front() {
                Some(r) => {
                    buf[..r.len()].copy_from_slice(&r);
                    Ok(r.len())
                }
                None => Ok(0),
            }
        }
    }

    /// A keyboard that answers STATUS, defers ENTER until "the module acked",
    /// and then replies to any stream frame with a HELLO response.
    fn happy_link(initial_state: u8) -> ScriptedLink {
        let link = ScriptedLink::new();
        let mut state = initial_state;
        let mut elapsed = 0u32;
        link.on_write(move |report| {
            if report[0] == TAG_CONTROL {
                return match report[2] {
                    OP_STATUS => {
                        let out = vec![status_report(state, elapsed, 0)];
                        elapsed += 5;
                        out
                    }
                    OP_ENTER => {
                        state = ST_SETTLING;
                        vec![control(OP_ENTER, &[OBB_OK])]
                    }
                    OP_EXIT => vec![control(OP_EXIT, &[OBB_OK])],
                    OP_IDENTIFY => vec![control(OP_IDENTIFY, &[OBB_OK])],
                    _ => vec![],
                };
            }
            // Stream: answer a HELLO probe, ignore anything else.
            let frame = &report[2..2 + usize::from(report[1])];
            if frame.len() > 4 && frame[0] == 0xB0 && frame[1] == 0x07 && frame[2] == OB_CMD_HELLO {
                return stream_reports(&framing::encode_sof(&hello_response(frame[3])));
            }
            vec![]
        });
        link
    }

    fn open(link: &ScriptedLink) -> Result<QmkTransport> {
        QmkTransport::with_link(Box::new(link.clone()), "test".into(), &fast_opts())
    }

    /* --- framing -------------------------------------------------------- */

    #[test]
    fn every_frame_length_survives_the_report_round_trip() {
        for len in 1..=OB_MAX_FRAME_FOR_TEST {
            let frame: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let wire = framing::encode_sof(&frame);
            let mut joined = Vec::new();
            for report in stream_reports(&wire) {
                assert_eq!(report.len(), REPORT_LEN);
                joined.extend_from_slice(&report[2..2 + usize::from(report[1])]);
            }
            assert_eq!(joined, wire, "len {len} did not round trip");
        }
    }
    const OB_MAX_FRAME_FOR_TEST: usize = 66;

    #[test]
    fn every_real_request_fits_a_single_report() {
        // The largest frame the tool actually emits is a max-size WRITE.
        let write = Frame::new(
            crate::proto::consts::OB_CMD_WRITE,
            0,
            proto::write_req_payload(0x2000, &[0u8; crate::proto::consts::OB_MAX_WRITE_DATA]),
        );
        assert_eq!(
            framing::encode_sof(&write.encode()).len(),
            STREAM_MAX,
            "a maximum WRITE should exactly fill one report"
        );
        assert_eq!(
            framing::encode_sof(&write.encode())
                .chunks(STREAM_MAX)
                .count(),
            1
        );
    }

    #[test]
    fn malformed_reports_yield_no_bytes_and_do_not_panic() {
        let link = ScriptedLink::new();
        {
            let mut s = link.0.lock().unwrap();
            let mut over = vec![0u8; REPORT_LEN];
            over[0] = TAG_STREAM;
            over[1] = 200; // len beyond the report
            s.rx.push_back(over);
            let mut short = vec![0u8; 1];
            short[0] = TAG_STREAM;
            s.rx.push_back(short);
            let mut unknown = vec![0u8; REPORT_LEN];
            unknown[0] = 0x7E;
            unknown[1] = 4;
            s.rx.push_back(unknown);
        }
        let mut t = QmkTransport {
            link: Box::new(link.clone()),
            rx: VecDeque::new(),
            path: "test".into(),
            settled: Duration::ZERO,
            passthrough: false,
            entered: false,
            booted: false,
        };
        // The over-long count is clamped to what the report actually holds, so
        // it yields zero bytes rather than reading past the buffer.
        let got = t.pump_byte(Duration::from_millis(5)).expect("no IO error");
        assert_eq!(got, Some(0));
    }

    /// The property the tag byte exists for: a control report arriving in the
    /// middle of a frame must not be read as data, and must not be read as
    /// silence either — the latter would abandon the partial frame.
    #[test]
    fn a_control_report_mid_frame_does_not_break_reassembly() {
        let frame = hello_response(0x11);
        let wire = framing::encode_sof(&frame);
        let link = ScriptedLink::new();
        {
            let mut s = link.0.lock().unwrap();
            s.rx.push_back(stream_reports(&wire[..4])[0].clone());
            s.rx.push_back(status_report(ST_PASSTHRU, 1234, 0));
            let mut empty = vec![0u8; REPORT_LEN];
            empty[0] = TAG_STREAM;
            empty[1] = 0;
            s.rx.push_back(empty);
            s.rx.push_back(stream_reports(&wire[4..])[0].clone());
        }
        let mut t = QmkTransport {
            link: Box::new(link),
            rx: VecDeque::new(),
            path: "test".into(),
            settled: Duration::ZERO,
            passthrough: false,
            entered: false,
            booted: false,
        };
        let got = t
            .recv_frame(Instant::now() + Duration::from_secs(5))
            .expect("no IO error");
        assert_eq!(got, Some(frame));
    }

    /* --- handshake ------------------------------------------------------ */

    #[test]
    fn the_handshake_enters_then_probes_and_reports_how_long_it_took() {
        let link = happy_link(ST_IDLE);
        let t = open(&link).expect("handshake should succeed");

        let ops = link.control_ops();
        assert!(ops.contains(&OP_ENTER), "ENTER was never sent: {ops:?}");
        assert!(ops.contains(&OP_STATUS), "STATUS was never polled: {ops:?}");

        // The magic has to be on the wire, or a stray report could do this.
        let enter = link
            .written()
            .into_iter()
            .find(|r| r[0] == TAG_CONTROL && r[2] == OP_ENTER)
            .expect("ENTER report");
        assert_eq!(
            u32::from_le_bytes([enter[3], enter[4], enter[5], enter[6]]),
            ENTER_MAGIC
        );
        assert_eq!(u16::from_le_bytes([enter[7], enter[8]]), 10);

        assert!(t.entered);
        drop(t);
    }

    /// A tunnel left open by an earlier run must be adopted, not bounced: a
    /// second ENTER would reset the module out from under a live bootloader.
    #[test]
    fn an_already_open_tunnel_is_adopted_without_re_entering() {
        let link = happy_link(ST_PASSTHRU);
        let t = open(&link).expect("handshake should succeed");

        assert!(
            !link.control_ops().contains(&OP_ENTER),
            "ENTER must not be sent when the tunnel is already open"
        );
        drop(t);
    }

    /// No acknowledgement is exactly what "already in the bootloader" looks
    /// like, so the retry must go out with FORCE rather than give up.
    #[test]
    fn a_missing_acknowledgement_is_retried_with_force() {
        let link = ScriptedLink::new();
        let mut state = ST_IDLE;
        link.on_write(move |report| {
            if report[0] == TAG_CONTROL {
                return match report[2] {
                    OP_STATUS => vec![status_report(state, 0, 0)],
                    OP_ENTER => {
                        if report[9] & ENTER_FLAG_FORCE != 0 {
                            state = ST_SETTLING;
                            vec![control(OP_ENTER, &[OBB_OK])]
                        } else {
                            vec![control(OP_ENTER, &[OBB_E_NO_ACK])]
                        }
                    }
                    OP_EXIT => vec![control(OP_EXIT, &[OBB_OK])],
                    _ => vec![],
                };
            }
            let frame = &report[2..2 + usize::from(report[1])];
            if frame.len() > 4 && frame[2] == OB_CMD_HELLO {
                return stream_reports(&framing::encode_sof(&hello_response(frame[3])));
            }
            vec![]
        });

        let t = open(&link).expect("the forced retry should succeed");
        let enters: Vec<Vec<u8>> = link
            .written()
            .into_iter()
            .filter(|r| r[0] == TAG_CONTROL && r[2] == OP_ENTER)
            .collect();
        assert_eq!(
            enters.len(),
            2,
            "expected a plain attempt then a forced one"
        );
        assert_eq!(enters[0][9] & ENTER_FLAG_FORCE, 0);
        assert_eq!(enters[1][9] & ENTER_FLAG_FORCE, ENTER_FLAG_FORCE);
        drop(t);
    }

    #[test]
    fn a_rejected_magic_is_reported_rather_than_retried() {
        let link = ScriptedLink::new();
        link.on_write(move |report| {
            if report[0] != TAG_CONTROL {
                return vec![];
            }
            match report[2] {
                OP_STATUS => vec![status_report(ST_IDLE, 0, 0)],
                OP_ENTER => vec![control(OP_ENTER, &[OBB_E_MAGIC])],
                _ => vec![],
            }
        });

        let Err(err) = open(&link) else {
            panic!("a refused ENTER must fail the open");
        };
        assert!(
            format!("{err:#}").contains("bad magic"),
            "unhelpful error: {err:#}"
        );
    }

    #[test]
    fn a_keyboard_that_never_answers_is_named_as_the_problem() {
        let link = ScriptedLink::new(); // answers nothing at all
        let Err(err) = open(&link) else {
            panic!("a silent interface must fail the open");
        };
        assert!(
            format!("{err:#}").contains("tunnel status"),
            "unhelpful error: {err:#}"
        );
    }

    /// The probe has to survive the post-reset noise window, and its sequence
    /// numbers must not collide with the real client's, which counts up from 0.
    #[test]
    fn the_probe_retries_past_corruption_and_uses_descending_sequences() {
        let link = ScriptedLink::new();
        let mut state = ST_IDLE;
        let mut probes = 0;
        link.on_write(move |report| {
            if report[0] == TAG_CONTROL {
                return match report[2] {
                    OP_STATUS => vec![status_report(state, 0, 0)],
                    OP_ENTER => {
                        state = ST_SETTLING;
                        vec![control(OP_ENTER, &[OBB_OK])]
                    }
                    OP_EXIT => vec![control(OP_EXIT, &[OBB_OK])],
                    _ => vec![],
                };
            }
            let frame = &report[2..2 + usize::from(report[1])];
            if frame.len() > 4 && frame[2] == OB_CMD_HELLO {
                probes += 1;
                // Two rounds of garbage, then a clean answer.
                if probes <= 2 {
                    return stream_reports(&[0xDE, 0xAD, 0xBE, 0xEF]);
                }
                return stream_reports(&framing::encode_sof(&hello_response(frame[3])));
            }
            vec![]
        });

        let t = open(&link).expect("the probe should converge");
        let seqs: Vec<u8> = link
            .sent_frames()
            .iter()
            .filter(|f| f[0] == OB_CMD_HELLO)
            .map(|f| f[1])
            .collect();
        assert_eq!(seqs, vec![0xFF, 0xFE, 0xFD]);
        assert!(
            seqs.iter().all(|s| *s > 0x80),
            "probe sequences must stay clear of the client's ascending range"
        );
        drop(t);
    }

    #[test]
    fn a_bootloader_that_never_answers_gives_up_with_actionable_advice() {
        let link = ScriptedLink::new();
        let mut state = ST_IDLE;
        link.on_write(move |report| {
            if report[0] != TAG_CONTROL {
                return vec![]; // never answer a probe
            }
            match report[2] {
                OP_STATUS => vec![status_report(state, 0, 0)],
                OP_ENTER => {
                    state = ST_SETTLING;
                    vec![control(OP_ENTER, &[OBB_OK])]
                }
                OP_EXIT => vec![control(OP_EXIT, &[OBB_OK])],
                _ => vec![],
            }
        });

        let Err(err) = open(&link) else {
            panic!("an unresponsive bootloader must fail");
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("auto-boots"), "unhelpful error: {msg}");
        assert!(msg.contains("--transport uart"), "no fallback named: {msg}");
    }

    /* --- teardown ------------------------------------------------------- */

    /// Every exit path has to put the module back into the application: by the
    /// time the probe succeeded, the ten second auto-boot is already disabled.
    #[test]
    fn dropping_an_open_tunnel_boots_the_application_then_exits() {
        let link = happy_link(ST_IDLE);
        let t = open(&link).expect("handshake should succeed");
        drop(t);

        let boots: Vec<Vec<u8>> = link
            .sent_frames()
            .into_iter()
            .filter(|f| f[0] == OB_CMD_BOOT)
            .collect();
        assert_eq!(boots.len(), 1, "teardown should send exactly one BOOT");
        assert_eq!(
            boots[0][4], OB_BOOT_APP,
            "teardown must boot the application"
        );
        assert_eq!(
            *link.control_ops().last().unwrap(),
            OP_EXIT,
            "EXIT must be the last thing on the wire"
        );
    }

    /// An explicit BOOT is the caller's decision — `boot --stay` and
    /// `flash --no-boot` park the module on purpose.
    #[test]
    fn an_explicit_boot_is_not_overridden_by_the_teardown() {
        let link = happy_link(ST_IDLE);
        let mut t = open(&link).expect("handshake should succeed");

        let explicit = Frame::new(OB_CMD_BOOT, 0x05, proto::boot_req_payload(1));
        t.send_frame(&explicit.encode()).expect("send BOOT");
        drop(t);

        let boots: Vec<u8> = link
            .sent_frames()
            .iter()
            .filter(|f| f[0] == OB_CMD_BOOT)
            .map(|f| f[4])
            .collect();
        assert_eq!(boots, vec![1], "the teardown must not add a second BOOT");
    }

    #[test]
    fn dropping_a_transport_that_never_opened_touches_nothing() {
        let link = ScriptedLink::new();
        let t = QmkTransport {
            link: Box::new(link.clone()),
            rx: VecDeque::new(),
            path: "test".into(),
            settled: Duration::ZERO,
            passthrough: false,
            entered: false,
            booted: false,
        };
        drop(t);
        assert!(link.written().is_empty());
    }

    /// ENTER lands (the module is off air) but the bootloader never answers a
    /// HELLO probe, so construction fails. The keyboard must still be handed
    /// back with OP_EXIT rather than left off air until the bridge's own ten
    /// second watchdog fires.
    #[test]
    fn a_failed_probe_still_exits_passthrough() {
        let link = ScriptedLink::new();
        let mut state = ST_IDLE;
        link.on_write(move |report| {
            if report[0] != TAG_CONTROL {
                return vec![]; // never answer a HELLO probe
            }
            match report[2] {
                OP_STATUS => vec![status_report(state, 0, 0)],
                OP_ENTER => {
                    state = ST_SETTLING;
                    vec![control(OP_ENTER, &[OBB_OK])]
                }
                OP_EXIT => vec![control(OP_EXIT, &[OBB_OK])],
                _ => vec![],
            }
        });

        assert!(open(&link).is_err(), "a probe that never lands must fail");
        assert!(
            link.control_ops().contains(&OP_EXIT),
            "a failed open must still send OP_EXIT to release the keyboard"
        );
    }

    /// The tunnel disables the module's idle auto-boot, so leaving it in the
    /// bootloader would strand the keyboard — `flash --no-boot` keys off this to
    /// park it with BOOT --stay instead.
    #[test]
    fn the_tunnel_does_not_hold_the_module_in_the_bootloader() {
        let link = happy_link(ST_IDLE);
        let t = open(&link).expect("handshake should succeed");
        assert!(!t.holds_in_bootloader());
    }

    /// A device-reported elapsed of any size must not underflow the monotonic
    /// clock (which panics on some platforms). Reaching the assertions at all
    /// proves the subtraction did not panic.
    #[test]
    fn a_bogus_elapsed_does_not_underflow_the_clock() {
        let anchor = anchor_from_elapsed(u32::MAX); // ~49 days; must not panic
        assert!(
            anchor <= Instant::now(),
            "anchor must never be in the future"
        );
        // A sane value still walks the anchor back by that much.
        assert!(anchor_from_elapsed(1000).elapsed() >= Duration::from_millis(1000));
    }

    #[test]
    fn a_status_payload_is_parsed_little_endian() {
        let report = status_report(ST_PASSTHRU, 0x0001_2345, OBB_E_NO_ACK);
        let payload = &report[2..2 + usize::from(report[1])];
        let status = parse_status(payload).expect("parses");
        assert_eq!(status.state, ST_PASSTHRU);
        assert_eq!(status.elapsed_ms, 0x0001_2345);
        assert_eq!(status.last_error, OBB_E_NO_ACK);

        assert!(parse_status(&[OP_STATUS, OB_OK]).is_none(), "short payload");
        assert!(parse_status(&[OP_ENTER; 13]).is_none(), "wrong opcode");
    }
}
