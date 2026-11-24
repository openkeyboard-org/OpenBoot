//! High-level flows over any `Transport`: probe, flash, verify, erase,
//! boot, bless. Command encoding, timeouts, and the retry policy live one
//! layer down in [`crate::client`]; this module owns planning, region checks,
//! and the CRC-mismatch contract.
//!
//! Nothing here prints. A flow reports each step to an [`Observer`] as it
//! happens and returns what it found, so the same code serves the CLI (which
//! renders the events as text, see [`crate::console`]) and a caller that
//! wants to draw them some other way. Pass `&mut ()` to observe nothing.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::bundle::Source;
use crate::client::{BootClient, BootMode, ClientError};
use crate::image::Image;
use crate::proto::consts::{OB_DET_VERIFY_MISMATCH, OB_E_VERIFY};
use crate::proto::device_info::DeviceInfo;
use crate::transport::Transport;

/// ERASE is chunked so progress ticks and per-request timeouts stay small.
pub const ERASE_CHUNK: u32 = 32 * 1024;

/// CRC comparison failure. The CLI maps this (via downcast) to exit code 2.
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

/* --- observing a flow --------------------------------------------------- */

/// The two stretches of a flow that take long enough to show progress for,
/// and the only ones an observer can stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Erase,
    Write,
}

/// Which flow stopped at its plan for want of `force`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    Flash,
    Erase,
}

/// How a flashed image was checked.
///
/// `CommitOnly` is not a weaker choice but a device property: without
/// FEAT_CRC_LIVE a post-write CRC read may be served from a stale XIP view
/// (CH57x F26), so COMMIT's stream attestation is the authoritative check and
/// no cross-check was attempted. Callers must not present the three as one
/// "verified".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The caller asked for no cross-check; COMMIT still attested the image.
    Skipped,
    CommitOnly,
    /// COMMIT attested the image and a live CRC read of flash agreed.
    LiveCrc {
        device_crc: u32,
    },
}

/// What `flash` did about booting once the image was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashBoot {
    /// BOOT was sent; the device is starting the application.
    Started,
    /// Not asked to boot, and the transport holds the device in the bootloader.
    Skipped,
    /// Not asked to boot over a transport that cannot hold the device (the QMK
    /// tunnel): it was reset into the bootloader, whose idle timer then
    /// returns it to the application on its own.
    Parked,
}

/// A device CRC read next to the local image's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrcCheck {
    pub base: u32,
    pub len: usize,
    pub local: u32,
    pub device: u32,
}

/// One step of a flow, reported as it happens.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// HELLO answered. Every later event in the flow is about this device.
    Device(&'a DeviceInfo),
    /// What `flash` will do — or, on a dry run, would have done.
    FlashPlanned(&'a FlashPlan),
    /// What `erase` will do — or, on a dry run, would have done.
    ErasePlanned {
        start: u32,
        end: u64,
        blocks: u32,
        block: u32,
    },
    /// The flow stopped at its plan; nothing was erased or written.
    DryRun(Mutation),
    /// `done` of `total` bytes. Sent once at zero, then after every request.
    Progress {
        phase: Phase,
        done: usize,
        total: usize,
    },
    /// Every WRITE was acknowledged.
    Written {
        bytes: usize,
        elapsed: Duration,
    },
    /// COMMIT accepted: the device attested the image and wrote its record.
    Committed {
        len: usize,
        crc: u32,
    },
    Verified(Verification),
    FlashBoot(FlashBoot),
    /// `verify` is about to read a CRC the device does not vouch for: with no
    /// FEAT_CRC_LIVE, flash written this power cycle may read back stale.
    CrcMayBeStale,
    /// `verify` read the device CRC. Sent before the comparison, so it is seen
    /// on a mismatch too.
    CrcRead(CrcCheck),
    Erased {
        blocks: u32,
    },
    /// `boot` sent BOOT: the device is resetting, into the application or
    /// (`stay`) back into the bootloader.
    Reset {
        stay: bool,
    },
    /// `bless` is about to COMMIT an image already in flash.
    Attesting {
        base: u32,
        len: usize,
        crc: u32,
    },
    Blessed,
}

/// Receives a flow's events, and may stop it.
pub trait Observer {
    fn event(&mut self, event: Event<'_>);

    /// Asked before every ERASE and WRITE request and once more before
    /// COMMIT. Returning true ends the flow there with [`Cancelled`].
    ///
    /// It is not asked from COMMIT onwards: a committed image is finished
    /// work, and BOOT is never something to abandon half-way. Stopping does
    /// not reset the device — its session stays open and it stays in the
    /// bootloader, so the caller decides whether to boot it back.
    fn cancelled(&mut self) -> bool {
        false
    }
}

/// Observes nothing and never cancels.
impl Observer for () {
    fn event(&mut self, _event: Event<'_>) {}
}

/// The observer stopped the flow. No COMMIT was sent, so the slot being
/// written holds a partial image the device will not boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled {
    pub phase: Phase,
}

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let during = match self.phase {
            Phase::Erase => "erasing",
            Phase::Write => "writing",
        };
        write!(f, "cancelled while {during}; no COMMIT was sent")
    }
}

