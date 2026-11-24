//! The CLI's text, pinned. Each case drives one flow against the scripted
//! device and compares everything it prints — stdout, stderr and the error
//! line `main` would add — with a file under `tests/golden/`.
//!
//! These exist because the flow tests assert what goes on the WIRE, and say
//! nothing about what the user reads. A change to the text is sometimes
//! intended; it should never be an accident. To accept a deliberate change:
//!
//!     UPDATE_GOLDEN=1 cargo test golden
//!
//! and review the diff of `tests/golden/` like any other.

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Result;

use crate::bundle::{Bundle, Source};
use crate::console::{self, Console};
use crate::flows::FlashOpts;
use crate::image::Image;
use crate::proto::consts::{
    OB_DET_NONE, OB_DET_VERIFY_MISMATCH, OB_E_NOT_ERASED, OB_E_VERIFY, OB_FAMILY_CH570,
    OB_FAMILY_CH592, OB_FEAT_CRC_LIVE, OB_OK, OB_SLOT_ID_NONE,
};
use crate::testutil::*;

/* --- capture ------------------------------------------------------------ */

#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).expect("console output is UTF-8")
    }
}

/// The one figure in the output that depends on the clock.
fn mask_elapsed(text: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        match (line.find(" bytes in "), line.rfind(" s")) {
            (Some(at), Some(end)) if line.starts_with("wrote ") && end > at => {
                out.push_str(&line[..at]);
                out.push_str(" bytes in <elapsed>");
                out.push_str(&line[end..]);
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// Run `flow` and render everything a user would see as one transcript.
fn transcript(flow: impl FnOnce(&mut Console) -> Result<()>) -> String {
    let (out, err) = (Shared::default(), Shared::default());
    let mut con = Console {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };
    let result = flow(&mut con);
    drop(con);

    // Progress redraws with a bare carriage return; spell it out so the files
    // hold no control characters and survive any line-ending conversion.
    let stderr = err.text().replace('\r', "<CR>");
    let outcome = match &result {
        Ok(()) => "exit 0\n".to_string(),
        Err(e) => {
            let code = console::exit_code(e);
            format!("ERROR: {e:#}\nexit {code}\n")
        }
    };
    format!(
        "--- stdout ---\n{}--- stderr ---\n{}--- outcome ---\n{}",
        mask_elapsed(&out.text()),
        stderr,
        outcome
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(format!("{name}.txt"))
}

fn check(name: &str, actual: String) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e}\nrun `UPDATE_GOLDEN=1 cargo test golden` to create it",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "{name}: output differs from {}\n--- expected ---\n{expected}\n--- actual ---\n{actual}",
        path.display()
    );
}

/* --- scripted devices --------------------------------------------------- */

/// A HELLO whose payload is the usual fixture, then patched.
fn hello(
    mock: &mut MockTransport,
    features: u32,
    app_end: u32,
    active: u8,
    write: u8,
    patch: impl Fn(&mut Vec<u8>) + 'static,
) {
    mock.expect(move |req| {
        let mut p = info_payload_slots(features, app_end, active, write);
        patch(&mut p);
        vec![ok_resp(req, p)]
    });
}

/// A CH592 running slot A and offering slot B, whose silicon matches its
/// build — the ordinary healthy device.
fn hello_ch592(mock: &mut MockTransport) {
    hello(mock, OB_FEAT_CRC_LIVE, 0x70000, 0, 1, |p| p[3] = 0x92);
}

/// A CH570: no live CRC, smaller region.
fn hello_ch570(mock: &mut MockTransport) {
    hello(mock, 0, 0x3B000, 0, 1, |p| {
        p[3] = 0x70;
        p[6] = OB_FAMILY_CH570;
    });
}

fn ok(mock: &mut MockTransport, n: usize) {
    for _ in 0..n {
        mock.expect(|req| vec![status_ok(req)]);
    }
}

fn crc_reply(mock: &mut MockTransport, value: u32) {
    mock.expect(move |req| {
        let mut p = vec![OB_OK];
        p.extend_from_slice(&value.to_le_bytes());
        vec![ok_resp(req, p)]
    });
}

/// 100 bytes linked for slot B of the CH592 fixture: one erase block, three
/// WRITE frames.
fn image_b() -> Image {
    Image {
        base: 0x39000,
        bytes: (0..100).map(|i| i as u8).collect(),
    }
}

/// The same for the CH570 fixture, whose slot B starts lower.
fn image_b_ch570() -> Image {
    Image {
        base: 0x1E000,
        bytes: (0..100).map(|i| i as u8).collect(),
    }
}

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

const FORCED: FlashOpts = FlashOpts {
    force: true,
    verify: true,
    boot: true,
};

/* --- probe -------------------------------------------------------------- */

#[test]
fn golden_probe() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    check("probe", transcript(|con| console::probe(&mut mock, con)));
}

