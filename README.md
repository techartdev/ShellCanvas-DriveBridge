# ShellCanvas Drive Bridge

An optional, free native app for attaching a folder from a ShellCanvas file
provider as a local drive or mount point. Local applications can then open and
save remote files through the operating system's filesystem interface.

**Development preview — not an end-user release yet.** The Windows backend builds,
the Linux FUSE backend passes a cross-target compile check, and the separate
process protocol has passed a live SFTP read/write test. Native mounted-drive
acceptance, macOS builds, the ShellCanvas attachment UI, installation and graceful
detach integration are still in progress. Do not treat compilation as verified
filesystem compatibility.

## Architecture

Local application → WinFsp/FUSE → Drive Bridge → inherited pipes → ShellCanvas
core → the selected Files provider.

The main ShellCanvas app does not link or bundle filesystem drivers. It retains
host authentication and grants one selected remote root to the bridge. The bridge
does not receive SSH credentials, host profiles or a network API for other hosts.
Switching the visible workspace must never change an existing mapping's source.

The small MPL-2.0 filesystem SDK is vendored under `vendor/filesystem-sdk` so this
repository builds independently. It defines optional metadata, file handles,
offset reads/writes, directory pages, rename, flush and a bounded versioned pipe
protocol. Simple ShellCanvas adapters need not implement mounting.

## Platforms and prerequisites

| Client | Native integration | Current verification |
| --- | --- | --- |
| Windows x64 | Separately installed [WinFsp](https://winfsp.dev/rel/); system administrator approval for the driver | Executable builds; native mount test pending |
| Linux | FUSE kernel interface and the distribution's mount helper | x86_64 compile check passes; native mount test pending |
| macOS | Separately installed [macFUSE](https://macfuse.github.io/) | Backend shares the FUSE implementation; macOS build and runtime verification pending |

Modern supported systems are the target. The current macFUSE release requires
macOS 12+. This Rust FUSE backend uses the kernel/libfuse interface; macFUSE's
new FSKit backend is **not yet verified** with it. Do not assume FSKit removes
kernel-extension setup for this app. Old-system workarounds are not a priority.

The remote host does not need WinFsp or FUSE. An existing SFTP-capable Linux,
macOS or Windows host can supply files. Permissions and available SFTP extensions
still determine what the remote account can do.

## Build

Install current Rust. On Windows, install a C/C++ toolchain with the Windows SDK
and libclang; set `LIBCLANG_PATH` if bindgen cannot find it. Build headers come
from the Rust dependency; the driver runtime is installed separately. On macOS,
install macFUSE development libraries and `pkg-config` first.

```sh
cargo build --locked
cargo check --locked --target x86_64-unknown-linux-gnu
```

The second command is an optional cross-target check after `rustup target add
x86_64-unknown-linux-gnu`. It does not prove a native mount works.

The executable is launched by ShellCanvas with inherited request/reply pipes:

```text
shellcanvas-drive-bridge --mount LOCAL_TARGET
```

It is not a standalone SSH client. Launching that command from an ordinary
terminal without the host protocol will not establish a connection. The internal
`--verify-transport` mode is for the explicit disposable-root integration harness;
it creates and removes test files and must not receive a user's working folder.

## Current semantics and limitations

- Read/write requests use bounded chunks and real offsets; the whole remote tree
  is not downloaded or indexed before use. Directory pages are consumed lazily.
- Writes are acknowledged by the provider before success. SFTP servers with the
  OpenSSH fsync extension can provide durable flush; other servers only acknowledge
  writes. Atomic replacement requires the server's corresponding extension.
- No offline write-back or automatic replay of failed mutations. A timeout can
  mean a remote operation completed: check the remote state before retrying.
- Links and special files are currently excluded by the SFTP projection. Portable
  component validation rejects names that would inject separators or alternate
  namespaces. Root selection is not a server-side security sandbox: SFTP v3 does
  not provide race-free `openat`/`nofollow` guarantees against concurrent remote
  path replacement.
- Windows ACLs, alternate data streams, distributed locks, remote hard-link
  identity and database/VM-image compatibility are not claimed. Windows deletion
  failure during cleanup currently reports to stderr; visible failure handling
  and lifecycle acceptance are still required before release.
- The FUSE backend uses direct I/O to avoid silently serving a stale file-content
  cache. Memory-mapped application workflows need explicit acceptance testing.
- Driver setup, live filesystem acceptance and graceful busy detach remain release
  gates. This preview should not be used for valuable working files yet.

## Licensing and commercial distribution

Drive Bridge is **GPL-3.0-only**, reflecting its use of the GPL-licensed
[winfsp-rs bindings](https://github.com/SnowflakePowered/winfsp-rs). It is free
software; GPL does not prohibit commercial use. Distribution and modifications
must follow the license. This does not change the main ShellCanvas repository's
MPL-2.0 license. The vendored SDK retains its own MPL-2.0 license and notices.

[WinFsp](https://winfsp.dev/com/) has GPLv3 terms with a FLOSS exception and a
separate commercial license. The Rust wrapper has its own GPL terms; do not assume
the WinFsp exception grants an exception for the wrapper.

[macFUSE's license](https://raw.githubusercontent.com/macfuse/macfuse/release/macfuse/LICENSE.txt)
restricts bundling its binaries with commercial software, including automated
download/installation in that context, without prior permission. This app does
not bundle or automatically install macFUSE. Check the selected runtime's terms
before changing distribution. Linux's FUSE integration and the MIT-licensed
[fuser library](https://github.com/cberner/fuser) are separate components.

See `LICENSE`, `THIRD-PARTY.md` and the vendored SDK license. Dependency runtime
licenses are not blanket restrictions on what users may do with their files.
