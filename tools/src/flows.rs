//! High-level flows over any `Transport`: probe, flash, verify, erase,
//! boot, bless. Command encoding, timeouts, and the retry policy live one
//! layer down in [`crate::client`]; this module owns planning, progress
//! reporting, region checks, and the CRC-mismatch exit-code contract.

use std::fmt;
use std::io::Write as _;
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::client::{BootClient, BootMode, ClientError};
use crate::image::Image;
use crate::proto::consts::{OB_DET_VERIFY_MISMATCH, OB_E_VERIFY, OB_MAX_WRITE_DATA};
use crate::proto::device_info::DeviceInfo;
use crate::transport::Transport;

/// ERASE is chunked so progress ticks and per-request timeouts stay small.
pub const ERASE_CHUNK: u32 = 32 * 1024;

/// CRC comparison failure. `main` maps this (via downcast) to exit code 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyMismatch {
    pub local_crc: u32,
    /// `None` when the device rejected COMMIT with E_VERIFY:mismatch and
    /// therefore never disclosed the CRC it computed.
    pub device_crc: Option<u32>,
}

impl fmt::Display for VerifyMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.device_crc {
            Some(dev) => write!(
                f,
                "verify mismatch: local image CRC 0x{:08X}, device region CRC 0x{dev:08X}",
                self.local_crc
            ),
            None => write!(
                f,
                "verify mismatch: device rejected the committed image CRC 0x{:08X}",
                self.local_crc
            ),
        }
    }
}

impl std::error::Error for VerifyMismatch {}

/* --- progress ----------------------------------------------------------- */

/// `\r`-style percent progress on stderr, printed only when the percent
/// changes so large images do not flood the terminal.
struct Progress {
    label: &'static str,
    total: usize,
    last: Option<usize>,
}

impl Progress {
    fn new(label: &'static str, total: usize) -> Progress {
        Progress {
            label,
            total,
            last: None,
        }
    }

    fn update(&mut self, done: usize) {
        let pct = (done * 100).checked_div(self.total).unwrap_or(100);
        if self.last != Some(pct) {
            eprint!("\r{}: {pct:3}% ({done}/{} B)", self.label, self.total);
            let _ = std::io::stderr().flush();
            self.last = Some(pct);
        }
    }

    fn finish(self) {
        if self.last.is_some() {
            eprintln!();
        }
    }
}

/* --- shared helpers ----------------------------------------------------- */

fn print_device_info(info: &DeviceInfo) {
    let region_kib = (u64::from(info.app_end) - u64::from(info.app_start)) / 1024;
    println!("device:");
    println!(
        "  protocol        OBP {}.{}",
        info.proto_major, info.proto_minor
    );
    println!(
        "  bootloader      v{}.{} (0x{:04X})",
        info.bl_version >> 8,
        info.bl_version & 0xFF,
        info.bl_version
    );
    println!(
        "  chip            {} (rev 0x{:02X})",
        info.family_name(),
        info.chip_rev
    );
    println!("  transport       {}", info.transport_name());
    println!(
        "  app region      0x{:08X}..0x{:08X} ({region_kib} KiB)",
        info.app_start, info.app_end
    );
    println!("  erase block     {} B", info.erase_block);
    println!(
        "  write           page {} B, align {}, max {} B/frame",
        info.write_page, info.write_align, info.max_write_data
    );
    println!("  features        {}", info.feature_names());
    println!("  uid             {}", info.uid_hex());
    if let Some(w) = info.variant_mismatch() {
        eprintln!("WARNING: {w}");
    }
}

fn print_device_brief(info: &DeviceInfo) {
    println!(
        "device: {} rev 0x{:02X}, bootloader v{}.{}, uid {}",
        info.family_name(),
        info.chip_rev,
        info.bl_version >> 8,
        info.bl_version & 0xFF,
        info.uid_hex()
    );
    println!(
        "        app 0x{:08X}..0x{:08X}, erase block {} B, features {}",
        info.app_start,
        info.app_end,
        info.erase_block,
        info.feature_names()
    );
    if let Some(w) = info.variant_mismatch() {
        eprintln!("WARNING: {w}");
    }
}

/// `[base, base+len)` must lie inside the device-reported app region.
fn check_in_region(info: &DeviceInfo, base: u32, len: usize) -> Result<()> {
    let end = u64::from(base) + len as u64;
    if base < info.app_start || end > u64::from(info.app_end) {
        bail!(
            "image [0x{base:08X}, 0x{end:08X}) is outside the device app \
             region [0x{:08X}, 0x{:08X})",
            info.app_start,
            info.app_end
        );
    }
    Ok(())
}

