# ShellCanvas Drive Bridge

An optional, free native app for attaching a folder from a ShellCanvas file
provider as a local drive or mount point. Local applications can then open and
save remote files through the operating system's filesystem interface.

**Development preview — not an end-user release yet.** Windows, Linux and macOS
builds pass in CI. Native Linux mounts pass live SFTP file-operation checks.
Native Windows mounts pass file-operation, memory-mapping and busy-detach tests
with WinFsp and a disposable local provider. The Windows executable also passes
the separate-process SFTP protocol test. A desktop-created Windows SFTP mapping,
the installation UI and macOS native runtime acceptance remain unverified.
Do not treat compilation as verified filesystem compatibility.

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
| Windows x64 | Separately installed [WinFsp](https://winfsp.dev/rel/); system administrator approval for the driver | Native WinFsp 2.1.25156: offset I/O above 4 GiB, truncate, replacement save, paged enumeration, capacity, shared/private/read-only mappings and busy detach pass with a disposable local provider |
| Linux | FUSE kernel interface and the distribution's mount helper | Native x86_64 mount: real SFTP offset I/O, truncate, replacement saves, directory paging, rename with an open file and capacity checks pass |
| macOS | Separately installed [macFUSE](https://macfuse.github.io/) | macOS 14 CI build passes; native mount verification pending |

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
shellcanvas-drive-bridge --help
shellcanvas-drive-bridge --licenses
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
  identity and database/VM-image compatibility are not claimed. Windows cleanup
  failures report structured warnings to ShellCanvas as well as stderr. Native
  failure injection verifies delivery of close/deletion warnings and preservation
  of the source after failed deletion. Full desktop lifecycle acceptance remains.
- Windows volume flush visits all writable descriptors, preserving the first
  failure while attempting the others. Confirmed renames update other open path
  references; failed renames preserve them. Descriptor drop closes remote handles
  even when opening failed after the remote handle was acquired. These paths have
  bookkeeping tests; they still need live WinFsp acceptance. WinFsp's normal Windows
  rename/share rules may refuse renames while child files are open.
- The FUSE backend uses direct I/O for ordinary reads/writes. On Linux kernels
  advertising `FUSE_DIRECT_IO_ALLOW_MMAP`, it also enables shared memory mapping.
  Native Linux 6.8 acceptance passed shared cross-page writes with `msync`, mapping
  lifetime after descriptor close, read-only mapping and private copy-on-write
  mapping; the underlying SFTP source was checked independently. Mapped stores
  reach the provider when pages are flushed, not on each CPU write. Coherence
  with concurrent remote edits and database/VM workloads is not promised.
  Kernels without that capability keep their existing direct-I/O limitations.
  Windows WinFsp acceptance also passes shared cross-page flush, mapping lifetime
  after descriptor close, read-only and private copy-on-write maps against a
  disposable local provider. macOS mapped-file acceptance remains pending. See the
  [Linux FUSE I/O contract](https://www.kernel.org/doc/html/latest/filesystems/fuse/fuse-io.html).
- Full desktop setup, failure recovery and modern macOS runtime acceptance remain
  release gates. This preview should not be used for valuable working files yet.

## Native Windows verification

The opt-in `tests/native_windows.rs` test launches the actual bridge, mounts an
unused drive letter, exercises ordinary Windows file APIs and inspects the backing
files independently. It verifies exact directory contents across multiple pages,
missing/denied/nonempty errors, capacity and memory mapping. An open file must
prevent detach; a second explicit request after closing it must remove the drive.
It uses a temporary local provider, no SSH credentials or normal app profile.

[CI run 34511425152](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34511425152)
passed this test at `30c4125` (27.93 seconds). CI installs the official WinFsp MSI
only on its disposable Windows runner after checking the pinned SHA-256 and
Authenticode signer. Ordinary `cargo test` skips this driver-dependent test.
To run explicitly on a Windows test machine with WinFsp already installed:

```powershell
$env:SHELLCANVAS_NATIVE_WINDOWS_TEST = '1'
cargo test --locked --test native_windows -- --ignored --nocapture
```

This is not Explorer/editor UI acceptance or an end-to-end desktop/SFTP test.

[CI run 34521265017](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34521265017)
at `9c4fbe1` verifies volume-wide flush through a native Windows volume handle.
Three writable files are attempted even when one provider flush fails, with
ERROR_IO_DEVICE returned to Windows. After clearing the injected fault, the next
flush succeeds and backing bytes are independently verified for every file.
The full native suite passes in 28.88 seconds. This tests propagation of provider
durability results; physical power-loss behavior remains the storage provider's
responsibility.

[CI run 34518237898](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34518237898)
at `e58d386` passes directory rename checks with open handles. Windows rejects
renaming a directory containing an open child file with error 5; closing the
child permits rename while two directory handles remain open. Both handles
continue returning metadata after rename. A failed replacement of a nonempty
directory preserves both trees. These results match a separate NTFS baseline and
[Microsoft's FileRenameInformation rules](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/87f86c9b-6c2a-4803-84b7-131a74a434fa).
Linux/POSIX open-child rename behavior is different. All earlier native Windows
checks also pass in that run (32.21 seconds).

[CI run 34512537189](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34512537189)
at `36464c5` also passes write-through failure checks: I/O, offline, timed-out and
read-only writes return their corresponding Windows errors without changing the
backing bytes. Failed flush returns an error; unrelated I/O remains usable.
Failed close and cleanup-time deletion reach the parent as warnings, and failed
deletion preserves the source file. These are injected provider errors, not a
real network outage or exhausted remote disk.

[CI run 34513391440](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34513391440)
at `1459801` additionally cuts both inherited pipe endpoints with a Windows file
still open. The next write fails within a ten-second deadline, confirmed backing
bytes remain intact, the bridge exits unsuccessfully and the drive letter is
removed. Unexpected lifecycle replies or lost connections now produce a failure
exit on Windows and FUSE. Normal explicit detach still exits successfully.
This exercises real bridge transport loss; SSH network interruption is separate.

Normal FUSE detach uses the ordinary system unmount helper and preserves a busy
mount. Abnormal process/session teardown also involves `fuser`'s own cleanup;
its fallback can use lazy/forced unmount depending on the platform. Linux 6.8
acceptance as root at `1459801` cuts the actual pipes with a local file open over
the core SFTP provider: write and close return ENOTCONN, confirmed source bytes
remain unchanged, the helper exits unsuccessfully and the native mount table
shows removal. The fixture and staged binary were then verified removed.
Modern macOS failure cleanup remains unverified. Non-root Linux results follow.
ShellCanvas retains failed mappings for
inspection and verifies the OS mount table before releasing cleanup ownership.

At `87a28c9`, directory rewind recovery clears retired remote handles and buffered
entries before reopening, including failures followed by a seek to a later
position. Linux/macOS regression tests cover close/reopen failures and invalid
replies; all platform jobs pass in [CI run 34519068114](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34519068114).
The Linux artifact also passes native Linux 6.8 testing through the core SFTP
provider: repeated rewinds of the same directory descriptor return exact paged
contents, including after renaming the open directory. The earlier I/O, mmap and
busy-detach checks pass, with independently confirmed fixture/mount cleanup.
This does not establish native injected-reopen-failure acceptance.

The same `87a28c9` Linux artifact also passes as an unprivileged mounting user
(UID 65534) on Linux 6.8 using the installed setuid `fusermount3` helper. Local
file operations, rewinds/renames, memory mapping, busy-detach protection and
ordinary unmount pass. Source bytes are independently checked by the source
owner. Cutting both bridge pipes with a file open returns ENOTCONN for write and
close, preserves confirmed source bytes, produces a failure exit and removes the
mount without privileged recovery. Both disposable fixture trees, mounts and
the staged binary were independently confirmed removed. No system FUSE settings
or accounts were changed for the test.

The core's `native_bridge_probe` has an opt-in
`SHELLCANVAS_PROBE_UNPRIVILEGED=1` mode for this existing-account test; combine it
with `SHELLCANVAS_PROBE_TRANSPORT_LOSS=1` for pipe-loss acceptance. This result is
specific to that Linux/runtime combination, not a guarantee for every distribution.

Inode lifetime regressions pass at `558d1f2` in
[CI run 34521983619](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34521983619).
They cover both orders of releasing kernel references and open handles, and
ensure retiring an old inode cannot remove a recreated path. Its Linux artifact
also passes native Linux 6.8 acceptance as UID 65534 through the core SFTP
provider: unlink/recreate gives a distinct inode, the old open descriptor keeps
reading/writing its original object, and closing it leaves the replacement
unchanged. The source owner independently verifies replacement bytes. Earlier
directory rewind, mmap, busy detach and ordinary unmount checks pass; fixture,
mount and staged binary removal were independently confirmed. All platform CI
jobs and native Windows checks pass. Concurrent edits by another remote client
remain outside this evidence.

### Windows read-only attributes

Existing regular files expose read-only when their Unix permission bits contain
no write permission. Setting read-only through `SetFileAttributes` removes all
write bits; clearing it restores owner write only, preserving read/execute bits
and never adding group/other write. This is a permission projection, not a full
Windows ACL or effective-access calculation. The remote account still determines
which operations are allowed. Use a matching updated core: attribute-only opens
use a remote read handle, and non-size metadata changes require a writable root.
Read handles cannot truncate and read-only roots cannot change metadata.

`SetBasicInfo` rejects hidden/system/archive and other unsupported DOS flags
before changing timestamps or permissions. Directory read-only changes, absent
permissions and transitions involving special Unix mode bits also report
unsupported instead of silently changing unrelated permissions. Creation and
overwrite attribute requests and creation/change-time semantics remain review
items; this implementation does not claim full DOS-attribute persistence.

The mapping and rejection regressions pass at `447b1c3`. Native WinFsp acceptance
in [CI run 34523633220](https://github.com/techartdev/ShellCanvas-DriveBridge/actions/runs/34523633220)
checks read-only set/clear against backing metadata, rejected writes/deletion,
and rejection of a combined hidden/read-only request without partial changes.
The core's live SFTP probe separately verifies metadata changes through a read
handle, rejected truncation and read-only-root enforcement. These are separate
provider and bridge checks; desktop-created Windows SFTP acceptance remains open.

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
