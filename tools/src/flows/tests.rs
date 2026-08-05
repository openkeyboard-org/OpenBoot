use super::*;
use crate::bundle::{Bundle, Source};
use crate::image::Image;
use crate::proto::consts::{
    OB_BOOT_APP, OB_CMD_BOOT, OB_CMD_COMMIT, OB_CMD_CRC, OB_CMD_ERASE, OB_CMD_HELLO, OB_CMD_WRITE,
    OB_DET_NONE, OB_E_NOT_ERASED, OB_FEAT_CRC_LIVE, OB_MAX_WRITE_DATA, OB_OK,
};
use crate::proto::consts::{OB_FAMILY_CH570, OB_FAMILY_CH592};
use crate::proto::{self};
use crate::testutil::*;

#[test]
fn happy_flash_flow() {
    let image = test_image(100);
    let crc = image.crc32();
    let mut mock = MockTransport::new();

    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_ERASE);
        assert_eq!(req.payload, proto::erase_req_payload(0x2000, 4096));
        vec![status_ok(req)]
    });
    for (i, chunk) in image.bytes.chunks(OB_MAX_WRITE_DATA).enumerate() {
        let expected = proto::write_req_payload(0x2000 + (i * OB_MAX_WRITE_DATA) as u32, chunk);
        mock.expect(move |req| {
            assert_eq!(req.cmd, OB_CMD_WRITE);
            assert_eq!(req.payload, expected);
            vec![status_ok(req)]
        });
    }
    mock.expect(move |req| {
        assert_eq!(req.cmd, OB_CMD_COMMIT);
        assert_eq!(req.payload, proto::commit_req_payload(100, crc));
        vec![status_ok(req)]
    });
    mock.expect(move |req| {
        assert_eq!(req.cmd, OB_CMD_CRC);
        assert_eq!(req.payload, proto::crc_req_payload(0x2000, 100));
        let mut p = vec![OB_OK];
        p.extend_from_slice(&crc.to_le_bytes());
        vec![ok_resp(req, p)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_BOOT);
        assert_eq!(req.payload, proto::boot_req_payload(OB_BOOT_APP));
        vec![status_ok(req)]
    });

    flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap();

    let cmds: Vec<u8> = mock.log.iter().map(|f| f.cmd).collect();
    assert_eq!(
        cmds,
        vec![
            OB_CMD_HELLO,
            OB_CMD_ERASE,
            OB_CMD_WRITE,
            OB_CMD_WRITE,
            OB_CMD_WRITE,
            OB_CMD_COMMIT,
            OB_CMD_CRC,
            OB_CMD_BOOT
        ]
    );
    // Sequential per-attempt seq numbers, no reuse.
    let seqs: Vec<u8> = mock.log.iter().map(|f| f.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    assert!(mock.steps.is_empty(), "unconsumed device script steps");
}

#[test]
fn dry_run_sends_only_hello() {
    let image = test_image(100);
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: false,
            verify: true,
            boot: true,
        },
    )
    .unwrap();
    assert_eq!(mock.log.len(), 1);
    assert_eq!(mock.log[0].cmd, OB_CMD_HELLO);
}

#[test]
fn e_not_erased_propagates_without_retry() {
    let image = test_image(16);
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    mock.expect(|req| vec![status_ok(req)]); // ERASE
    mock.expect(|req| vec![status_err(req, OB_E_NOT_ERASED, OB_DET_NONE)]);

    let err = flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("not erased"),
        "expected the E_NOT_ERASED taxonomy text, got: {err:#}"
    );
    assert_eq!(
        mock.sent(OB_CMD_WRITE).len(),
        1,
        "a definitive status error must not be retried"
    );
}

#[test]
fn verify_mismatch_error_downcasts_for_exit_code_2() {
    let image = test_image(32);
    let local = image.crc32();
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    mock.expect(move |req| {
        let mut p = vec![OB_OK];
        p.extend_from_slice(&(local ^ 0xDEAD_BEEF).to_le_bytes());
        vec![ok_resp(req, p)]
    });

    let err = verify(&mut mock, &Source::Single(image.clone())).unwrap_err();
    let mismatch = err
        .downcast_ref::<VerifyMismatch>()
        .expect("verify mismatch must downcast for the exit-code-2 path");
    assert_eq!(mismatch.local_crc, local);
    assert_eq!(mismatch.device_crc, Some(local ^ 0xDEAD_BEEF));
}