/// Flash/bless additionally require `base == app_start`: COMMIT attests
/// `[app_start, app_start+len)` (CH57x stream CRC must even be written
/// sequentially from app_start), so an offset image could never be
/// committed or booted.
fn check_committable(info: &DeviceInfo, image: &Image) -> Result<()> {
    if image.base != info.app_start {
        bail!(
            "image base 0x{:08X} != device app_start 0x{:08X}; COMMIT attests \
             images starting at app_start, so flash/bless require the image \
             to be linked/based there",
            image.base,
            info.app_start
        );
    }
    check_in_region(info, image.base, image.bytes.len())
}

/// Bytes per WRITE frame: the device-reported maximum, capped at the
/// protocol ceiling (48 B).
fn write_chunk_len(info: &DeviceInfo) -> usize {
    usize::from(info.max_write_data).min(OB_MAX_WRITE_DATA)
}

fn erase_region(client: &mut BootClient, start: u32, total: u32, block: u32) -> Result<()> {
    let blocks_per_chunk = (ERASE_CHUNK / block).max(1);
    let chunk_len = blocks_per_chunk * block;
    let mut progress = Progress::new("erase", total as usize);
    let mut done = 0u32;
    progress.update(0);
    while done < total {
        let addr = start + done;
        let len = chunk_len.min(total - done);
        let blocks = len.div_ceil(block);
        client
            .erase(addr, len, blocks)
            .with_context(|| format!("ERASE @ 0x{addr:08X} (+{len} B)"))?;
        done += len;
        progress.update(done as usize);
    }
    progress.finish();
    Ok(())
}

fn read_device_crc(client: &mut BootClient, addr: u32, len: u32) -> Result<u32> {
    Ok(client.crc(addr, len)?)
}

/// COMMIT rejections with E_VERIFY:mismatch mean "flash content does not
/// match the attested CRC" — surface them as a VerifyMismatch, which
/// `main` maps to exit code 2. VerifyMismatch must remain the anyhow error
/// OBJECT (not a #[source] of one): anyhow's downcast sees through
/// .context() layers but not through a nested source chain.
fn map_commit_mismatch(e: ClientError, local_crc: u32) -> anyhow::Error {
    match &e {
        ClientError::Device { source, .. }
            if source.status == OB_E_VERIFY && source.detail == Some(OB_DET_VERIFY_MISMATCH) =>
        {
            VerifyMismatch {
                local_crc,
                device_crc: None,
            }
            .into()
        }
        _ => e.into(),
    }
}

fn boot_device(client: &mut BootClient, mode: BootMode) -> Result<()> {
    client.boot(mode).context(
        "BOOT is never retried (a lost response is indistinguishable \
             from a successful boot); re-run `openboot probe` to check the \
             device state",
    )?;
    Ok(())
}

/* --- flows --------------------------------------------------------------- */

/// HELLO and pretty-print the device info.
pub fn probe(transport: &mut dyn Transport) -> Result<()> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    print_device_info(&info);
    Ok(())
}

pub struct FlashOpts {
    /// Actually erase/write; without it, print the plan and stop (dry run).
    pub force: bool,
    /// Post-COMMIT CRC cross-check (skipped where not FEAT_CRC_LIVE).
    pub verify: bool,
    /// BOOT the application at the end.
    pub boot: bool,
}

/// Full flash flow: HELLO -> bounds check -> plan (dry run unless forced)
/// -> chunked ERASE -> sequential WRITEs -> COMMIT -> optional CRC
/// cross-check -> BOOT.
/// What `flash` intends to do, computed once from the image and the
/// device's HELLO. Printing it and executing it then read from the same
/// values — the dry run cannot describe a different operation than the
/// real run performs.
struct FlashPlan {
    len: usize,
    block: u32,
    blocks: u32,
    erase_len: u32,
    crc: u32,
    chunk: usize,
    writes: usize,
    verify: VerifyPlan,
    boot: bool,
}

/// How the flashed image will be checked. `CommitOnly` is not a weaker
/// choice but a device property: without FEAT_CRC_LIVE a post-write CRC
/// read may be served from a stale XIP view (CH57x F26), so COMMIT's
/// stream attestation is the authoritative check.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VerifyPlan {
    Skipped,
    CommitOnly,
    CommitPlusLiveCrc,
}

