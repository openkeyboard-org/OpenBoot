//! openboot — host tool for the OpenBoot IAP bootloader (OBP v0.2) on WCH
//! CH570/CH572/CH591/CH592, over USB HID or UART.
//!
//! Safe by default: `probe` (the default subcommand) is read-only, and
//! `flash`/`erase` print a plan and stop unless `--force` is given.

mod bundle;
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

use anyhow::{bail, Context, Result};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand, ValueEnum};

use bundle::{load_source, Bundle};
use flows::{FlashOpts, VerifyMismatch};
use image::load_image;
use proto::consts::OB_UART_BAUD;
use transport::hid::{HidTransport, DEFAULT_PID, DEFAULT_VID};
use transport::hidsel::UsageFilter;
use transport::qmk::{QmkOpts, QmkTransport, TUNNEL_USAGE, TUNNEL_USAGE_PAGE};
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
    /// The bootloader's UART reached through a QMK keyboard's HID tunnel.
    Qmk,
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
        openboot boot --stay                  # reset, stay in bootloader\n  \
        openboot --transport qmk --vid 0x4D4B --pid 0x0002 flash fw.obb --force\n                                        \
        # update a module through its keyboard's HID tunnel"
)]
struct Cli {
    /// Transport (default usb; --port implies uart, --serial implies usb; qmk
    /// is never implied and must be asked for)
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

    /// HID usage page of the QMK tunnel interface (--transport qmk only)
    #[arg(long, global = true, value_parser = parse_u16_auto, value_name = "PAGE")]
    usage_page: Option<u16>,

    /// HID usage of the QMK tunnel interface (--transport qmk only)
    #[arg(long, global = true, value_parser = parse_u16_auto, value_name = "USAGE")]
    usage: Option<u16>,

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

    /// Build or inspect a slot bundle: one release file carrying every
    /// per-slot build of an application
    #[command(subcommand)]
    Bundle(BundleCmd),

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

#[derive(Subcommand)]
enum BundleCmd {
    /// Pack per-slot images into one bundle
    #[command(long_about = "Pack per-slot images into one bundle.\n\n\
        Each IMAGE is PATH@BASE, where BASE is the address that image was \
        LINKED for — .bin files carry no base of their own, and guessing one \
        is exactly the mistake bundles exist to prevent. Intel HEX takes its \
        base from its records, so PATH alone is used and @BASE is rejected.\n\n\
        Example:\n  \
        openboot bundle create -o app.obb --chip ch592 \\\n    \
        app-slot-a.bin@0x2000 app-slot-b.bin@0x39000")]
    Create {
        /// PATH@BASE per slot (@BASE required for .bin, rejected for .hex)
        #[arg(required = true, value_name = "IMAGE")]
        images: Vec<String>,

        /// Output bundle
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,

        /// Chip family to record, cross-checked against HELLO when flashing
        #[arg(long, value_name = "CHIP")]
        chip: Option<String>,
    },

    /// Show what a bundle contains
    Info {
        /// Bundle file
        bundle: PathBuf,
    },
}

/// `PATH@BASE`, split on the LAST '@' so a path may itself contain one —
/// provided a base follows. The format cannot tell `my@file.bin` (a path)
/// from a path with a malformed base, so that case is an error rather than a
/// bare path, and says so.
fn parse_image_spec(spec: &str) -> Result<(PathBuf, Option<u32>)> {
    let Some((path, base)) = spec.rsplit_once('@') else {
        return Ok((PathBuf::from(spec), None));
    };
    if path.is_empty() {
        bail!("{spec:?}: nothing before the '@' — the form is PATH@BASE");
    }
    if base.is_empty() {
        bail!("{spec:?}: nothing after the '@' — the form is PATH@BASE");
    }
    let addr = parse_u32(base).map_err(|_| {
        anyhow::anyhow!(
            "{spec:?}: {base:?} is not an address. The form is PATH@BASE, and a \
             path containing '@' still has to be followed by its base."
        )
    })?;
    Ok((PathBuf::from(path), Some(addr)))
}

fn parse_u32(s: &str) -> Result<u32> {
    let t = s.trim();
    let parsed = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
    } else {
        t.parse::<u32>()
    };
    parsed.map_err(|_| anyhow::anyhow!("{s:?} is not a number"))
}

fn run_bundle(cmd: BundleCmd) -> Result<()> {
    match cmd {
        BundleCmd::Create {
            images,
            output,
            chip,
        } => {
            let family = match chip.as_deref() {
                None => bundle::FAMILY_UNSPECIFIED,
                Some(name) => bundle::family_from_name(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown chip {name:?}; expected one of {}",
                        bundle::FAMILY_NAMES
                            .iter()
                            .map(|(n, _)| *n)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?,
            };
            let mut loaded = Vec::new();
            for spec in &images {
                let (path, base) = parse_image_spec(spec)?;
                let is_hex = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("hex"));
                if base.is_none() && !is_hex {
                    bail!(
                        "{}: a .bin carries no link base, so give it as PATH@BASE — \
                         the base it was LINKED for, not where you would like it",
                        path.display()
                    );
                }
                // Rejected here rather than left to load_image, whose message
                // names --base: the user typed @BASE and would be told about a
                // flag they never used.
                if base.is_some() && is_hex {
                    bail!(
                        "{}: drop the @BASE — Intel HEX carries its own load \
                         address in its records",
                        path.display()
                    );
                }
                loaded.push(load_image(&path, base)?);
            }
            let b = Bundle::new(family, loaded)?;
            let bytes = b.encode();
            std::fs::write(&output, &bytes)
                .with_context(|| format!("write {}", output.display()))?;
            println!("wrote {} ({} bytes)", output.display(), bytes.len());
            print_bundle(&b);
            Ok(())
        }
        BundleCmd::Info { bundle: path } => {
            let raw = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let b = Bundle::parse(&raw).with_context(|| format!("parse {}", path.display()))?;
            println!("{}: {} bytes", path.display(), raw.len());
            print_bundle(&b);
            Ok(())
        }
    }
}

