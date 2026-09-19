#!/usr/bin/env node
'use strict';
/*
 * Offline-safe smoke test for the npm wrapper (no network, no registry).
 * Uses DIET_CODE_BINARY_PATH to point at a local `diet-code` build and
 * exercises the launcher end-to-end.
 *
 *   DIET_CODE_BINARY_PATH=/path/to/diet-code node smoke-test.cjs
 */
const {execFileSync} = require('node:child_process');
const path = require('node:path');

function run(...args) {
  return execFileSync(process.execPath, [path.join(__dirname, 'bin', 'diet-code.js'), ...args], {
    encoding: 'utf8',
    env: {...process.env},
  });
}

const help = run('--help');
if (!help.includes('analyze') || !help.includes('benchmark')) {
  throw new Error('smoke: unexpected --help output:\n' + help);
}

// --version must pass through to the Rust binary.
const version = run('--version').trim();
if (!/^diet-code \d+\.\d+\.\d+/.test(version)) {
  throw new Error('smoke: unexpected --version output: ' + JSON.stringify(version));
}

console.log('smoke OK:', version);
