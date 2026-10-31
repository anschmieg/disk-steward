import { readFileSync, readdirSync } from 'node:fs';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const versions = {
  frontend: JSON.parse(read('package.json')).version,
  frontendLock: JSON.parse(read('package-lock.json')).version,
  frontendLockPackage: JSON.parse(read('package-lock.json')).packages[''].version,
  evaluationProbe: read('evaluation/live_jev.py').match(/^VERSION = "([^"]+)"/m)?.[1],
  httpsProbe: read('probes/https/transport_probe.py').match(/^VERSION = "([^"]+)"/m)?.[1],
  desktop: JSON.parse(read('src-tauri/tauri.conf.json')).version,
  rustCore: read('Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1],
  rustDesktop: read('src-tauri/Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1],
};
// Check explicit first-party probe/package versions too. Workspace-inherited
// versions already follow rustCore; dependencies are not app version fields.
const checkManifests = (directory) => {
  for (const entry of readdirSync(new URL(`../${directory}`, import.meta.url), { withFileTypes: true })) {
    if (entry.isDirectory() && entry.name !== 'target' && !entry.name.startsWith('.')) {
      checkManifests(`${directory}/${entry.name}`);
    } else if (entry.isFile() && entry.name === 'Cargo.toml') {
      const path = `${directory}/${entry.name}`;
      const version = read(path).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
      if (version) versions[path] = version;
    }
  }
};
checkManifests('crates');
checkManifests('probes');
for (const path of ['Cargo.lock', 'src-tauri/Cargo.lock']) {
  for (const block of read(path).split('[[package]]').slice(1)) {
    const name = block.match(/^name = "([^"]+)"/m)?.[1];
    if (name?.startsWith('disk-steward') ||
        ['approval-ledger', 'verification-budget', 'atomic-rename-probe'].includes(name)) {
      versions[`${path}:${name}`] = block.match(/^version = "([^"]+)"/m)?.[1];
    }
  }
}

if (Object.values(versions).some((version) => !/^0\.0\.\d+$/.test(version ?? '')) ||
    new Set(Object.values(versions)).size !== 1) {
  throw new Error(`Versions must match and remain 0.0.x until authorized: ${JSON.stringify(versions)}`);
}
console.log(`Version policy passed: ${versions.frontend}`);
