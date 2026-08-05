//! openboot — host tool for the OpenBoot IAP bootloader (OBP v0.2) on WCH
//! CH570/CH572/CH591/CH592, over USB HID or UART.
//!
//! Safe by default: `probe` (the default subcommand) is read-only, and
//! `flash`/`erase` print a plan and stop unless `--force` is given.

mod client;
mod flows;
mod hex;
mod image;
mod proto;
#[cfg(test)]
mod testutil;
mod transport;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand, ValueEnum};

use flows::{FlashOpts, VerifyMismatch};
use image::load_image;
use proto::consts::OB_UART_BAUD;
use transport::hid::{HidTransport, DEFAULT_PID, DEFAULT_VID};
use transport::uart::UartTransport;
use transport::Transport;

/// Parse an integer the way Python's `int(s, 0)` does: 0x/0o/0b prefixes
/// select the radix, otherwise decimal.
fn parse_int_auto(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let r = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16)
    } else if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        u64::from_str_radix(o, 8)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(b, 2)
    } else {
        t.parse::<u64>()
    };
    r.map_err(|e| format!("invalid integer '{s}': {e}"))
}

fn parse_u16_auto(s: &str) -> Result<u16, String> {
    let v = parse_int_auto(s)?;
    u16::try_from(v).map_err(|_| format!("value out of range for u16: {s}"))
}

fn parse_u32_auto(s: &str) -> Result<u32, String> {
    let v = parse_int_auto(s)?;
    u32::try_from(v).map_err(|_| format!("value out of range for u32: {s}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum TransportArg {
    Usb,
    Uart,
}

#[derive(Parser)]
#[command(
    name = "openboot",
    version = env!("CARGO_PKG_VERSION"),
    about = "Flash and manage OpenBoot (OBP v0.2) bootloaders on WCH CH57x/CH59x chips.",
    long_about = "Flash and manage OpenBoot (OBP v0.2) bootloaders on WCH \
        CH570/CH572/CH591/CH592 chips over USB HID or UART.\n\n\
        Safe by default: `probe` (the default) is read-only; `flash` and \
        `erase` print a plan and stop unless --force is given.\n\n\
        Exit codes: 0 ok, 1 device/IO/usage error, 2 verify mismatch.",
    after_help = "Examples:\n  \
        openboot                              # probe over USB HID\n  \
        openboot flash app.bin                # dry-run plan\n  \
        openboot flash app.bin --force        # erase/write/commit/verify/boot\n  \
        openboot --port /dev/ttyUSB0 probe    # probe over UART\n  \
        openboot verify app.hex               # compare flash CRC vs file\n  \
        openboot bless app.bin                # write boot record for an SWD-flashed image\n  \
        openboot boot --stay                  # reset, stay in bootloader"
)]
struct Cli {
    /// Transport (default usb; --port implies uart, --serial implies usb)
    #[arg(long, global = true, value_enum)]
    transport: Option<TransportArg>,

    /// Serial port path for UART (e.g. /dev/ttyUSB0); implies --transport uart
    #[arg(long, global = true, value_name = "PATH")]
    port: Option<String>,

    /// USB serial number filter (16-hex device UID); implies --transport usb
    #[arg(long, global = true, value_name = "SN")]
    serial: Option<String>,

    /// USB VID (default 0x1209)
    #[arg(long, global = true, value_parser = parse_u16_auto, value_name = "VID")]
    vid: Option<u16>,

    /// USB PID (default 0x0001)
    #[arg(long, global = true, value_parser = parse_u16_auto, value_name = "PID")]
    pid: Option<u16>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Query and display bootloader/device information (default action)
    Probe,

    /// Flash IMG: erase, write, commit, verify, boot (dry run without --force)
    Flash {
        /// .hex (Intel HEX) or flat .bin image (default base 0x2000)
        image: PathBuf,

        /// Load base for .bin images (default 0x2000 = app_start)
        #[arg(long, value_parser = parse_u32_auto, value_name = "ADDR")]
        base: Option<u32>,

        /// Actually erase/write (default: print the plan and exit)
        #[arg(long)]
        force: bool,

        /// Skip the post-commit live CRC cross-check
        #[arg(long)]
        no_verify: bool,

        /// Stay in the bootloader instead of booting the app afterwards
        #[arg(long)]
        no_boot: bool,
    },

    /// Compare device flash CRC against IMG (read-only)
    #[command(long_about = "Compare device flash CRC against IMG (read-only).\n\n\
        CH57x caveat: the CH570/CH572 flash controller can serve stale data \
        over XIP for flash written earlier in the same power cycle (errata \
        F26). Such devices clear FEAT_CRC_LIVE and this comparison is only \
        authoritative on a fresh boot, i.e. after a power cycle. CH59x \
        devices report FEAT_CRC_LIVE and the result is always authoritative. \
        The COMMIT attestation performed by `flash` is unaffected either way.")]
    Verify {
        /// .hex (Intel HEX) or flat .bin image (default base 0x2000)
        image: PathBuf,

        /// Load base for .bin images (default 0x2000)
        #[arg(long, value_parser = parse_u32_auto, value_name = "ADDR")]
        base: Option<u32>,
    },

