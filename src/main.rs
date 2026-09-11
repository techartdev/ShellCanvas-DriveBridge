// SPDX-License-Identifier: GPL-3.0-only
use shellcanvas_filesystem_sdk::{
    bridge_control::{BridgeDirective, BridgeEvent},
    wire::{Client, Operation, Value},
    *,
};
use std::{
    io::{Stdin, Stdout},
    sync::Arc,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuse;
#[cfg(any(windows, test))]
mod mount_gate;
mod probe;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_drives;
#[cfg(any(windows, test))]
mod windows_handles;
#[cfg(any(windows, test))]
mod windows_attributes;
#[cfg(any(windows, test))]
mod windows_times;
type Pipe = Client<Stdin, Stdout>;
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() == 2 && args[1] == "--version" {
        println!("ShellCanvas Drive Bridge {} (protocol 1)", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.len() == 2 && args[1] == "--licenses" {
        println!(
            "ShellCanvas Drive Bridge: GPL-3.0-only. Free software; commercial use is permitted under its license.\n\n{}",
            include_str!("../THIRD-PARTY.md")
        );
        return Ok(());
    }
    if args.len() == 2 && (args[1] == "--help" || args[1] == "-h") {
        println!(
            "ShellCanvas Drive Bridge (development preview)\n\nAttach a selected ShellCanvas folder through your operating system's filesystem interface.\n\nLaunch mappings from ShellCanvas. Driver installation is separate:\n  Windows: WinFsp\n  Linux: FUSE and the distribution's mount helper\n  macOS: macFUSE\n\n--licenses  Show licensing and third-party notices\n--mount LOCAL_TARGET  Use a root grant supplied over inherited pipes\n\nSource and installation status: https://github.com/techartdev/ShellCanvas-DriveBridge"
        );
        return Ok(());
    }
    if args.len() == 2 && args[1] == "--verify-transport" {
        return probe::run(Client::new(std::io::stdin(), std::io::stdout()));
    }
    if args.len() != 3 || args[1] != "--mount" {
        anyhow::bail!(
            "Launch this bridge from ShellCanvas. Usage: shellcanvas-drive-bridge --mount LOCAL_TARGET"
        );
    }
    let pipe = Arc::new(Client::new(std::io::stdin(), std::io::stdout()));
    let capabilities = match pipe.call(Operation::Capabilities)? {
        Value::Capabilities(caps) => caps,
        _ => anyhow::bail!("Invalid capability reply"),
    };
    #[cfg(windows)]
    {
        windows::run(pipe, capabilities, &args[2])
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        fuse::run(pipe, capabilities, &args[2])
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (pipe, capabilities);
        anyhow::bail!("Unsupported client OS")
    }
}
