//! The CLI's presentation of the flows: the text it prints and the exit code
//! it returns.
//!
//! [`crate::flows`] reports events and returns data; this module is the one
//! place that turns those into words. It lives in the library, not in the
//! binary, so the text can be tested against the scripted device (see
//! `golden_tests`) and reused by anything that wants the same transcript — a
//! log pane, say. It depends on nothing the flows do not.

use std::io::Write;

use anyhow::Result;

use crate::bundle::Source;
use crate::flows::{
    self, Event, FlashBoot, FlashOpts, FlashPlan, Mutation, Observer, Phase, Verification,
    VerifyMismatch, VerifyPlan,
};
use crate::proto::device_info::DeviceInfo;
use crate::transport::Transport;

/// Where the text goes: the two streams the CLI prints to, as writers so the
/// exact output can be captured and compared in tests.
pub struct Console {
    pub out: Box<dyn Write>,
    pub err: Box<dyn Write>,
}

impl Console {
    /// The process's stdout and stderr.
    pub fn stdio() -> Console {
        Console {
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
        }
    }

    /// Discards everything.
    pub fn null() -> Console {
        Console {
            out: Box::new(std::io::sink()),
            err: Box::new(std::io::sink()),
        }
    }
}

// A failed write to the console is ignored rather than propagated: losing a
// line of text must never abort a flash part-way through.
macro_rules! outln {
    ($con:expr, $($arg:tt)*) => {{ let _ = writeln!($con.out, $($arg)*); }};
}
macro_rules! errln {
    ($con:expr) => {{ let _ = writeln!($con.err); }};
    ($con:expr, $($arg:tt)*) => {{ let _ = writeln!($con.err, $($arg)*); }};
}
macro_rules! err {
    ($con:expr, $($arg:tt)*) => {{ let _ = write!($con.err, $($arg)*); }};
}

/// Exit codes: 0 ok, 1 device/IO/usage, 2 verify mismatch.
pub fn exit_code(e: &anyhow::Error) -> u8 {
    if e.downcast_ref::<VerifyMismatch>().is_some() {
        2
    } else {
        1
    }
}

/* --- rendering ---------------------------------------------------------- */

/// Everything HELLO reported, one field per line: `probe`'s output.
pub fn print_device_info(con: &mut Console, info: &DeviceInfo) {
    let region_kib = (u64::from(info.app_end) - u64::from(info.app_start)) / 1024;
    outln!(con, "device:");
    outln!(
        con,
        "  protocol        OBP {}.{}",
        info.proto_major,
        info.proto_minor
    );
    outln!(
        con,
        "  bootloader      v{}.{} (0x{:04X})",
        info.bl_version >> 8,
        info.bl_version & 0xFF,
        info.bl_version
    );
    outln!(
        con,
        "  chip            {} (rev 0x{:02X})",
        info.family_name(),
        info.chip_rev
    );
    outln!(con, "  transport       {}", info.transport_name());
    outln!(
        con,
        "  app region      0x{:08X}..0x{:08X} ({region_kib} KiB)",
        info.app_start,
        info.app_end
    );
    outln!(con, "  erase block     {} B", info.erase_block);
    outln!(
        con,
        "  write           page {} B, align {}, max {} B/frame",
        info.write_page,
        info.write_align,
        info.max_write_data
    );
    outln!(con, "  features        {}", info.feature_names());
    outln!(con, "  uid             {}", info.uid_hex());
    outln!(
        con,
        "  slots           {} (active {}, writing {})",
        info.slot_count,
        DeviceInfo::slot_name(info.active_slot),
        DeviceInfo::slot_name(info.write_slot)
    );
    if info.write_capacity == 0 {
        outln!(
            con,
            "  write window    none — this slot does not fit the silicon"
        );
    } else {
        outln!(
            con,
            "  write window    0x{:08X}..0x{:08X} ({} KiB)",
            info.write_base,
            u64::from(info.write_base) + u64::from(info.write_capacity),
            info.write_capacity / 1024
        );
    }
    if let Some(w) = info.variant_mismatch() {
        errln!(con, "WARNING: {w}");
    }
}

