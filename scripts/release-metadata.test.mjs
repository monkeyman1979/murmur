import test from 'node:test';
import assert from 'node:assert/strict';
import { parseCargoPackageVersion, validateReleaseMetadata } from './release-metadata.mjs';

const cargo = (version) => `[package]\nname = "murmur"\nversion = "${version}"\nedition = "2021"\n\n[dependencies]\nserde = "1"\n`;
const metadata = (version = '0.1.0-pre.1') => ({
  packageJson: { version },
  cargoToml: cargo(version),
  tauriConfig: { version },
});

test('reads the package version from Cargo.toml, not dependency versions', () => {
  assert.equal(parseCargoPackageVersion(cargo('0.1.0-pre.1')), '0.1.0-pre.1');
  assert.equal(parseCargoPackageVersion('[dependencies]\nserde = "1"\n'), null);
});

test('accepts matching prerelease metadata in development mode', () => {
  assert.deepEqual(validateReleaseMetadata(metadata()).errors, []);
});

test('reports every source when package versions drift', () => {
  const input = metadata();
  input.packageJson.version = '0.1.0';
  const { errors } = validateReleaseMetadata(input);
  assert.equal(errors.length, 1);
  assert.match(errors[0], /Version mismatch/);
  assert.match(errors[0], /package\.json=0\.1\.0/);
});

test('rejects missing, malformed, and non-SemVer versions', () => {
  const input = metadata();
  input.cargoToml = '[package]\nname = "murmur"\nedition = "2021"\n';
  input.tauriConfig.version = '01.0.0';
  const { errors } = validateReleaseMetadata(input);
  assert.equal(errors.length, 2);
  assert.match(errors[0], /Cargo\.toml is missing a valid semantic version/);
  assert.match(errors[1], /tauri\.conf\.json is missing a valid semantic version/);
});

test('rejects prerelease versions in release mode', () => {
  const { errors } = validateReleaseMetadata({ ...metadata(), release: true });
  assert.equal(errors.length, 1);
  assert.match(errors[0], /is a prerelease/);
});

test('accepts consistent stable versions in release mode', () => {
  assert.deepEqual(validateReleaseMetadata({ ...metadata('0.1.0'), release: true }).errors, []);
});
