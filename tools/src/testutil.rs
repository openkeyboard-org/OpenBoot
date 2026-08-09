//! Shared test scaffolding: a scripted in-memory device plus canned
//! responses, used by both the client and flow test modules. The mock
//! uses the real `Transport::xfer` matcher, so
//! stale-seq discarding and frame-error acceptance are exercised for
//! real rather than simulated.

use std::collections::VecDeque;
use std::time::Instant;

use anyhow::Result;

use crate::image::Image;
use crate::proto::consts::{
    OB_CMD_HELLO, OB_CMD_RESP_BIT, OB_FAMILY_CH592, OB_OK, OB_PROTO_MAJOR, OB_PROTO_MINOR,
    OB_SLOT_ID_NONE, OB_TRANSPORT_ID_USB,
};
use crate::proto::{self, Frame};
use crate::transport::Transport;

/// bl_version carried by canned HELLO payloads. A fixture value, not the
/// live firmware version: the host must parse whatever a device reports,
/// so mock payloads do not track OB_BL_VERSION — only the golden-vector
/// tests assert the real one.
pub(crate) const FIXTURE_BL_VERSION: u16 = 0x000B;

/// One scripted device reaction: raw frames fed back for a request
/// (empty list = timeout).
pub(crate) type Step = Box<dyn FnMut(&Frame) -> Vec<Vec<u8>>>;

/// Scripted in-memory device. Implements `Transport` through the real
/// default matcher, so stale-seq discarding and frame-error
/// acceptance are exercised for real.
pub(crate) struct MockTransport {
    pub(crate) steps: VecDeque<Step>,
    pub(crate) rx: VecDeque<Vec<u8>>,
    pub(crate) log: Vec<Frame>,
}

impl MockTransport {
    pub(crate) fn new() -> MockTransport {
        MockTransport {
            steps: VecDeque::new(),
            rx: VecDeque::new(),
            log: Vec::new(),
        }
    }

    pub(crate) fn expect(&mut self, step: impl FnMut(&Frame) -> Vec<Vec<u8>> + 'static) {
        self.steps.push_back(Box::new(step));
    }

    pub(crate) fn sent(&self, cmd: u8) -> Vec<&Frame> {
        self.log.iter().filter(|f| f.cmd == cmd).collect()
    }
}

impl Transport for MockTransport {
    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        let f = Frame::decode(frame).expect("host sent an undecodable frame");
        self.log.push(f.clone());
        let mut step = self
            .steps
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected request: cmd 0x{:02X}", f.cmd));
        self.rx.extend(step(&f));
        Ok(())
    }

    fn recv_frame(&mut self, _deadline: Instant) -> Result<Option<Vec<u8>>> {
        // Empty queue = the deadline passed with nothing matching.
        Ok(self.rx.pop_front())
    }
}

/* --- canned device responses --------------------------------------- */

pub(crate) fn ok_resp(req: &Frame, payload: Vec<u8>) -> Vec<u8> {
    Frame::new(req.cmd | OB_CMD_RESP_BIT, req.seq, payload).encode()
}

pub(crate) fn status_ok(req: &Frame) -> Vec<u8> {
    ok_resp(req, vec![OB_OK])
}

pub(crate) fn status_err(req: &Frame, status: u8, detail: u8) -> Vec<u8> {
    ok_resp(req, vec![status, detail])
}

pub(crate) fn info_payload(features: u32, app_end: u32) -> Vec<u8> {
    info_payload_slots(features, app_end, OB_SLOT_ID_NONE, 0)
}

/// A HELLO payload with an explicit A/B view, for the cases where which
/// slot the device is willing to write is the thing under test.
pub(crate) fn info_payload_slots(features: u32, app_end: u32, active: u8, write: u8) -> Vec<u8> {
    let mut p = vec![OB_OK, OB_PROTO_MAJOR, OB_PROTO_MINOR, 9];
    p.extend_from_slice(&FIXTURE_BL_VERSION.to_le_bytes());
    p.push(OB_FAMILY_CH592);
    p.push(OB_TRANSPORT_ID_USB);
    p.extend_from_slice(&0x2000u32.to_le_bytes());
    p.extend_from_slice(&app_end.to_le_bytes());
    p.extend_from_slice(&4096u32.to_le_bytes());
    p.extend_from_slice(&256u16.to_le_bytes());
    p.push(4);
    p.push(48);
    p.extend_from_slice(&features.to_le_bytes());
    p.extend_from_slice(&0x0123_4567_89AB_CDEFu64.to_le_bytes());
    // The geometry is the real one a device derives — half the region rounded
    // down to a whole erase block, less the block the record owns — so the
    // flows are exercised against a window a device actually reports.
    let slot_size = ((app_end - 0x2000) / 2 / 4096) * 4096;
    p.push(2);
    p.push(active);
    p.push(write);
    p.push(0);
    p.extend_from_slice(&(0x2000 + u32::from(write) * slot_size).to_le_bytes());
    p.extend_from_slice(&(slot_size - 4096).to_le_bytes());
    p
}

pub(crate) fn expect_hello(mock: &mut MockTransport, features: u32, app_end: u32) {
    expect_hello_slots(mock, features, app_end, OB_SLOT_ID_NONE, 0)
}

pub(crate) fn expect_hello_slots(
    mock: &mut MockTransport,
    features: u32,
    app_end: u32,
    active: u8,
    write: u8,
) {
    mock.expect(move |req| {
        assert_eq!(req.cmd, OB_CMD_HELLO);
        assert_eq!(
            req.payload,
            proto::hello_req_payload(OB_PROTO_MAJOR, OB_PROTO_MINOR)
        );
        vec![ok_resp(
            req,
            info_payload_slots(features, app_end, active, write),
        )]
    });
}

pub(crate) fn test_image(len: usize) -> Image {
    Image {
        base: 0x2000,
        bytes: (0..len).map(|i| i as u8).collect(),
    }
}

/* --- tests ---------------------------------------------------------- */
