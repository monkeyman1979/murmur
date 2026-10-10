const NUMERIC_IDENTIFIER = '(?:0|[1-9]\\d*)';
const PRERELEASE_IDENTIFIER = '(?:0|[1-9]\\d*|\\d*[A-Za-z-][0-9A-Za-z-]*)';
const BUILD_IDENTIFIER = '[0-9A-Za-z-]+';
const VERSION_PATTERN = new RegExp(
  `^${NUMERIC_IDENTIFIER}\\.${NUMERIC_IDENTIFIER}\\.${NUMERIC_IDENTIFIER}` +
    `(?:-(${PRERELEASE_IDENTIFIER}(?:\\.${PRERELEASE_IDENTIFIER})*))?` +
    `(?:\\+(${BUILD_IDENTIFIER}(?:\\.${BUILD_IDENTIFIER})*))?$`,
);

export function parseCargoPackageVersion(cargoToml) {
  let inPackage = false;

  for (const line of cargoToml.split(/\r?\n/)) {
    if (/^\s*\[/.test(line)) {
      if (inPackage) break;
      inPackage = /^\s*\[package\]\s*(?:#.*)?$/.test(line);
      continue;
    }

    if (!inPackage) continue;
    const match = line.match(/^\s*version\s*=\s*"([^"]+)"\s*(?:#.*)?$/);
    if (match) return match[1];
  }

  return null;
}

export function validateReleaseMetadata({ packageJson, cargoToml, tauriConfig, release = false }) {
  const versions = {
    'package.json': packageJson?.version ?? null,
    'src-tauri/Cargo.toml': parseCargoPackageVersion(cargoToml ?? ''),
    'src-tauri/tauri.conf.json': tauriConfig?.version ?? null,
  };
  const errors = [];

  for (const [source, version] of Object.entries(versions)) {
    if (typeof version !== 'string' || !VERSION_PATTERN.test(version)) {
      errors.push(`${source} is missing a valid semantic version.`);
    }
  }

  const validVersions = Object.entries(versions).filter(([, version]) =>
    typeof version === 'string' && VERSION_PATTERN.test(version),
  );
  const uniqueVersions = new Set(validVersions.map(([, version]) => version));
  if (uniqueVersions.size > 1) {
    errors.push(`Version mismatch: ${validVersions.map(([source, version]) => `${source}=${version}`).join(', ')}.`);
  }

  const appVersion = versions['src-tauri/tauri.conf.json'];
  const parsedVersion = typeof appVersion === 'string' ? appVersion.match(VERSION_PATTERN) : null;
  if (release && parsedVersion?.[1]) {
    errors.push(`Release version ${appVersion} is a prerelease; choose a stable version before releasing.`);
  }

  return { errors, versions };
}