#[test]
fn golden_probe_wrong_variant() {
    // Silicon id disagrees with the build, nothing bootable, and the write
    // slot does not fit: every warning probe can print, at once.
    let mut mock = MockTransport::new();
    hello(
        &mut mock,
        OB_FEAT_CRC_LIVE,
        0x70000,
        OB_SLOT_ID_NONE,
        0,
        |p| {
            p[44..48].copy_from_slice(&0u32.to_le_bytes());
        },
    );
    check(
        "probe_wrong_variant",
        transcript(|con| console::probe(&mut mock, con)),
    );
}

/* --- flash -------------------------------------------------------------- */

#[test]
fn golden_flash_dry_run() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    let src = Source::Single(image_b());
    let opts = FlashOpts {
        force: false,
        ..FORCED
    };
    check(
        "flash_dry_run",
        transcript(|con| console::flash(&mut mock, &src, &opts, con)),
    );
}

#[test]
fn golden_flash() {
    let image = image_b();
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1 + 3 + 1); // ERASE, 3 x WRITE, COMMIT
    crc_reply(&mut mock, image.crc32());
    ok(&mut mock, 1); // BOOT
    let src = Source::Single(image);
    check(
        "flash",
        transcript(|con| console::flash(&mut mock, &src, &FORCED, con)),
    );
}

#[test]
fn golden_flash_bundle_no_verify() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1 + 1 + 1 + 1); // ERASE, WRITE, COMMIT, BOOT
    let opts = FlashOpts {
        verify: false,
        ..FORCED
    };
    check(
        "flash_bundle_no_verify",
        transcript(|con| console::flash(&mut mock, &bundle_ab(OB_FAMILY_CH592), &opts, con)),
    );
}

#[test]
fn golden_flash_ch570_no_boot() {
    // No FEAT_CRC_LIVE: COMMIT is the only attestation, and says so.
    let mut mock = MockTransport::new();
    hello_ch570(&mut mock);
    ok(&mut mock, 1 + 3 + 1); // ERASE, 3 x WRITE, COMMIT
    let src = Source::Single(image_b_ch570());
    let opts = FlashOpts {
        boot: false,
        ..FORCED
    };
    check(
        "flash_ch570_no_boot",
        transcript(|con| console::flash(&mut mock, &src, &opts, con)),
    );
}

#[test]
fn golden_flash_no_boot_over_a_tunnel() {
    // A transport that cannot hold the device in the bootloader parks it with
    // BOOT --stay instead of leaving it un-booted.
    let mut mock = MockTransport::new();
    mock.holds = false;
    hello_ch592(&mut mock);
    ok(&mut mock, 1 + 3 + 1); // ERASE, 3 x WRITE, COMMIT
    crc_reply(&mut mock, image_b().crc32());
    ok(&mut mock, 1); // BOOT (stay)
    let src = Source::Single(image_b());
    let opts = FlashOpts {
        boot: false,
        ..FORCED
    };
    check(
        "flash_no_boot_over_a_tunnel",
        transcript(|con| console::flash(&mut mock, &src, &opts, con)),
    );
}

#[test]
fn golden_flash_write_refused() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1 + 1); // ERASE, first WRITE
    mock.expect(|req| vec![status_err(req, OB_E_NOT_ERASED, OB_DET_NONE)]);
    let src = Source::Single(image_b());
    check(
        "flash_write_refused",
        transcript(|con| console::flash(&mut mock, &src, &FORCED, con)),
    );
}