fn plan_flash(info: &DeviceInfo, image: &Image, opts: &FlashOpts) -> Result<FlashPlan> {
    check_committable(info, image)?;
    let len = image.bytes.len();
    let block = info.erase_block;
    let blocks = (len as u32).div_ceil(block);
    let erase_len = blocks * block;
    check_in_region(info, image.base, erase_len as usize)
        .context("erase span (image rounded up to whole blocks)")?;
    let chunk = write_chunk_len(info);
    Ok(FlashPlan {
        len,
        block,
        blocks,
        erase_len,
        crc: image.crc32(),
        chunk,
        writes: len.div_ceil(chunk),
        verify: if !opts.verify {
            VerifyPlan::Skipped
        } else if info.crc_live() {
            VerifyPlan::CommitPlusLiveCrc
        } else {
            VerifyPlan::CommitOnly
        },
        boot: opts.boot,
    })
}

fn print_flash_plan(image: &Image, p: &FlashPlan) {
    let (len, block, blocks, chunk, writes, crc) =
        (p.len, p.block, p.blocks, p.chunk, p.writes, p.crc);
    println!("plan:");
    println!("  base address    0x{:08X}", image.base);
    println!("  image length    {len} bytes (0x{len:X}), padded to 4 B");
    println!(
        "  erase           {blocks} block{} x {block} B = {} bytes",
        if blocks == 1 { "" } else { "s" },
        p.erase_len
    );
    println!(
        "  write           {writes} frame{} x <={chunk} B",
        if writes == 1 { "" } else { "s" }
    );
    println!("  image crc32     0x{crc:08X}");
    println!(
        "  verify          {}",
        match p.verify {
            VerifyPlan::Skipped => "NO (--no-verify)",
            VerifyPlan::CommitPlusLiveCrc => "COMMIT attestation + live CRC cross-check",
            VerifyPlan::CommitOnly => "COMMIT attestation only (no FEAT_CRC_LIVE on this chip)",
        }
    );
    println!(
        "  boot after      {}",
        if p.boot { "yes" } else { "NO (--no-boot)" }
    );
}

fn execute_flash(
    client: &mut BootClient,
    info: &DeviceInfo,
    image: &Image,
    p: &FlashPlan,
) -> Result<()> {
    // ERASE. The device invalidates the boot record before the session's
    // first mutation, so an interrupted update stays in the bootloader.
    erase_region(client, image.base, p.erase_len, p.block)?;

    // WRITE, sequentially from base (required for the CH57x stream CRC).
    let started = Instant::now();
    let mut progress = Progress::new("write", p.len);
    progress.update(0);
    let mut off = 0usize;
    while off < p.len {
        let end = (off + p.chunk).min(p.len);
        let addr = image.base + off as u32;
        client
            .write(addr, &image.bytes[off..end])
            .with_context(|| format!("WRITE @ 0x{addr:08X}"))?;
        off = end;
        progress.update(off);
    }
    progress.finish();
    println!(
        "wrote {} bytes in {:.2} s",
        p.len,
        started.elapsed().as_secs_f64()
    );

    // COMMIT: the device checks the image CRC (stream CRC on CH57x, direct
    // flash CRC on CH59x) and writes the boot record on match.
    if let Err(e) = client.commit(p.len as u32, p.crc) {
        return Err(map_commit_mismatch(e, p.crc));
    }
    println!("commit OK (len {}, crc32 0x{:08X})", p.len, p.crc);

    match p.verify {
        VerifyPlan::CommitPlusLiveCrc => {
            let device_crc = read_device_crc(client, image.base, p.len as u32)?;
            if device_crc != p.crc {
                return Err(VerifyMismatch {
                    local_crc: p.crc,
                    device_crc: Some(device_crc),
                }
                .into());
            }
            println!("verify OK (device crc32 0x{device_crc:08X})");
        }
        VerifyPlan::CommitOnly => println!(
            "verify: no live CRC on {} (XIP may serve stale data this \
             power cycle); COMMIT already attested the streamed image CRC",
            info.family_name()
        ),
        VerifyPlan::Skipped => {}
    }

    if p.boot {
        boot_device(client, BootMode::App)?;
        println!("boot: device is starting the application");
    } else {
        println!("boot: skipped (--no-boot); device stays in the bootloader");
    }
    Ok(())
}