    /// Erase app-region flash (dry run without --force)
    Erase {
        /// Erase the whole app region
        #[arg(long, conflicts_with_all = ["start", "length"])]
        all: bool,

        /// Start address (erase-block aligned)
        #[arg(long, value_parser = parse_u32_auto, value_name = "ADDR", requires = "length")]
        start: Option<u32>,

        /// Length in bytes (erase-block multiple)
        #[arg(long, value_parser = parse_u32_auto, value_name = "LEN", requires = "start")]
        length: Option<u32>,

        /// Actually erase (default: print the plan and exit)
        #[arg(long)]
        force: bool,
    },

    /// Boot the application (validates the boot record first)
    Boot {
        /// Reset and stay in the bootloader instead
        #[arg(long)]
        stay: bool,
    },

    /// Write the boot record for an image already in flash (e.g. flashed
    /// over SWD): zero-write COMMIT — the device CRCs flash directly
    Bless {
        /// .hex (Intel HEX) or flat .bin image (default base 0x2000)
        image: PathBuf,

        /// Load base for .bin images (default 0x2000)
        #[arg(long, value_parser = parse_u32_auto, value_name = "ADDR")]
        base: Option<u32>,
    },
}

fn resolve_transport_kind(
    transport: Option<TransportArg>,
    has_port: bool,
    has_serial: bool,
) -> Result<TransportArg> {
    match (transport, has_port, has_serial) {
        (Some(TransportArg::Usb), true, _) => {
            bail!("--port selects the UART transport and conflicts with --transport usb")
        }
        (Some(TransportArg::Uart), _, true) => {
            bail!("--serial selects a USB device and conflicts with --transport uart")
        }
        (Some(kind), _, _) => Ok(kind),
        (None, true, true) => bail!("--port (uart) and --serial (usb) conflict; pick one"),
        (None, true, false) => Ok(TransportArg::Uart),
        (None, false, _) => Ok(TransportArg::Usb),
    }
}

fn open_transport(cli: &Cli) -> Result<Box<dyn Transport>> {
    let kind = resolve_transport_kind(cli.transport, cli.port.is_some(), cli.serial.is_some())?;
    match kind {
        TransportArg::Usb => {
            if cli.port.is_some() {
                bail!("--port is a UART option; use --transport uart");
            }
            let vid = cli.vid.unwrap_or(DEFAULT_VID);
            let pid = cli.pid.unwrap_or(DEFAULT_PID);
            let t = HidTransport::open(vid, pid, cli.serial.as_deref())?;
            eprintln!(
                "opened HID device {} (VID=0x{vid:04X} PID=0x{pid:04X})",
                t.path
            );
            Ok(Box::new(t))
        }
        TransportArg::Uart => {
            let Some(port) = cli.port.as_deref() else {
                bail!("--transport uart requires --port PATH (no port auto-scan)");
            };
            if cli.vid.is_some() || cli.pid.is_some() {
                bail!("--vid/--pid are USB options and do not apply to UART");
            }
            let t = UartTransport::open(port)?;
            eprintln!("opened serial port {} ({OB_UART_BAUD} 8N1)", t.path);
            Ok(Box::new(t))
        }
    }
}

fn run(mut cli: Cli) -> Result<()> {
    let cmd = cli.cmd.take().unwrap_or(Cmd::Probe);
    match cmd {
        Cmd::Probe => {
            let mut t = open_transport(&cli)?;
            flows::probe(t.as_mut())
        }
        Cmd::Flash {
            image,
            base,
            force,
            no_verify,
            no_boot,
        } => {
            let img = load_image(&image, base)?;
            println!(
                "loaded {}: base 0x{:08X}, {} bytes, crc32 0x{:08X}",
                image.display(),
                img.base,
                img.bytes.len(),
                img.crc32()
            );
            let mut t = open_transport(&cli)?;
            flows::flash(
                t.as_mut(),
                &img,
                &FlashOpts {
                    force,
                    verify: !no_verify,
                    boot: !no_boot,
                },
            )
        }
        Cmd::Verify { image, base } => {
            let img = load_image(&image, base)?;
            let mut t = open_transport(&cli)?;
            flows::verify(t.as_mut(), &img)
        }
        Cmd::Erase {
            all,
            start,
            length,
            force,
        } => {
            if !all && start.is_none() {
                bail!("erase needs --all, or both --start and --length");
            }
            let mut t = open_transport(&cli)?;
            flows::erase(t.as_mut(), all, start, length, force)
        }
        Cmd::Boot { stay } => {
            let mut t = open_transport(&cli)?;
            flows::boot(t.as_mut(), stay)
        }
        Cmd::Bless { image, base } => {
            let img = load_image(&image, base)?;
            println!(
                "loaded {}: base 0x{:08X}, {} bytes, crc32 0x{:08X}",
                image.display(),
                img.base,
                img.bytes.len(),
                img.crc32()
            );
            let mut t = open_transport(&cli)?;
            flows::bless(t.as_mut(), &img)
        }
    }
}