#[test]
fn golden_flash_commit_mismatch() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1 + 3); // ERASE, 3 x WRITE
    mock.expect(|req| vec![status_err(req, OB_E_VERIFY, OB_DET_VERIFY_MISMATCH)]);
    let src = Source::Single(image_b());
    check(
        "flash_commit_mismatch",
        transcript(|con| console::flash(&mut mock, &src, &FORCED, con)),
    );
}

#[test]
fn golden_flash_live_crc_mismatch() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1 + 3 + 1); // ERASE, 3 x WRITE, COMMIT
    crc_reply(&mut mock, 0xDEAD_BEEF);
    let src = Source::Single(image_b());
    check(
        "flash_live_crc_mismatch",
        transcript(|con| console::flash(&mut mock, &src, &FORCED, con)),
    );
}

#[test]
fn golden_flash_wrong_family_bundle() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    check(
        "flash_wrong_family_bundle",
        transcript(|con| console::flash(&mut mock, &bundle_ab(OB_FAMILY_CH570), &FORCED, con)),
    );
}

#[test]
fn golden_flash_image_for_the_running_slot() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    let src = Source::Single(test_image(100)); // linked for slot A
    check(
        "flash_image_for_the_running_slot",
        transcript(|con| console::flash(&mut mock, &src, &FORCED, con)),
    );
}

/* --- verify ------------------------------------------------------------- */

#[test]
fn golden_verify() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    crc_reply(&mut mock, crc32fast::hash(&[0xAA; 48]));
    check(
        "verify",
        transcript(|con| console::verify(&mut mock, &bundle_ab(OB_FAMILY_CH592), con)),
    );
}

#[test]
fn golden_verify_mismatch() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    crc_reply(&mut mock, 0x0BAD_F00D);
    check(
        "verify_mismatch",
        transcript(|con| console::verify(&mut mock, &bundle_ab(OB_FAMILY_CH592), con)),
    );
}

#[test]
fn golden_verify_without_live_crc() {
    let image = test_image(100);
    let mut mock = MockTransport::new();
    hello_ch570(&mut mock);
    crc_reply(&mut mock, image.crc32());
    let src = Source::Single(image);
    check(
        "verify_without_live_crc",
        transcript(|con| console::verify(&mut mock, &src, con)),
    );
}

/* --- erase -------------------------------------------------------------- */

#[test]
fn golden_erase_dry_run() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    check(
        "erase_dry_run",
        transcript(|con| console::erase(&mut mock, true, None, None, false, con)),
    );
}

#[test]
fn golden_erase_all() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 7); // 54 blocks in 32 KiB chunks
    check(
        "erase_all",
        transcript(|con| console::erase(&mut mock, true, None, None, true, con)),
    );
}

#[test]
fn golden_erase_one_block() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1);
    check(
        "erase_one_block",
        transcript(|con| console::erase(&mut mock, false, Some(0x39000), Some(0x1000), true, con)),
    );
}

#[test]
fn golden_erase_misaligned() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    check(
        "erase_misaligned",
        transcript(|con| console::erase(&mut mock, false, Some(0x39800), Some(0x1000), true, con)),
    );
}

/* --- boot --------------------------------------------------------------- */

#[test]
fn golden_boot() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1);
    check(
        "boot",
        transcript(|con| console::boot(&mut mock, false, con)),
    );
}

#[test]
fn golden_boot_stay() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1);
    check(
        "boot_stay",
        transcript(|con| console::boot(&mut mock, true, con)),
    );
}

#[test]
fn golden_boot_no_response() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    mock.expect(|_req| Vec::new());
    check(
        "boot_no_response",
        transcript(|con| console::boot(&mut mock, false, con)),
    );
}

/* --- bless -------------------------------------------------------------- */

#[test]
fn golden_bless() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    ok(&mut mock, 1); // COMMIT
    let src = Source::Single(image_b());
    check(
        "bless",
        transcript(|con| console::bless(&mut mock, &src, con)),
    );
}

#[test]
fn golden_bless_mismatch() {
    let mut mock = MockTransport::new();
    hello_ch592(&mut mock);
    mock.expect(|req| vec![status_err(req, OB_E_VERIFY, OB_DET_VERIFY_MISMATCH)]);
    let src = Source::Single(image_b());
    check(
        "bless_mismatch",
        transcript(|con| console::bless(&mut mock, &src, con)),
    );
}