fn print_bundle(b: &Bundle) {
    println!("  chip            {}", b.family_name());
    println!("  variants        {}", b.images.len());
    for img in &b.images {
        println!(
            "    base 0x{:08X}  {:>7} B  crc32 0x{:08X}",
            img.base,
            img.bytes.len(),
            img.crc32()
        );
    }
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
        // --serial IS allowed with qmk: there it picks which keyboard, the same
        // device-versus-interface split --serial has on the USB transport.
        (Some(TransportArg::Qmk), true, _) => {
            bail!("--port selects the UART transport and conflicts with --transport qmk")
        }
        (Some(kind), _, _) => Ok(kind),
        (None, true, true) => bail!("--port (uart) and --serial (usb) conflict; pick one"),
        (None, true, false) => Ok(TransportArg::Uart),
        (None, false, _) => Ok(TransportArg::Usb),
    }
}

fn open_transport(cli: &Cli) -> Result<Box<dyn Transport>> {
    let kind = resolve_transport_kind(cli.transport, cli.port.is_some(), cli.serial.is_some())?;
    if kind != TransportArg::Qmk && (cli.usage_page.is_some() || cli.usage.is_some()) {
        // The bootloader's own usage is normative and must stay fixed: pointing
        // that selector at another interface is how you write flash frames into
        // a keyboard.
        bail!("--usage-page/--usage apply only to --transport qmk");
    }
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
        TransportArg::Qmk => {
            // No VID/PID default: hid.rs's is the BOOTLOADER's identity, while
            // the tunnel carries the keyboard's. Guessing one product's here
            // would bake it into a product-neutral tool.
            let (Some(vid), Some(pid)) = (cli.vid, cli.pid) else {
                bail!(
                    "--transport qmk requires --vid and --pid — the KEYBOARD's \
                     USB identity, not the bootloader's, so there is no default"
                );
            };
            let filter = UsageFilter {
                page: cli.usage_page.unwrap_or(TUNNEL_USAGE_PAGE),
                usage: cli.usage.unwrap_or(TUNNEL_USAGE),
            };
            eprintln!(
                "entering the bootloader through the QMK tunnel — the keyboard \
                 goes off air until this finishes"
            );
            let t =
                QmkTransport::open(vid, pid, cli.serial.as_deref(), filter, &QmkOpts::default())?;
            eprintln!(
                "opened QMK tunnel {} (VID=0x{vid:04X} PID=0x{pid:04X}); the \
                 bootloader answered {:.1} s after the module reset",
                t.path,
                t.settled.as_secs_f64()
            );
            Ok(Box::new(t))
        }
    }
}

/// Warn where both facts are visible: parking the module in the bootloader is
/// legitimate, but over the tunnel it means the keyboard stays off air, and the
/// ten second auto-boot that would normally rescue it has already been disabled
/// by the HELLO the transport had to send to get here.
fn warn_if_parking_over_the_tunnel(cli: &Cli) {
    if cli.transport != Some(TransportArg::Qmk) {
        return;
    }
    eprintln!(
        "WARNING: this leaves the module in the bootloader. A successful HELLO \
         has already disabled the ten second idle auto-boot, so the keyboard \
         stays off air until you run `openboot --transport qmk --vid ... \
         --pid ... boot` or power-cycle it."
    );
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
            let src = load_source(&image, base)?;
            println!("loaded {}: {}", image.display(), src.describe());
            if no_boot {
                warn_if_parking_over_the_tunnel(&cli);
            }
            let mut t = open_transport(&cli)?;
            flows::flash(
                t.as_mut(),
                &src,
                &FlashOpts {
                    force,
                    verify: !no_verify,
                    boot: !no_boot,
                },
            )
        }
        Cmd::Verify { image, base } => {
            let src = load_source(&image, base)?;
            let mut t = open_transport(&cli)?;
            flows::verify(t.as_mut(), &src)
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
            if stay {
                warn_if_parking_over_the_tunnel(&cli);
            }
            let mut t = open_transport(&cli)?;
            flows::boot(t.as_mut(), stay)
        }
        Cmd::Bundle(cmd) => run_bundle(cmd),

        Cmd::Bless { image, base } => {
            let src = load_source(&image, base)?;
            println!("loaded {}: {}", image.display(), src.describe());
            let mut t = open_transport(&cli)?;
            flows::bless(t.as_mut(), &src)
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

        // The tunnel is never implied — nothing selects it but asking for it.
        assert_eq!(
            resolve_transport_kind(Some(Qmk), false, false).unwrap(),
            Qmk
        );
        // --serial picks WHICH keyboard, so it composes rather than conflicts.
        assert_eq!(resolve_transport_kind(Some(Qmk), false, true).unwrap(), Qmk);
        // A serial port is a different link entirely.
        assert!(resolve_transport_kind(Some(Qmk), true, false).is_err());
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