impl std::error::Error for Cancelled {}

fn check_cancelled(observer: &mut dyn Observer, phase: Phase) -> Result<()> {
    if observer.cancelled() {
        return Err(Cancelled { phase }.into());
    }
    Ok(())
}

/* --- shared helpers ----------------------------------------------------- */

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

/// The window this session may mutate. A zero capacity is a truthful report
/// from a device whose silicon cannot hold the slot its build laid out — a
/// wrong-variant bootloader — and every ERASE and WRITE would come back as a
/// bare range error, so say what is actually wrong instead.
fn write_window(info: &DeviceInfo) -> Result<(u32, u64)> {
    if info.write_capacity == 0 {
        bail!(
            "device reports no writable slot: slot {} does not fit this \
             silicon, whose app region ends at 0x{:08X}. The bootloader was \
             built for a larger variant of this family.",
            DeviceInfo::slot_name(info.write_slot),
            info.app_end
        );
    }
    Ok((
        info.write_base,
        u64::from(info.write_base) + u64::from(info.write_capacity),
    ))
}

/// `[base, base+len)` must lie inside the writable slot. Mutations are
/// bounded by the slot, not by the app region — the device enforces this
/// too, and it is what keeps the currently bootable image out of reach.
fn check_in_write_window(info: &DeviceInfo, base: u32, len: usize) -> Result<()> {
    let (wbase, wend) = write_window(info)?;
    let end = u64::from(base) + len as u64;
    if base < wbase || end > wend {
        bail!(
            "[0x{base:08X}, 0x{end:08X}) is outside the device's writable \
             slot {} window [0x{wbase:08X}, 0x{wend:08X})",
            DeviceInfo::slot_name(info.write_slot)
        );
    }
    Ok(())
}

/// Flash/bless additionally require `base == write_base`: COMMIT attests
/// `[write_base, write_base+len)` (on CH57x the stream CRC must even be
/// written sequentially from there), so an offset image could never be
/// committed or booted.
///
/// That base is the INACTIVE slot, which alternates between updates, so the
/// artifact the device will accept changes from one update to the next. An
/// image is linked for one slot base and cannot be relocated — see
/// docs/AB-UPDATE.md.
fn check_committable(info: &DeviceInfo, image: &Image) -> Result<()> {
    if image.base != info.write_base {
        bail!(
            "image base 0x{:08X} != device write base 0x{:08X} (slot {}); \
             COMMIT attests images starting at the write base, so flash and \
             bless need an image linked for that slot",
            image.base,
            info.write_base,
            DeviceInfo::slot_name(info.write_slot)
        );
    }
    check_in_write_window(info, image.base, image.bytes.len())
}

/// Bytes per WRITE frame. DeviceInfo parsing has already enforced the
/// protocol ceiling.
fn write_chunk_len(info: &DeviceInfo) -> usize {
    usize::from(info.max_write_data)
}

