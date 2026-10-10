#!/usr/bin/env node
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { validateReleaseMetadata } from './release-metadata.mjs';

const root = resolve(import.meta.dirname, '..');
const args = process.argv.slice(2);
const release = args.includes('--release');
const unknownArgs = args.filter((arg) => arg !== '--release');
if (unknownArgs.length > 0) {
  console.error(`Unknown argument(s): ${unknownArgs.join(', ')}`);
  process.exit(2);
}

const [packageText, cargoToml, tauriText] = await Promise.all([
  readFile(resolve(root, 'package.json'), 'utf8'),
  readFile(resolve(root, 'src-tauri/Cargo.toml'), 'utf8'),
  readFile(resolve(root, 'src-tauri/tauri.conf.json'), 'utf8'),
]);

let packageJson;
let tauriConfig;
try {
  packageJson = JSON.parse(packageText);
  tauriConfig = JSON.parse(tauriText);
} catch (error) {
  console.error(`Unable to parse release metadata: ${error.message}`);
  process.exit(2);
}

const { errors, versions } = validateReleaseMetadata({ packageJson, cargoToml, tauriConfig, release });
for (const [source, version] of Object.entries(versions)) console.log(`${source}: ${version ?? 'missing'}`);
if (errors.length > 0) {
  for (const error of errors) console.error(`ERROR: ${error}`);
  process.exit(1);
}

console.log(release ? 'Stable release metadata is consistent.' : 'Release metadata is consistent.');