#[test]
fn commit_mismatch_maps_to_verify_mismatch() {
    let image = test_image(32);
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    mock.expect(|req| vec![status_err(req, OB_E_VERIFY, OB_DET_VERIFY_MISMATCH)]);

    let err = bless(&mut mock, &Source::Single(image.clone())).unwrap_err();
    let mismatch = err.downcast_ref::<VerifyMismatch>().expect("must downcast");
    assert_eq!(mismatch.device_crc, None);
    assert_eq!(mismatch.local_crc, image.crc32());
}

#[test]
fn boot_is_never_retried() {
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    mock.expect(|_req| Vec::new()); // BOOT: no response

    let err = boot(&mut mock, false).unwrap_err();
    assert_eq!(
        mock.sent(OB_CMD_BOOT).len(),
        1,
        "BOOT must be sent exactly once even on timeout"
    );
    assert!(format!("{err:#}").contains("never retried"), "got: {err:#}");
}

#[test]
fn oversize_image_rejected_before_any_mutation() {
    // Slot A of the [0x2000, 0x70000) region is [0x2000, 0x39000) and its
    // record owns the top block, so 0x36000 bytes are writable; the smallest
    // 4-byte-aligned oversized image is four bytes larger.
    let image = Image {
        base: 0x2000,
        bytes: vec![0xA5; 0x36004],
    };
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    let err = flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("outside the device's writable slot A"),
        "got: {err:#}"
    );
    assert_eq!(mock.log.len(), 1, "nothing after HELLO may be sent");
}

#[test]
fn oversized_device_write_limit_rejected_before_any_mutation() {
    let image = test_image(16);
    let mut mock = MockTransport::new();
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_HELLO);
        let mut payload = info_payload(OB_FEAT_CRC_LIVE, 0x70000);
        payload[23] = 52;
        vec![ok_resp(req, payload)]
    });

    let err = flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("protocol maximum"), "{err:#}");
    assert_eq!(mock.log.len(), 1, "nothing after HELLO may be sent");
}

#[test]
fn offset_base_rejected_for_flash() {
    let image = Image {
        base: 0x3000,
        bytes: vec![0xA5; 64],
    };
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    let err = flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("write base"), "got: {err:#}");
}

#[test]
fn verify_allows_offset_base_within_region() {
    let image = Image {
        base: 0x3000,
        bytes: vec![0xA5; 64],
    };
    let crc = image.crc32();
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    mock.expect(move |req| {
        assert_eq!(req.payload, proto::crc_req_payload(0x3000, 64));
        let mut p = vec![OB_OK];
        p.extend_from_slice(&crc.to_le_bytes());
        vec![ok_resp(req, p)]
    });
    verify(&mut mock, &Source::Single(image.clone())).unwrap();
}

#[test]
fn erase_all_is_chunked_at_32_kib() {
    // --all erases the writable SLOT, not the region: [0x2000, 0x38000) is
    // 54 blocks of 4 KiB, and 8 blocks fit a 32 KiB chunk -> 6 full chunks
    // plus one 6-block tail = 7 ERASE requests.
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    for i in 0u32..7 {
        let addr = 0x2000 + i * ERASE_CHUNK;
        let len = if i == 6 { 6 * 4096 } else { ERASE_CHUNK };
        mock.expect(move |req| {
            assert_eq!(req.cmd, OB_CMD_ERASE);
            assert_eq!(req.payload, proto::erase_req_payload(addr, len));
            vec![status_ok(req)]
        });
    }
    erase(&mut mock, true, None, None, true).unwrap();

    let erases = mock.sent(OB_CMD_ERASE);
    assert_eq!(erases.len(), 7);
}

