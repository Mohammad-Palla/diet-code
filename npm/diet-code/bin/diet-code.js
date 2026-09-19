#!/usr/bin/env node
'use strict';
/*
 * diet-code bin launcher: exec the vendored Rust binary with the user's args.
 * No dependencies. Honors DIET_CODE_BINARY_PATH to override the binary
 * location (local builds, testing).
 */
const {spawnSync} = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const PLATFORMS = require('../platforms.json');

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

function resolveBinary() {
  if (process.env.DIET_CODE_BINARY_PATH) {
    return process.env.DIET_CODE_BINARY_PATH;
  }
  const key = platformKey();
  if (!key || !PLATFORMS[key]) {
    return null;
  }
  const ext = process.platform === 'win32' ? '.exe' : '';
  return path.join(__dirname, 'vendor', `diet-code-${key}${ext}`);
}

function main() {
  const bin = resolveBinary();
  if (!bin || !fs.existsSync(bin)) {
    console.error(
      '[diet-code] binary not found.\n' +
        'The postinstall download probably failed (offline install?).\n' +
        'Fix: `npm rebuild diet-code`, or set DIET_CODE_BINARY_PATH to a local build, ' +
        'or install from source: cargo install diet-code-cli',
    );
    process.exit(1);
  }
  const result = spawnSync(bin, process.argv.slice(2), {stdio: 'inherit'});
  if (result.error) {
    console.error(`[diet-code] failed to launch: ${result.error.message}`);
    process.exit(1);
  }
  process.exit(result.status ?? 1);
}

main();
