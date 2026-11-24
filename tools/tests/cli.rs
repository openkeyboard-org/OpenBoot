//! The built `openboot` binary, run as a user would run it.
//!
//! Only what needs no device: bundle handling, and the refusals that happen
//! before any transport is opened. Everything that talks to a device is
//! pinned in `src/golden_tests.rs` against the scripted one. As there, a
//! deliberate change to the text is accepted with
//! `UPDATE_GOLDEN=1 cargo test` and reviewed as a diff of `tests/golden/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn openboot(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_openboot"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run openboot")
}

/// stdout, stderr and the exit status as one transcript, with line endings
/// normalised so the same file serves every platform.
fn transcript(out: &Output) -> String {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    format!(
        "--- stdout ---\n{}--- stderr ---\n{}--- outcome ---\nexit {}\n",
        text(&out.stdout),
        text(&out.stderr),
        out.status
            .code()
            .expect("openboot was not killed by a signal")
    )
}

fn check(name: &str, actual: String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e}\nrun `UPDATE_GOLDEN=1 cargo test` to create it",
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

/// A scratch directory holding two small slot images. Commands run inside it
/// with relative names, so no machine-specific path reaches the output.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("openboot-cli-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), [0xAA; 48]).unwrap();
        std::fs::write(dir.join("b.bin"), [0xBB; 48]).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn bundle_create_then_info() {
    let scratch = Scratch::new("bundle");
    let created = openboot(
        &scratch.0,
        &[
            "bundle",
            "create",
            "-o",
            "app.obb",
            "--chip",
            "ch592",
            "a.bin@0x2000",
            "b.bin@0x39000",
        ],
    );
    check("cli_bundle_create", transcript(&created));
    check(
        "cli_bundle_info",
        transcript(&openboot(&scratch.0, &["bundle", "info", "app.obb"])),
    );
}

#[test]
fn bundle_create_refuses_a_bin_without_its_link_base() {
    let scratch = Scratch::new("nobase");
    check(
        "cli_bundle_create_no_base",
        transcript(&openboot(
            &scratch.0,
            &["bundle", "create", "-o", "app.obb", "a.bin"],
        )),
    );
    assert!(
        !scratch.0.join("app.obb").exists(),
        "nothing may be written"
    );
}

#[test]
fn bundle_info_rejects_a_file_that_is_not_a_bundle() {
    let scratch = Scratch::new("notbundle");
    check(
        "cli_bundle_info_not_a_bundle",
        transcript(&openboot(&scratch.0, &["bundle", "info", "a.bin"])),
    );
}

/* --- refusals that never open a device ---------------------------------- */

#[test]
fn a_port_with_the_usb_transport_is_a_conflict() {
    let scratch = Scratch::new("conflict");
    check(
        "cli_port_conflicts_with_usb",
        transcript(&openboot(
            &scratch.0,
            &["--port", "nowhere", "--transport", "usb", "probe"],
        )),
    );
}

#[test]
fn the_qmk_tunnel_has_no_default_identity() {
    let scratch = Scratch::new("qmk");
    check(
        "cli_qmk_needs_vid_pid",
        transcript(&openboot(&scratch.0, &["--transport", "qmk", "probe"])),
    );
}

#[test]
fn the_bootloader_usage_cannot_be_overridden() {
    let scratch = Scratch::new("usage");
    check(
        "cli_usage_page_only_for_qmk",
        transcript(&openboot(&scratch.0, &["--usage-page", "0xFF61", "probe"])),
    );
}

#[test]
fn erase_without_a_range_is_refused() {
    let scratch = Scratch::new("erase");
    check(
        "cli_erase_needs_a_range",
        transcript(&openboot(&scratch.0, &["erase"])),
    );
}

/// clap's own usage errors exit 2 by default, which this tool reserves for a
/// verify mismatch; `main` remaps them. The text is clap's and names the
/// binary as it was invoked, so only the status is pinned.
#[test]
fn a_usage_error_exits_1_not_2() {
    let scratch = Scratch::new("usageerr");
    let out = openboot(&scratch.0, &["flash"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Usage"));
}

#[test]
fn help_exits_0() {
    let scratch = Scratch::new("help");
    let out = openboot(&scratch.0, &["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Exit codes"));
}