fn erase_region(
    client: &mut BootClient,
    start: u32,
    total: u32,
    block: u32,
    observer: &mut dyn Observer,
) -> Result<()> {
    let blocks_per_chunk = (ERASE_CHUNK / block).max(1);
    let chunk_len = blocks_per_chunk * block;
    let progress = |done: u32| Event::Progress {
        phase: Phase::Erase,
        done: done as usize,
        total: total as usize,
    };
    let mut done = 0u32;
    observer.event(progress(0));
    while done < total {
        check_cancelled(observer, Phase::Erase)?;
        let addr = start + done;
        let len = chunk_len.min(total - done);
        let blocks = len.div_ceil(block);
        client
            .erase(addr, len, blocks)
            .with_context(|| format!("ERASE @ 0x{addr:08X} (+{len} B)"))?;
        done += len;
        observer.event(progress(done));
    }
    Ok(())
}

fn read_device_crc(client: &mut BootClient, addr: u32, len: u32) -> Result<u32> {
    Ok(client.crc(addr, len)?)
}

/// COMMIT rejections with E_VERIFY:mismatch mean "flash content does not
/// match the attested CRC" — surface them as a VerifyMismatch, which the CLI
/// maps to exit code 2. VerifyMismatch must remain the anyhow error OBJECT
/// (not a `#[source]` of one): anyhow's downcast sees through .context()
/// layers but not through a nested source chain.
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

/// HELLO, and what the device said about itself.
pub fn probe(transport: &mut dyn Transport) -> Result<DeviceInfo> {
    let mut client = BootClient::new(transport);
    Ok(client.hello()?)
}

pub struct FlashOpts {
    /// Actually erase/write; without it, stop at the plan (dry run).
    pub force: bool,
    /// Post-COMMIT CRC cross-check (skipped where not FEAT_CRC_LIVE).
    pub verify: bool,
    /// BOOT the application at the end.
    pub boot: bool,
}

/// What `flash` intends to do, computed once from the image and the
/// device's HELLO. Showing it and executing it then read from the same
/// values — the dry run cannot describe a different operation than the
/// real run performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashPlan {
    /// Where the image starts: the device's write base.
    pub base: u32,
    pub len: usize,
    pub block: u32,
    pub blocks: u32,
    pub erase_len: u32,
    pub crc: u32,
    pub chunk: usize,
    pub writes: usize,
    pub verify: VerifyPlan,
    pub boot: bool,
}

/// How the flashed image will be checked; see [`Verification`] for why
/// `CommitOnly` is a property of the device and not a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPlan {
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
    check_in_write_window(info, image.base, erase_len as usize)
        .context("erase span (image rounded up to whole blocks)")?;
    let chunk = write_chunk_len(info);
    Ok(FlashPlan {
        base: image.base,
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

/// A flash that has been checked against a live device and not yet started:
/// the device that answered, the image chosen for the slot it offered, and
/// the plan derived from the two. [`execute_flash`] runs exactly this.
#[derive(Debug, Clone)]
pub struct FlashJob<'s> {
    pub info: DeviceInfo,
    pub image: &'s Image,
    pub plan: FlashPlan,
}

/// What a completed flash did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashDone {
    pub verification: Verification,
    pub boot: FlashBoot,
}

/// The result of [`flash`]. `done` is `None` on a dry run.
#[derive(Debug, Clone)]
pub struct FlashReport {
    pub info: DeviceInfo,
    pub plan: FlashPlan,
    pub done: Option<FlashDone>,
}

/// HELLO, choose the image for the slot the device offers, and check that it
/// fits. Erases and writes nothing — but a successful HELLO does open a
/// session, which stops the bootloader's idle auto-boot until the device is
/// booted or reset.
///
/// `client` must be the one later handed to [`execute_flash`]: a second HELLO
/// in between would restart the device's session.
pub fn prepare_flash<'s>(
    client: &mut BootClient,
    source: &'s Source,
    opts: &FlashOpts,
    observer: &mut dyn Observer,
) -> Result<FlashJob<'s>> {
    let info = client.hello()?;
    observer.event(Event::Device(&info));

    // Which build to send is the device's answer, not the operator's: it
    // names the slot it will write, and an application only runs at the base
    // it was linked for.
    source.check_family(info.chip_family, &info.family_name())?;
    let image = source.for_base(info.write_base, "device write")?;

    let plan = plan_flash(&info, image, opts)?;
    observer.event(Event::FlashPlanned(&plan));
    Ok(FlashJob { info, image, plan })
}