#[test]
fn erase_requires_force() {
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    erase(&mut mock, true, None, None, false).unwrap();
    assert_eq!(mock.log.len(), 1, "dry run must not send ERASE");
}

#[test]
fn erase_range_validation() {
    // Misaligned start.
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    let err = erase(&mut mock, false, Some(0x1800), Some(0x1000), true).unwrap_err();
    assert!(format!("{err:#}").contains("aligned"), "got: {err:#}");

    // Inside the app region but past the writable slot: the active slot is
    // not addressable, which is the point of bounding erase by the slot.
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, OB_FEAT_CRC_LIVE, 0x70000);
    let err = erase(&mut mock, false, Some(0x39000), Some(0x2000), true).unwrap_err();
    assert!(
        format!("{err:#}").contains("outside the device's writable slot A"),
        "got: {err:#}"
    );
}

#[test]
fn flash_skips_live_crc_when_feature_clear() {
    // CH57x-style device: FEAT_CRC_LIVE clear -> no CRC command issued.
    let image = test_image(16);
    let crc = image.crc32();
    let mut mock = MockTransport::new();
    expect_hello(&mut mock, 0, 0x3B000);
    mock.expect(|req| vec![status_ok(req)]); // ERASE
    mock.expect(|req| vec![status_ok(req)]); // WRITE
    mock.expect(move |req| {
        assert_eq!(req.payload, proto::commit_req_payload(16, crc));
        vec![status_ok(req)]
    });
    mock.expect(|req| vec![status_ok(req)]); // BOOT

    flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap();
    assert!(
        mock.sent(OB_CMD_CRC).is_empty(),
        "no CRC command without FEAT_CRC_LIVE"
    );
}

/* --- A/B slots ---------------------------------------------------------- */

#[test]
fn flash_targets_the_slot_the_device_names() {
    // Slot A is running, so the device offers slot B at 0x39000. An image
    // linked for that base flashes normally — nothing else in the flow
    // changes, because every address it uses comes from the device.
    let image = Image {
        base: 0x39000,
        bytes: vec![0xA5; 48], // one max-sized WRITE
    };
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1);
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_ERASE);
        assert_eq!(req.payload, proto::erase_req_payload(0x39000, 4096));
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_WRITE);
        assert_eq!(
            u32::from_le_bytes(req.payload[..4].try_into().unwrap()),
            0x39000
        );
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_COMMIT);
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_CRC);
        assert_eq!(req.payload, proto::crc_req_payload(0x39000, 48));
        let mut p = vec![OB_OK];
        p.extend_from_slice(&crc32fast::hash(&[0xA5u8; 48]).to_le_bytes());
        vec![ok_resp(req, p)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_BOOT);
        vec![status_ok(req)]
    });
    flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: true,
            boot: true,
        },
    )
    .unwrap();
}

#[test]
fn an_image_for_the_running_slot_is_rejected() {
    // The same device, handed the slot-A artifact. It is a valid image and
    // it is inside the app region, but writing it would overwrite the image
    // the device is currently running, so it must not reach the wire.
    let image = Image {
        base: 0x2000,
        bytes: vec![0xA5; 64],
    };
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1);
    let err = flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: false,
            boot: false,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("0x00039000"), "got: {msg}");
    assert!(msg.contains("slot B"), "got: {msg}");
    assert_eq!(mock.log.len(), 1, "nothing after HELLO may be sent");
}