pub fn flash(transport: &mut dyn Transport, image: &Image, opts: &FlashOpts) -> Result<()> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    print_device_brief(&info);

    let plan = plan_flash(&info, image, opts)?;
    print_flash_plan(image, &plan);
    if !opts.force {
        println!("\n(dry run — pass --force to actually erase and write)");
        return Ok(());
    }
    execute_flash(&mut client, &info, image, &plan)
}

/// Read-only comparison: device CRC over `[base, base+len)` vs local image.
pub fn verify(transport: &mut dyn Transport, image: &Image) -> Result<()> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    print_device_brief(&info);
    check_in_region(&info, image.base, image.bytes.len())?;

    if !info.crc_live() {
        eprintln!(
            "note: {} has no FEAT_CRC_LIVE — CRC over flash written this \
             power cycle may read stale data (F26); the result is \
             authoritative after a power cycle",
            info.family_name()
        );
    }
    let len = image.bytes.len();
    let local = image.crc32();
    let device = read_device_crc(&mut client, image.base, len as u32)?;
    println!(
        "local  crc32 0x{local:08X} ({len} B @ 0x{:08X})",
        image.base
    );
    println!("device crc32 0x{device:08X}");
    if device != local {
        return Err(VerifyMismatch {
            local_crc: local,
            device_crc: Some(device),
        }
        .into());
    }
    println!("verify OK");
    Ok(())
}

/// Erase `--all` (whole app region) or an explicit block-aligned range.
/// Dry run unless forced.
pub fn erase(
    transport: &mut dyn Transport,
    all: bool,
    start: Option<u32>,
    length: Option<u32>,
    force: bool,
) -> Result<()> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    print_device_brief(&info);

    let (start, len) = if all {
        (info.app_start, info.app_end - info.app_start)
    } else {
        match (start, length) {
            (Some(s), Some(l)) => (s, l),
            _ => bail!("erase needs --all, or both --start and --length"),
        }
    };
    if len == 0 {
        bail!("erase length is zero");
    }
    let block = info.erase_block;
    if start % block != 0 || len % block != 0 {
        bail!(
            "erase range 0x{start:08X}+0x{len:X} is not aligned to the \
             {block}-byte erase block"
        );
    }
    let end = u64::from(start) + u64::from(len);
    if start < info.app_start || end > u64::from(info.app_end) {
        bail!(
            "erase range [0x{start:08X}, 0x{end:08X}) is outside the app \
             region [0x{:08X}, 0x{:08X})",
            info.app_start,
            info.app_end
        );
    }

    let blocks = len / block;
    println!("plan:");
    println!(
        "  erase           0x{start:08X}..0x{end:08X} ({blocks} block{} x {block} B)",
        if blocks == 1 { "" } else { "s" }
    );
    println!(
        "  note: the first erase invalidates the boot record; the device \
         stays in the bootloader until a new image is committed"
    );
    if !force {
        println!("\n(dry run — pass --force to actually erase)");
        return Ok(());
    }
    erase_region(&mut client, start, len, block)?;
    println!("erase OK ({blocks} blocks)");
    Ok(())
}

/// BOOT the app (mode 0) or reset staying in the bootloader (`--stay`).
pub fn boot(transport: &mut dyn Transport, stay: bool) -> Result<()> {
    let mut client = BootClient::new(transport);
    let _info = client.hello()?; // BOOT requires a session (E_STATE otherwise)
    let mode = if stay { BootMode::Stay } else { BootMode::App };
    boot_device(&mut client, mode)?;
    if stay {
        println!("device is resetting and staying in the bootloader");
    } else {
        println!("device is booting the application");
    }
    Ok(())
}

/// Zero-write COMMIT for images already in flash (e.g. SWD-flashed dev
/// builds): the device CRCs flash directly and writes the boot record.
pub fn bless(transport: &mut dyn Transport, image: &Image) -> Result<()> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    print_device_brief(&info);
    check_committable(&info, image)?;

    let len = image.bytes.len();
    let crc = image.crc32();
    println!(
        "bless: attesting {len} B @ 0x{:08X}, crc32 0x{crc:08X} (no erase/write)",
        image.base
    );
    if let Err(e) = client.commit(len as u32, crc) {
        return Err(map_commit_mismatch(e, crc));
    }
    println!("bless OK — boot record written");
    Ok(())
}

#[cfg(test)]
mod tests;