/// Exit codes: 0 ok, 1 device/IO/usage, 2 verify mismatch.
fn exit_code_for(e: &anyhow::Error) -> u8 {
    if e.downcast_ref::<VerifyMismatch>().is_some() {
        2
    } else {
        1
    }
}

fn main() -> ExitCode {
    // clap's default exit code for usage errors is 2, which this tool
    // reserves for verify mismatches — so parse manually and remap.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let ok = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
            let _ = e.print();
            return if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            };
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ERROR: {e:#}");
            ExitCode::from(exit_code_for(&e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_parser_radix() {
        assert_eq!(parse_int_auto("0x1209").unwrap(), 0x1209);
        assert_eq!(parse_int_auto("4096").unwrap(), 4096);
        assert_eq!(parse_int_auto("0o17").unwrap(), 0o17);
        assert_eq!(parse_int_auto("0b1010").unwrap(), 0b1010);
        assert!(parse_int_auto("nope").is_err());
        assert_eq!(parse_u16_auto("0x0001").unwrap(), 1);
        assert!(parse_u16_auto("0x1FFFF").is_err());
        assert_eq!(parse_u32_auto("0x70000").unwrap(), 0x70000);
        assert!(parse_u32_auto("0x100000000").is_err());
    }

    #[test]
    fn transport_resolution() {
        use TransportArg::*;
        // Defaults and implications.
        assert_eq!(resolve_transport_kind(None, false, false).unwrap(), Usb);
        assert_eq!(resolve_transport_kind(None, true, false).unwrap(), Uart);
        assert_eq!(resolve_transport_kind(None, false, true).unwrap(), Usb);
        assert_eq!(
            resolve_transport_kind(Some(Uart), true, false).unwrap(),
            Uart
        );
        assert_eq!(resolve_transport_kind(Some(Usb), false, true).unwrap(), Usb);
        // Conflicts.
        assert!(resolve_transport_kind(Some(Usb), true, false).is_err());
        assert!(resolve_transport_kind(Some(Uart), false, true).is_err());
        assert!(resolve_transport_kind(None, true, true).is_err());
    }

    #[test]
    fn cli_parses_subcommands_and_globals() {
        let cli = Cli::try_parse_from(["openboot"]).unwrap();
        assert!(cli.cmd.is_none()); // defaults to probe

        let cli = Cli::try_parse_from([
            "openboot",
            "flash",
            "fw.bin",
            "--force",
            "--no-boot",
            "--base",
            "0x1000",
        ])
        .unwrap();
        match cli.cmd {
            Some(Cmd::Flash {
                force,
                no_boot,
                no_verify,
                base,
                ..
            }) => {
                assert!(force && no_boot && !no_verify);
                assert_eq!(base, Some(0x1000));
            }
            _ => panic!("expected flash"),
        }

        // Global flags are accepted after the subcommand.
        let cli = Cli::try_parse_from(["openboot", "probe", "--port", "/dev/ttyUSB0"]).unwrap();
        assert_eq!(cli.port.as_deref(), Some("/dev/ttyUSB0"));

        // --start requires --length and vice versa; --all conflicts.
        assert!(Cli::try_parse_from(["openboot", "erase", "--start", "0x1000"]).is_err());
        assert!(Cli::try_parse_from([
            "openboot", "erase", "--all", "--start", "0x1000", "--length", "0x1000"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "openboot", "erase", "--start", "0x1000", "--length", "0x1000"
        ])
        .is_ok());
    }

    #[test]
    fn exit_codes() {
        let mismatch: anyhow::Error = VerifyMismatch {
            local_crc: 1,
            device_crc: Some(2),
        }
        .into();
        assert_eq!(exit_code_for(&mismatch), 2);
        // Context wrapping must not hide the mismatch.
        let wrapped = mismatch.context("while flashing");
        assert_eq!(exit_code_for(&wrapped), 2);

        assert_eq!(exit_code_for(&anyhow::anyhow!("io error")), 1);
        let dev: anyhow::Error = proto::DeviceError {
            status: 7,
            detail: None,
        }
        .into();
        assert_eq!(exit_code_for(&dev), 1);
    }
}
