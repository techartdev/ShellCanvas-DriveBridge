Drive Bridge packages for Windows x64, Linux x64, and macOS Intel/Apple silicon.

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
