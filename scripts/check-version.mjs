import { readFileSync } from 'node:fs';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const versions = {
  frontend: JSON.parse(read('package.json')).version,
  desktop: JSON.parse(read('src-tauri/tauri.conf.json')).version,
  rustCore: read('Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1],
  rustDesktop: read('src-tauri/Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1],
};
if (Object.values(versions).some((version) => !/^0\.0\.\d+$/.test(version ?? '')) ||
    new Set(Object.values(versions)).size !== 1) {
  throw new Error(`Versions must match and remain 0.0.x until authorized: ${JSON.stringify(versions)}`);
}
console.log(`Version policy passed: ${versions.frontend}`);
