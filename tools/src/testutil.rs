//! Shared test scaffolding: a scripted in-memory device plus canned
//! responses, used by both the client and flow test modules. The mock
//! implements `Transport` through the real `xfer_link` matcher, so
//! stale-seq discarding and frame-error acceptance are exercised for
//! real rather than simulated.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::image::Image;
use crate::proto::consts::{
    OB_CMD_HELLO, OB_CMD_RESP_BIT, OB_FAMILY_CH592, OB_OK, OB_TRANSPORT_ID_USB,
};
use crate::proto::{self, Frame};
use crate::transport::{xfer_link, FrameLink, Transport, TransportError};

/// One scripted device reaction: raw frames fed back for a request
/// (empty list = timeout).
pub(crate) type Step = Box<dyn FnMut(&Frame) -> Vec<Vec<u8>>>;

/// Scripted in-memory device. Implements `Transport` through the real
/// `xfer_link` matcher, so stale-seq discarding and frame-error
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

impl FrameLink for MockTransport {
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

impl Transport for MockTransport {
    fn xfer(&mut self, req: &Frame, timeout: Duration) -> Result<Frame, TransportError> {
        xfer_link(self, req, timeout)
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
    let mut p = vec![OB_OK, 0, 1, 9];
    p.extend_from_slice(&0x000Au16.to_le_bytes());
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
    p
}

pub(crate) fn expect_hello(mock: &mut MockTransport, features: u32, app_end: u32) {
    mock.expect(move |req| {
        assert_eq!(req.cmd, OB_CMD_HELLO);
        assert_eq!(req.payload, proto::hello_req_payload(0, 1));
        vec![ok_resp(req, info_payload(features, app_end))]
    });
}

pub(crate) fn test_image(len: usize) -> Image {
    Image {
        base: 0x2000,
        bytes: (0..len).map(|i| i as u8).collect(),
    }
}

/* --- tests ---------------------------------------------------------- */
