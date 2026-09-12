import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

const destination = process.argv[2];
if (!destination) throw new Error('Usage: node scripts/dependency-notices.mjs OUTPUT');
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--format-version', '1'], { encoding: 'utf8' }));
const packages = metadata.packages.sort((a, b) => a.name.localeCompare(b.name));
const sections = ['ShellCanvas Drive Bridge dependency license texts\nGenerated from the locked Cargo package sources. Drivers are distributed separately.\n'];
for (const pkg of packages) {
  let root = dirname(pkg.manifest_path);
  // These import-library packages use the winapi workspace license texts.
  if (pkg.name.startsWith('winapi-') && pkg.name.endsWith('-gnu')) {
    root = dirname(packages.find(p => p.name === 'winapi').manifest_path);
  }
  let files = readdirSync(root, { withFileTypes: true }).filter(p => p.isFile() && /^(LICENSE|COPYING|NOTICE)([.-]|$)/i.test(p.name)).map(p => join(root, p.name));
  // Published winfsp-rs crates declare GPL-3.0 without shipping the text.
  // Include the complete GPLv3 text shipped by this GPL-3.0-only project.
  if (!files.length && ['winfsp', 'winfsp-sys'].includes(pkg.name)) files = ['LICENSE'];
  if (!files.length) throw new Error(`Missing license text for ${pkg.name} ${pkg.version}`);
  sections.push(`\n===== ${pkg.name} ${pkg.version} =====\nLicense: ${pkg.license ?? 'see text'}\nAuthors: ${(pkg.authors ?? []).join(', ')}\nRepository: ${pkg.repository ?? ''}\n${files.map(p => readFileSync(p, 'utf8')).join('\n')}`);
}
writeFileSync(destination, sections.join('\n'));