#[test]
fn a_slot_that_does_not_fit_the_silicon_is_named_as_such() {
    // write_capacity 0: the bootloader was built for a bigger variant. The
    // device would refuse every ERASE with a bare range error, so the flow
    // has to say what is actually wrong before it sends one.
    let image = Image {
        base: 0x2000,
        bytes: vec![0xA5; 64],
    };
    let mut mock = MockTransport::new();
    mock.expect(|req| {
        let mut p = info_payload(OB_FEAT_CRC_LIVE, 0x70000);
        p[44..48].copy_from_slice(&0u32.to_le_bytes());
        vec![ok_resp(req, p)]
    });
    let err = flash(
        &mut mock,
        &Source::Single(image.clone()),
        &FlashOpts {
            force: true,
            verify: false,
            boot: false,
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("does not fit this silicon"),
        "got: {err:#}"
    );
    assert_eq!(mock.log.len(), 1, "nothing after HELLO may be sent");
}

/* --- bundles ------------------------------------------------------------ */

fn bundle_ab(family: u8) -> Source {
    Source::Bundle(
        Bundle::new(
            family,
            vec![
                Image {
                    base: 0x2000,
                    bytes: vec![0xAA; 48],
                },
                Image {
                    base: 0x39000,
                    bytes: vec![0xBB; 48],
                },
            ],
        )
        .unwrap(),
    )
}

#[test]
fn a_bundle_flashes_the_variant_the_device_asks_for() {
    // Slot A is running, so the device offers slot B. The operator names one
    // file and picks nothing; the device's answer selects the build.
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1);
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_ERASE);
        assert_eq!(req.payload, proto::erase_req_payload(0x39000, 4096));
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_WRITE);
        assert_eq!(
            u32::from_le_bytes(req.payload[..4].try_into().unwrap()),
            0x39000
        );
        assert_eq!(
            &req.payload[4..],
            &[0xBB; 48],
            "slot B's build, not slot A's"
        );
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_COMMIT);
        assert_eq!(
            u32::from_le_bytes(req.payload[4..8].try_into().unwrap()),
            crc32fast::hash(&[0xBBu8; 48])
        );
        vec![status_ok(req)]
    });
    flash(
        &mut mock,
        &bundle_ab(OB_FAMILY_CH592),
        &FlashOpts {
            force: true,
            verify: false,
            boot: false,
        },
    )
    .unwrap();
}

#[test]
fn the_same_bundle_flashes_slot_a_when_that_is_the_target() {
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 1, 0);
    mock.expect(|req| {
        assert_eq!(req.payload, proto::erase_req_payload(0x2000, 4096));
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(&req.payload[4..], &[0xAA; 48], "slot A's build");
        vec![status_ok(req)]
    });
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_COMMIT);
        vec![status_ok(req)]
    });
    flash(
        &mut mock,
        &bundle_ab(OB_FAMILY_CH592),
        &FlashOpts {
            force: true,
            verify: false,
            boot: false,
        },
    )
    .unwrap();
}

#[test]
fn a_bundle_for_another_part_never_reaches_the_wire() {
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1);
    let err = flash(
        &mut mock,
        &bundle_ab(OB_FAMILY_CH570),
        &FlashOpts {
            force: true,
            verify: false,
            boot: false,
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("built for CH570"),
        "got: {err:#}"
    );
    assert_eq!(mock.log.len(), 1, "nothing after HELLO may be sent");
}

#[test]
fn a_bundle_missing_the_target_slot_says_what_it_has() {
    // A one-variant bundle handed to a device that wants the other slot: the
    // build simply is not there, and no amount of relocation would help.
    let only_a = Source::Bundle(
        Bundle::new(
            0,
            vec![Image {
                base: 0x2000,
                bytes: vec![0xAA; 48],
            }],
        )
        .unwrap(),
    );
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1);
    let err = flash(
        &mut mock,
        &only_a,
        &FlashOpts {
            force: true,
            verify: false,
            boot: false,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("0x00039000"), "got: {msg}");
    assert!(msg.contains("0x00002000"), "got: {msg}");
    assert_eq!(mock.log.len(), 1, "nothing after HELLO may be sent");
}

#[test]
fn verify_checks_the_running_slot_not_the_write_target() {
    // Device is writing B, so A is running: verify must CRC slot A.
    let mut mock = MockTransport::new();
    expect_hello_slots(&mut mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1);
    mock.expect(|req| {
        assert_eq!(req.cmd, OB_CMD_CRC);
        assert_eq!(req.payload, proto::crc_req_payload(0x2000, 48));
        let mut p = vec![OB_OK];
        p.extend_from_slice(&crc32fast::hash(&[0xAAu8; 48]).to_le_bytes());
        vec![ok_resp(req, p)]
    });
    verify(&mut mock, &bundle_ab(OB_FAMILY_CH592)).unwrap();
}
