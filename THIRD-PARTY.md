# Dependency notices

This source repository does not ship filesystem driver installers or binaries.

- `winfsp` 0.13.1 / `winfsp-sys` 0.12.1: GPL-3.0, project at
  https://github.com/SnowflakePowered/winfsp-rs.
- WinFsp native runtime: Copyright Bill Zissimopoulos / Navimatics;
  https://winfsp.dev/com/ and https://github.com/winfsp/winfsp.
- `fuser` 0.18: MIT, https://github.com/cberner/fuser.
- macFUSE runtime: separate license, including commercial binary-bundling
  restrictions: https://raw.githubusercontent.com/macfuse/macfuse/release/macfuse/LICENSE.txt.
- `vendor/filesystem-sdk`: ShellCanvas filesystem SDK, MPL-2.0; complete license
  included in that directory. This is the only ShellCanvas core source included.
- The other Rust dependency versions and checksums are recorded in `Cargo.lock`.
  Cargo package metadata retains their individual license declarations.

Before producing binary releases, include the required license notices for the
resolved dependency graph and the corresponding GPL source/build materials.
