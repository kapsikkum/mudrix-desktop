// node scripts/check-version.mjs [tag]     e.g. v26.10.3 or v26.10.3-next.1
// package.json is the single source of truth (tauri.conf.json reads it through
// "version": "../package.json"); Cargo.toml must agree, and so must the release tag.
import { readFileSync } from 'node:fs';

const here = new URL('..', import.meta.url);
const read = (p) => readFileSync(new URL(p, here), 'utf8');

const pkg = JSON.parse(read('package.json')).version;
const cargo = /^\[package\][^[]*?^version\s*=\s*"([^"]+)"/ms.exec(read('src-tauri/Cargo.toml'))?.[1];
const tag = process.argv[2]?.replace(/^v/, '');

const versions = { 'package.json': pkg, 'src-tauri/Cargo.toml': cargo, ...(tag ? { tag } : {}) };
console.log(versions);
if (new Set(Object.values(versions)).size !== 1 || Object.values(versions).includes(undefined)) {
  console.error('version mismatch');
  process.exit(1);
}
