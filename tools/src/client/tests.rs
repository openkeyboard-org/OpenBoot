use super::*;
use crate::proto::consts::{
    OB_CMD_FRAME_ERR, OB_CMD_RESP_BIT, OB_DET_NONE, OB_E_CRC, OB_FAMILY_CH592, OB_FEAT_CRC_LIVE,
    OB_OK,
};
use crate::testutil::*;

#[test]
fn hello_rejects_unusable_geometry() {
    // erase_block == 0 from a hostile device must error cleanly at
    // DeviceInfo::parse (never reach the flows' div_ceil arithmetic).
    // The field sits at payload offset 16..20.
    let mut mock = MockTransport::new();
    mock.expect(|req| {
        let mut p = info_payload(OB_FEAT_CRC_LIVE, 0x70000);
        p[16..20].copy_from_slice(&0u32.to_le_bytes());
        vec![ok_resp(req, p)]
    });
    let mut client = BootClient::new(&mut mock);
    // Wrap in anyhow the way the CLI boundary does, so the assertion walks
    // the #[source] chain exactly as the rendered error message will.
    let err = anyhow::Error::from(client.hello().unwrap_err());
    assert!(
        format!("{err:#}").contains("zero erase block"),
        "got: {err:#}"
    );
}

#[test]
fn retry_on_timeout_bumps_seq() {
    let mut mock = MockTransport::new();
    mock.expect(|_req| Vec::new()); // first HELLO: no response
    mock.expect(|req| vec![ok_resp(req, info_payload(OB_FEAT_CRC_LIVE, 0x70000))]);

    let mut client = BootClient::new(&mut mock);
    let info = client.hello().unwrap();
    assert_eq!(info.chip_family, OB_FAMILY_CH592);

    assert_eq!(mock.log.len(), 2);
    assert_ne!(
        mock.log[0].seq, mock.log[1].seq,
        "retry must use a fresh seq"
    );
    assert_eq!(mock.log[0].payload, mock.log[1].payload);
}

#[test]
fn frame_error_report_triggers_fresh_seq_resend() {
    let mut mock = MockTransport::new();
    mock.expect(|req| {
        // Device says it saw a corrupt frame (E_CRC) — retryable.
        vec![Frame::new(OB_CMD_FRAME_ERR, req.seq, vec![OB_E_CRC, OB_DET_NONE]).encode()]
    });
    mock.expect(|req| vec![ok_resp(req, info_payload(0, 0x70000))]);

    let mut client = BootClient::new(&mut mock);
    client.hello().unwrap();
    assert_eq!(mock.log.len(), 2);
    assert_ne!(mock.log[0].seq, mock.log[1].seq);
}

#[test]
fn stale_seq_responses_are_discarded() {
    let mut mock = MockTransport::new();
    mock.expect(|req| {
        // A late answer to a previous attempt arrives first, then the
        // real response; the matcher must skip the stale one.
        let stale = Frame::new(
            req.cmd | OB_CMD_RESP_BIT,
            req.seq.wrapping_sub(1),
            vec![OB_OK],
        )
        .encode();
        vec![stale, ok_resp(req, info_payload(OB_FEAT_CRC_LIVE, 0x70000))]
    });

    let mut client = BootClient::new(&mut mock);
    client.hello().unwrap();
    assert_eq!(
        mock.log.len(),
        1,
        "one request was enough — no retry needed"
    );
}

#[test]
fn corrupt_response_frames_are_discarded() {
    let mut mock = MockTransport::new();
    mock.expect(|req| {
        let mut corrupt = ok_resp(req, info_payload(0, 0x70000));
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xFF; // break the CRC
        vec![corrupt, ok_resp(req, info_payload(0, 0x70000))]
    });
    let mut client = BootClient::new(&mut mock);
    client.hello().unwrap();
    assert_eq!(mock.log.len(), 1);
}

#[test]
fn three_timeouts_exhaust_the_retry_budget() {
    let mut mock = MockTransport::new();
    for _ in 0..MAX_ATTEMPTS {
        mock.expect(|_req| Vec::new());
    }
    let mut client = BootClient::new(&mut mock);
    let err = client.hello().unwrap_err();
    assert!(
        format!("{err:#}").contains("3 attempts"),
        "expected an attempts-exhausted error, got: {err:#}"
    );
    assert_eq!(mock.log.len(), MAX_ATTEMPTS);
}

#[test]
fn erase_timeout_scales_with_blocks() {
    assert_eq!(erase_timeout(1), Duration::from_millis(230));
    assert_eq!(erase_timeout(8), Duration::from_millis(440));
}
