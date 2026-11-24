//! Host library for the OpenBoot IAP bootloader (OBP v0.2) on WCH
//! CH570/CH572/CH591/CH592, over USB HID, UART, or a QMK keyboard's HID
//! tunnel. The `openboot` CLI is a thin front end over this crate.
//!
//! The layers, bottom up:
//!
//! - [`transport`] — one request frame out, the matching response back, over
//!   HID, a serial port, or the tunnel.
//! - [`client`] — one method per OBP command, each owning its encoding,
//!   timeout and retry rule.
//! - [`flows`] — probe, flash, verify, erase, boot, bless. They print
//!   nothing: each reports its steps to an [`flows::Observer`] and returns
//!   what it found.
//! - [`console`] — the CLI's rendering of those events as text.
//!
//! [`image`], [`hex`] and [`bundle`] load what gets flashed; [`proto`] holds
//! the wire format.
//!
//! Build without default features to leave out the CLI's argument parser:
//!
//! ```toml
//! openboot = { git = "...", default-features = false }
//! ```

pub mod bundle;
pub mod client;
pub mod console;
pub mod flows;
pub mod hex;
pub mod image;
pub mod proto;
pub mod transport;

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod testutil;
