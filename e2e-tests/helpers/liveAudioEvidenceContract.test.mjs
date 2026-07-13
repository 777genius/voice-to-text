import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  REQUIRED_LIVE_AUDIO_SMOKE_LABELS,
  REQUIRED_LIVE_AUDIO_SOAK_LABELS,
} from './liveAudioEvidenceContract.mjs';

const helperDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(helperDirectory, '..', '..');

function readRepositoryFile(path) {
  return readFileSync(resolve(repositoryRoot, path), 'utf8');
}

function runnerLabels(source) {
  return [...source.matchAll(/\blabel:\s*'([^']+)'/g)].map((match) => match[1]);
}

test('live audio runners execute the complete ordered evidence contract', () => {
  const smokeRunner = readRepositoryFile('e2e-tests/run-live-audio-smoke.mjs');
  const soakRunner = readRepositoryFile('e2e-tests/run-live-audio-soak.mjs');

  assert.deepEqual(runnerLabels(smokeRunner), REQUIRED_LIVE_AUDIO_SMOKE_LABELS);
  assert.deepEqual(runnerLabels(soakRunner), REQUIRED_LIVE_AUDIO_SOAK_LABELS);
});

test('macOS gate and release verifier require every live audio evidence label', () => {
  const gate = readRepositoryFile('.github/workflows/macos-audio-gate.yml');
  const release = readRepositoryFile('.github/workflows/release.yml');

  for (const label of [
    ...REQUIRED_LIVE_AUDIO_SMOKE_LABELS,
    ...REQUIRED_LIVE_AUDIO_SOAK_LABELS,
  ]) {
    assert.ok(gate.includes(`\"${label}\"`), `macOS gate does not require ${label}`);
    assert.ok(release.includes(`\"${label}\"`), `release verifier does not require ${label}`);
  }
});