fn print_device_brief(con: &mut Console, info: &DeviceInfo) {
    outln!(
        con,
        "device: {} rev 0x{:02X}, bootloader v{}.{}, uid {}",
        info.family_name(),
        info.chip_rev,
        info.bl_version >> 8,
        info.bl_version & 0xFF,
        info.uid_hex()
    );
    outln!(
        con,
        "        app 0x{:08X}..0x{:08X}, erase block {} B, features {}",
        info.app_start,
        info.app_end,
        info.erase_block,
        info.feature_names()
    );
    outln!(
        con,
        "        active slot {}, writing slot {} at 0x{:08X}",
        DeviceInfo::slot_name(info.active_slot),
        DeviceInfo::slot_name(info.write_slot),
        info.write_base
    );
    if let Some(w) = info.variant_mismatch() {
        errln!(con, "WARNING: {w}");
    }
}

fn print_flash_plan(con: &mut Console, p: &FlashPlan) {
    let (len, block, blocks, chunk, writes, crc) =
        (p.len, p.block, p.blocks, p.chunk, p.writes, p.crc);
    outln!(con, "plan:");
    outln!(con, "  base address    0x{:08X}", p.base);
    outln!(
        con,
        "  image length    {len} bytes (0x{len:X}), padded to 4 B"
    );
    outln!(
        con,
        "  erase           {blocks} block{} x {block} B = {} bytes",
        if blocks == 1 { "" } else { "s" },
        p.erase_len
    );
    outln!(
        con,
        "  write           {writes} frame{} x <={chunk} B",
        if writes == 1 { "" } else { "s" }
    );
    outln!(con, "  image crc32     0x{crc:08X}");
    outln!(
        con,
        "  verify          {}",
        match p.verify {
            VerifyPlan::Skipped => "NO (--no-verify)",
            VerifyPlan::CommitPlusLiveCrc => "COMMIT attestation + live CRC cross-check",
            VerifyPlan::CommitOnly => "COMMIT attestation only (no FEAT_CRC_LIVE on this chip)",
        }
    );
    outln!(
        con,
        "  boot after      {}",
        if p.boot { "yes" } else { "NO (--no-boot)" }
    );
}

/// Renders a flow's events as the CLI's text.
pub struct TextObserver<'c> {
    con: &'c mut Console,
    /// The chip named by the last `Device` event, for the lines that say
    /// which part a caveat applies to.
    family: String,
    /// The phase being drawn and the last percent printed for it. Progress is
    /// `\r`-redrawn on stderr and printed only when the percent changes, so
    /// large images do not flood the terminal.
    progress: Option<(Phase, usize)>,
}

