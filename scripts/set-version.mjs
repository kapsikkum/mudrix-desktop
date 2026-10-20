// node scripts/set-version.mjs 26.10.3[-next.1]
// Writes the version to package.json and src-tauri/Cargo.toml (the lock file entry is
// refreshed by the next cargo build), then re-checks. It does not commit or tag.
import { readFileSync, writeFileSync } from 'node:fs';

const version = process.argv[2]?.replace(/^v/, '');
if (!version || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error('usage: node scripts/set-version.mjs X.Y.Z[-suffix]');
  process.exit(1);
}

const here = new URL('..', import.meta.url);

const pkgPath = new URL('package.json', here);
const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
pkg.version = version;
writeFileSync(pkgPath, `${JSON.stringify(pkg, null, 2)}\n`);

const cargoPath = new URL('src-tauri/Cargo.toml', here);
const cargo = readFileSync(cargoPath, 'utf8').replace(
  /^(\[package\][^[]*?^version\s*=\s*")[^"]+(")/ms,
  `$1${version}$2`
);
writeFileSync(cargoPath, cargo);

const lockPath = new URL('src-tauri/Cargo.lock', here);
try {
  const lock = readFileSync(lockPath, 'utf8').replace(
    /(name = "mudrix-desktop"\r?\nversion = ")[^"]+(")/,
    `$1${version}$2`
  );
  writeFileSync(lockPath, lock);
} catch {
  // no lock file yet
}

await import('./check-version.mjs');
