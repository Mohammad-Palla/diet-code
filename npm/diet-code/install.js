#!/usr/bin/env node
'use strict';
/*
 * diet-code postinstall: fetch the prebuilt Rust binary for this platform
 * from GitHub Releases into bin/vendor/. No dependencies, Node >= 18 only.
 *
 * Environment overrides:
 *   DIET_CODE_SKIP_DOWNLOAD=1   skip downloading (bring your own binary)
 *   DIET_CODE_DOWNLOAD_BASE_URL override the release base URL (mirror/offline)
 *   DIET_CODE_VERSION            override the version to download (default: package version)
 */
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const pkg = require('./package.json');
const PLATFORMS = require('./platforms.json');

const VERSION = process.env.DIET_CODE_VERSION || pkg.version;
const REPO = 'Mohammad-Palla/diet-code';
const BASE_URL =
  process.env.DIET_CODE_DOWNLOAD_BASE_URL ||
  `https://github.com/${REPO}/releases/download/v${VERSION}`;

function platformKey() {
  const osMap = {linux: 'linux', darwin: 'darwin', win32: 'win32'};
  const archMap = {x64: 'x64', arm64: 'arm64'};
  const os = osMap[process.platform];
  const arch = archMap[process.arch];
  if (!os || !arch) {
    return null;
  }
  return `${os}-${arch}`;
}

function vendorPath(key) {
  const ext = process.platform === 'win32' ? '.exe' : '';
  return path.join(__dirname, 'bin', 'vendor', `diet-code-${key}${ext}`);
}

async function sha256File(filePath) {
  const hash = crypto.createHash('sha256');
  const stream = fs.createReadStream(filePath);
  await new Promise((resolve, reject) => {
    stream.on('data', chunk => hash.update(chunk));
    stream.on('end', resolve);
    stream.on('error', reject);
  });
  return hash.digest('hex');
}

async function downloadTo(url, dest) {
  const response = await fetch(url, {redirect: 'follow'});
  if (!response.ok) {
    throw new Error(`download failed: HTTP ${response.status} for ${url}`);
  }
  const buffer = Buffer.from(await response.arrayBuffer());
  await fs.promises.writeFile(dest, buffer);
  return buffer;
}

async function main() {
  if (process.env.DIET_CODE_SKIP_DOWNLOAD) {
    console.log('[diet-code] DIET_CODE_SKIP_DOWNLOAD set — skipping binary download.');
    return;
  }

  const key = platformKey();
  const asset = key ? PLATFORMS[key] : undefined;
  if (!asset) {
    throw new Error(
      `[diet-code] unsupported platform: ${process.platform}-${process.arch}. ` +
        `Supported: ${Object.keys(PLATFORMS).join(', ')}. ` +
        'Build from source instead: clone ' +
        'https://github.com/Mohammad-Palla/diet-code and run ' +
        '`cargo install --path crates/diet-code-cli`.',
    );
  }

  const vendorDir = path.join(__dirname, 'bin', 'vendor');
  fs.mkdirSync(vendorDir, {recursive: true});
  const dest = vendorPath(key);

  const tmp = `${dest}.download-${process.pid}`;
  try {
    console.log(`[diet-code] downloading ${asset} (v${VERSION})…`);
    await downloadTo(`${BASE_URL}/${asset}`, tmp);

    // Verify against the release checksums when available; a missing
    // checksums file fails closed (no unverified binary is installed).
    let expected = null;
    try {
      const sums = await (await fetch(`${BASE_URL}/checksums.txt`)).text();
      for (const line of sums.split('\n')) {
        const [hash, name] = line.trim().split(/\s+/);
        if (name === asset) {
          expected = hash;
          break;
        }
      }
    } catch {
      expected = null;
    }
    if (!expected) {
      throw new Error(
        `[diet-code] no checksum found for ${asset} in ${BASE_URL}/checksums.txt — refusing to install an unverified binary.`,
      );
    }
    const actual = await sha256File(tmp);
    if (actual !== expected.toLowerCase()) {
      throw new Error(`[diet-code] checksum mismatch for ${asset}: expected ${expected}, got ${actual}.`);
    }

    fs.renameSync(tmp, dest);
    if (process.platform !== 'win32') {
      fs.chmodSync(dest, 0o755);
    }
    console.log(`[diet-code] installed ${dest}`);
  } finally {
    try {
      fs.unlinkSync(tmp);
    } catch {}
  }
}

main().catch(error => {
  console.error(error && error.message ? error.message : error);
  console.error(
    '[diet-code] postinstall failed. Set DIET_CODE_SKIP_DOWNLOAD=1 to skip, ' +
      'download a binary from ' +
      'https://github.com/Mohammad-Palla/diet-code/releases, ' +
      'or build from a checkout: `cargo install --path crates/diet-code-cli`',
  );
  process.exit(1);
});