/// Chunked ERASE -> sequential WRITEs -> COMMIT -> optional CRC cross-check
/// -> BOOT, exactly as `job` planned it.
pub fn execute_flash(
    client: &mut BootClient,
    job: &FlashJob,
    observer: &mut dyn Observer,
) -> Result<FlashDone> {
    let (image, p) = (job.image, &job.plan);

    // ERASE. The device invalidates the boot record before the session's
    // first mutation, so an interrupted update stays in the bootloader.
    erase_region(client, image.base, p.erase_len, p.block, observer)?;

    // WRITE, sequentially from base (required for the CH57x stream CRC).
    let started = Instant::now();
    let progress = |done: usize| Event::Progress {
        phase: Phase::Write,
        done,
        total: p.len,
    };
    observer.event(progress(0));
    let mut off = 0usize;
    while off < p.len {
        check_cancelled(observer, Phase::Write)?;
        let end = (off + p.chunk).min(p.len);
        let addr = image.base + off as u32;
        client
            .write(addr, &image.bytes[off..end])
            .with_context(|| format!("WRITE @ 0x{addr:08X}"))?;
        off = end;
        observer.event(progress(off));
    }
    observer.event(Event::Written {
        bytes: p.len,
        elapsed: started.elapsed(),
    });

    // The last point at which stopping leaves nothing committed.
    check_cancelled(observer, Phase::Write)?;

    // COMMIT: the device checks the image CRC (stream CRC on CH57x, direct
    // flash CRC on CH59x) and writes the boot record on match.
    if let Err(e) = client.commit(p.len as u32, p.crc) {
        return Err(map_commit_mismatch(e, p.crc));
    }
    observer.event(Event::Committed {
        len: p.len,
        crc: p.crc,
    });

    let verification = match p.verify {
        VerifyPlan::CommitPlusLiveCrc => {
            let device_crc = read_device_crc(client, image.base, p.len as u32)?;
            if device_crc != p.crc {
                return Err(VerifyMismatch {
                    local_crc: p.crc,
                    device_crc: Some(device_crc),
                }
                .into());
            }
            Verification::LiveCrc { device_crc }
        }
        VerifyPlan::CommitOnly => Verification::CommitOnly,
        VerifyPlan::Skipped => Verification::Skipped,
    };
    observer.event(Event::Verified(verification));

    let boot = if p.boot {
        boot_device(client, BootMode::App)?;
        FlashBoot::Started
    } else if client.holds_in_bootloader() {
        FlashBoot::Skipped
    } else {
        // The tunnel holds the session by disabling the module's idle auto-boot,
        // so leaving it un-booted would strand the keyboard off air. Reset it
        // into the bootloader instead (BOOT --stay): that re-arms its own idle
        // timer, so it comes back on its own, and the app is never launched here.
        boot_device(client, BootMode::Stay)?;
        FlashBoot::Parked
    };
    observer.event(Event::FlashBoot(boot));

    Ok(FlashDone { verification, boot })
}

/// Full flash flow: HELLO -> bounds check -> plan (dry run unless forced)
/// -> chunked ERASE -> sequential WRITEs -> COMMIT -> optional CRC
/// cross-check -> BOOT.
pub fn flash(
    transport: &mut dyn Transport,
    source: &Source,
    opts: &FlashOpts,
    observer: &mut dyn Observer,
) -> Result<FlashReport> {
    let mut client = BootClient::new(transport);
    let job = prepare_flash(&mut client, source, opts, observer)?;
    let done = if opts.force {
        Some(execute_flash(&mut client, &job, observer)?)
    } else {
        observer.event(Event::DryRun(Mutation::Flash));
        None
    };
    Ok(FlashReport {
        info: job.info,
        plan: job.plan,
        done,
    })
}

