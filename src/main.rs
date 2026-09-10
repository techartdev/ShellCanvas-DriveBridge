// SPDX-License-Identifier: GPL-3.0-only
use shellcanvas_filesystem_sdk::{
    wire::{Client, Operation, Value},
    *,
};
use std::{
    io::{Stdin, Stdout},
    sync::Arc,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuse;
mod probe;
#[cfg(windows)]
mod windows;
type Pipe = Client<Stdin, Stdout>;
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
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
