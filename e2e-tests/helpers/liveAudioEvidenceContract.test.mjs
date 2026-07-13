import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  REQUIRED_LIVE_AUDIO_SMOKE_LABELS,
  REQUIRED_LIVE_AUDIO_SOAK_LABELS,
  REQUIRED_PAID_AUDIO_SCENARIO_IDS,
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

test('macOS gate and release verifier require semantic verification of paid audio', () => {
  const gate = readRepositoryFile('.github/workflows/macos-audio-gate.yml');
  const release = readRepositoryFile('.github/workflows/release.yml');
  const requiredIds = JSON.stringify(REQUIRED_PAID_AUDIO_SCENARIO_IDS);

  for (const source of [gate, release]) {
    assert.ok(source.includes(`required_audio_ids='${requiredIds}'`));
    assert.ok(source.includes('.required_audio_scenario_ids == $required_audio_ids'));
    assert.ok(source.includes('.audio_verified_scenario_ids == $required_audio_ids'));
  }
});
