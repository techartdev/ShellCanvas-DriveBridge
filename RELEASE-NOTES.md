Drive Bridge 0.1.1 replaces 0.1.0, which was accidentally built from a branch
without the protocol-2 lifecycle changes required by the current desktop.
Use 0.1.1 with current ShellCanvas builds; do not install 0.1.0.

Includes graceful busy detach, passive Explorer folder-handle handling,
remembered drive-letter reservation, native file semantics and lifecycle tests.
The signed manifest and version output derive their protocol from the SDK.

Known Windows SFTP limitation: a second reader while a writable remote handle is
open, and native rename in the tested workflow, can fail with an I/O error.
Close-after-save followed by open/read and busy/ordinary detach passed through
the current desktop SDK; rejected rename preserved the source. Broader Windows
SFTP sharing and rename compatibility remains follow-up work.

Packages for Windows x64, Linux x64, and macOS Intel/Apple silicon.

Install through ShellCanvas Settings → Files → Drive Bridge. ShellCanvas selects
the local client's package and verifies its signed manifest and executable hash.
The executable remains available here for advanced/offline installation.

The driver is separate: WinFsp on Windows, FUSE on Linux, and macFUSE on macOS.
No SSH credentials are supplied to the bridge. Windows and Linux native tests
have passed within the scope documented in the README. macOS runtime acceptance
remains pending; compiled packages do not imply every filesystem workload works.

Drive Bridge is GPL-3.0-only; LICENSE and THIRD-PARTY.md accompany these binaries.
Complete corresponding source is the tagged repository, including the vendored
filesystem SDK and Cargo.lock (GitHub's source archives are below).