impl<'c> TextObserver<'c> {
    pub fn new(con: &'c mut Console) -> TextObserver<'c> {
        TextObserver {
            con,
            family: String::new(),
            progress: None,
        }
    }

    fn draw_progress(&mut self, phase: Phase, done: usize, total: usize) {
        let label = match phase {
            Phase::Erase => "erase",
            Phase::Write => "write",
        };
        let last = match self.progress {
            Some((p, pct)) if p == phase => Some(pct),
            _ => None,
        };
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        if last != Some(pct) {
            err!(self.con, "\r{label}: {pct:3}% ({done}/{total} B)");
            let _ = self.con.err.flush();
        }
        if done >= total {
            // The phase is complete: end the redrawn line.
            errln!(self.con);
            self.progress = None;
        } else {
            self.progress = Some((phase, pct));
        }
    }
}

impl Observer for TextObserver<'_> {
    fn event(&mut self, event: Event<'_>) {
        let con = &mut *self.con;
        match event {
            Event::Device(info) => {
                self.family = info.family_name();
                print_device_brief(con, info);
            }
            Event::FlashPlanned(plan) => print_flash_plan(con, plan),
            Event::ErasePlanned {
                start,
                end,
                blocks,
                block,
            } => {
                outln!(con, "plan:");
                outln!(
                    con,
                    "  erase           0x{start:08X}..0x{end:08X} ({blocks} block{} x {block} B)",
                    if blocks == 1 { "" } else { "s" }
                );
                outln!(
                    con,
                    "  note: the first erase invalidates the boot record; the device \
                     stays in the bootloader until a new image is committed"
                );
            }
            Event::DryRun(Mutation::Flash) => {
                outln!(
                    con,
                    "\n(dry run — pass --force to actually erase and write)"
                );
            }
            Event::DryRun(Mutation::Erase) => {
                outln!(con, "\n(dry run — pass --force to actually erase)");
            }
            Event::Progress { phase, done, total } => self.draw_progress(phase, done, total),
            Event::Written { bytes, elapsed } => {
                outln!(
                    con,
                    "wrote {} bytes in {:.2} s",
                    bytes,
                    elapsed.as_secs_f64()
                );
            }
            Event::Committed { len, crc } => {
                outln!(con, "commit OK (len {}, crc32 0x{:08X})", len, crc);
            }
            Event::Verified(Verification::LiveCrc { device_crc }) => {
                outln!(con, "verify OK (device crc32 0x{device_crc:08X})");
            }
            Event::Verified(Verification::CommitOnly) => {
                outln!(
                    con,
                    "verify: no live CRC on {} (XIP may serve stale data this \
                     power cycle); COMMIT already attested the streamed image CRC",
                    self.family
                );
            }
            Event::Verified(Verification::Skipped) => {}
            Event::FlashBoot(FlashBoot::Started) => {
                outln!(con, "boot: device is starting the application");
            }
            Event::FlashBoot(FlashBoot::Skipped) => {
                outln!(
                    con,
                    "boot: skipped (--no-boot); device stays in the bootloader"
                );
            }
            Event::FlashBoot(FlashBoot::Parked) => {
                outln!(
                    con,
                    "boot: --no-boot over the QMK tunnel resets the module into the \
                     bootloader; it re-arms auto-boot and returns on its own"
                );
            }
            Event::CrcMayBeStale => {
                errln!(
                    con,
                    "note: {} has no FEAT_CRC_LIVE — CRC over flash written this \
                     power cycle may read stale data (F26); the result is \
                     authoritative after a power cycle",
                    self.family
                );
            }
            Event::CrcRead(c) => {
                outln!(
                    con,
                    "local  crc32 0x{:08X} ({} B @ 0x{:08X})",
                    c.local,
                    c.len,
                    c.base
                );
                outln!(con, "device crc32 0x{:08X}", c.device);
                if c.device == c.local {
                    outln!(con, "verify OK");
                }
            }
            Event::Erased { blocks } => outln!(con, "erase OK ({blocks} blocks)"),
            Event::Reset { stay: true } => {
                outln!(con, "device is resetting and staying in the bootloader");
            }
            Event::Reset { stay: false } => {
                outln!(con, "device is booting the application");
            }
            Event::Attesting { base, len, crc } => {
                outln!(
                    con,
                    "bless: attesting {len} B @ 0x{base:08X}, crc32 0x{crc:08X} (no erase/write)"
                );
            }
            Event::Blessed => outln!(con, "bless OK — boot record written"),
        }
    }
}

/* --- commands ----------------------------------------------------------- */
//
// One function per CLI subcommand that talks to a device: run the flow, print
// it. `main` adds only argument parsing and opening the transport.

pub fn probe(transport: &mut dyn Transport, con: &mut Console) -> Result<()> {
    let info = flows::probe(transport)?;
    print_device_info(con, &info);
    Ok(())
}

pub fn flash(
    transport: &mut dyn Transport,
    source: &Source,
    opts: &FlashOpts,
    con: &mut Console,
) -> Result<()> {
    flows::flash(transport, source, opts, &mut TextObserver::new(con))?;
    Ok(())
}

pub fn verify(transport: &mut dyn Transport, source: &Source, con: &mut Console) -> Result<()> {
    flows::verify(transport, source, &mut TextObserver::new(con))?;
    Ok(())
}

pub fn erase(
    transport: &mut dyn Transport,
    all: bool,
    start: Option<u32>,
    length: Option<u32>,
    force: bool,
    con: &mut Console,
) -> Result<()> {
    flows::erase(
        transport,
        all,
        start,
        length,
        force,
        &mut TextObserver::new(con),
    )?;
    Ok(())
}

pub fn boot(transport: &mut dyn Transport, stay: bool, con: &mut Console) -> Result<()> {
    flows::boot(transport, stay, &mut TextObserver::new(con))
}

pub fn bless(transport: &mut dyn Transport, source: &Source, con: &mut Console) -> Result<()> {
    flows::bless(transport, source, &mut TextObserver::new(con))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto;

    #[test]
    fn exit_codes() {
        let mismatch: anyhow::Error = VerifyMismatch {
            local_crc: 1,
            device_crc: Some(2),
        }
        .into();
        assert_eq!(exit_code(&mismatch), 2);
        // Context wrapping must not hide the mismatch.
        let wrapped = mismatch.context("while flashing");
        assert_eq!(exit_code(&wrapped), 2);

        assert_eq!(exit_code(&anyhow::anyhow!("io error")), 1);
        let dev: anyhow::Error = proto::DeviceError {
            status: 7,
            detail: None,
        }
        .into();
        assert_eq!(exit_code(&dev), 1);
    }
}
