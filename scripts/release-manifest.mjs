// SPDX-License-Identifier: GPL-3.0-only
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
const [directory, tag] = process.argv.slice(2);
if (!directory || !/^v\d+\.\d+\.\d+$/.test(tag ?? '')) throw new Error('Usage: release-manifest.mjs ARTIFACT_DIRECTORY vX.Y.Z');
const version = tag.slice(1);
const wire = await readFile(new URL('../vendor/filesystem-sdk/src/wire.rs', import.meta.url), 'utf8');
const protocol = Number(wire.match(/pub const PROTOCOL: u32 = (\d+);/)?.[1]);
if (!Number.isSafeInteger(protocol) || protocol < 1) throw new Error('Cannot identify bridge SDK protocol');
const platforms = {};
for (const target of ['windows-x86_64', 'linux-x86_64', 'macos-x86_64', 'macos-aarch64']) {
  const name = `shellcanvas-drive-bridge-${version}-${target}${target.startsWith('windows-') ? '.exe' : ''}`;
  const bytes = await readFile(path.join(directory, name));
  const reported = (await readFile(path.join(directory, `${target}.version`), 'utf8')).trim();
  if (reported !== `ShellCanvas Drive Bridge ${version} (protocol ${protocol})`) throw new Error(`Version/protocol mismatch: ${target}`);
  if (!bytes.length || bytes.length > 128 * 1024 * 1024) throw new Error(`Invalid size: ${name}`);
  platforms[target] = { sha256: createHash('sha256').update(bytes).digest('hex'), size: bytes.length };
}
await writeFile(path.join(directory, 'bridge-release.json'), JSON.stringify({ schema: 1, protocol, version, platforms }, null, 2) + '\n');
const lines = [];
for (const name of (await readdir(directory)).sort()) {
  if (name === 'SHA256SUMS.txt') continue;
  const bytes = await readFile(path.join(directory, name));
  lines.push(`${createHash('sha256').update(bytes).digest('hex')}  ${name}`);
}
await writeFile(path.join(directory, 'SHA256SUMS.txt'), lines.join('\n') + '\n');