/// Read-only comparison: device CRC over `[base, base+len)` vs local image.
/// A difference is an `Err` carrying [`VerifyMismatch`].
pub fn verify(
    transport: &mut dyn Transport,
    source: &Source,
    observer: &mut dyn Observer,
) -> Result<CrcCheck> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    observer.event(Event::Device(&info));
    source.check_family(info.chip_family, &info.family_name())?;
    // Verify checks what the device is RUNNING, which is never the slot it
    // offers to write.
    let image = source.for_running_image(info.write_base)?;
    check_in_region(&info, image.base, image.bytes.len())?;

    if !info.crc_live() {
        observer.event(Event::CrcMayBeStale);
    }
    let len = image.bytes.len();
    let local = image.crc32();
    let device = read_device_crc(&mut client, image.base, len as u32)?;
    let check = CrcCheck {
        base: image.base,
        len,
        local,
        device,
    };
    observer.event(Event::CrcRead(check));
    if device != local {
        return Err(VerifyMismatch {
            local_crc: local,
            device_crc: Some(device),
        }
        .into());
    }
    Ok(check)
}

/// What [`erase`] covered. `erased` is false on a dry run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EraseReport {
    pub start: u32,
    pub len: u32,
    pub blocks: u32,
    pub erased: bool,
}

/// Erase `all` (the whole writable slot) or an explicit block-aligned range.
/// Dry run unless forced.
pub fn erase(
    transport: &mut dyn Transport,
    all: bool,
    start: Option<u32>,
    length: Option<u32>,
    force: bool,
    observer: &mut dyn Observer,
) -> Result<EraseReport> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    observer.event(Event::Device(&info));

    let (start, len) = if all {
        let (wbase, wend) = write_window(&info)?;
        (wbase, (wend - u64::from(wbase)) as u32)
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
    let (wbase, wend) = write_window(&info)?;
    if start < wbase || end > wend {
        bail!(
            "erase range [0x{start:08X}, 0x{end:08X}) is outside the \
             device's writable slot {} window [0x{wbase:08X}, 0x{wend:08X})",
            DeviceInfo::slot_name(info.write_slot)
        );
    }

    let blocks = len / block;
    observer.event(Event::ErasePlanned {
        start,
        end,
        blocks,
        block,
    });
    let report = |erased| EraseReport {
        start,
        len,
        blocks,
        erased,
    };
    if !force {
        observer.event(Event::DryRun(Mutation::Erase));
        return Ok(report(false));
    }
    erase_region(&mut client, start, len, block, observer)?;
    observer.event(Event::Erased { blocks });
    Ok(report(true))
}

/// BOOT the app (mode 0) or reset staying in the bootloader (`stay`).
pub fn boot(transport: &mut dyn Transport, stay: bool, observer: &mut dyn Observer) -> Result<()> {
    let mut client = BootClient::new(transport);
    let _info = client.hello()?; // BOOT requires a session (E_STATE otherwise)
    let mode = if stay { BootMode::Stay } else { BootMode::App };
    boot_device(&mut client, mode)?;
    observer.event(Event::Reset { stay });
    Ok(())
}

/// Zero-write COMMIT for images already in flash (e.g. SWD-flashed dev
/// builds): the device CRCs flash directly and writes the boot record.
pub fn bless(
    transport: &mut dyn Transport,
    source: &Source,
    observer: &mut dyn Observer,
) -> Result<()> {
    let mut client = BootClient::new(transport);
    let info = client.hello()?;
    observer.event(Event::Device(&info));
    source.check_family(info.chip_family, &info.family_name())?;
    // Bless attests an image already in flash at the base COMMIT will read
    // from, which is the write slot - same selection rule as flash.
    let image = source.for_base(info.write_base, "device write")?;
    check_committable(&info, image)?;

    let len = image.bytes.len();
    let crc = image.crc32();
    observer.event(Event::Attesting {
        base: image.base,
        len,
        crc,
    });
    if let Err(e) = client.commit(len as u32, crc) {
        return Err(map_commit_mismatch(e, crc));
    }
    observer.event(Event::Blessed);
    Ok(())
}

#[cfg(test)]
mod tests;
