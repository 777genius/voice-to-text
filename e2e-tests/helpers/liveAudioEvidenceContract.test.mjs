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

test('native spoken soak measurements are durable release evidence', () => {
  const runner = readRepositoryFile('e2e-tests/run-live-audio-soak.mjs');
  const gate = readRepositoryFile('.github/workflows/macos-audio-gate.yml');
  const release = readRepositoryFile('.github/workflows/release.yml');

  assert.ok(runner.includes('native_spoken_metrics: nativeSpokenMetrics'));
  assert.ok(runner.includes('restart_stress_metrics: restartStressMetrics'));
  assert.ok(gate.includes('native_spoken_soak: $soak[0].native_spoken_metrics'));
  assert.ok(gate.includes('spoken_restart_stress: $soak[0].restart_stress_metrics'));
  for (const field of [
    'playback_dropped_batches',
    'playback_pending_high_water_ms',
    'playback_pending_at_close_ms',
    'rss_growth_kib',
    'last_translation_text_age_ms',
    'last_translation_audio_age_ms',
  ]) {
    assert.ok(gate.includes(`.native_spoken_metrics.${field}`));
    assert.ok(release.includes(`.native_spoken_soak.${field}`));
  }
});

test('paid matrix audio artifacts are checksummed and verified before release', () => {
  const gate = readRepositoryFile('.github/workflows/macos-audio-gate.yml');
  const release = readRepositoryFile('.github/workflows/release.yml');

  assert.ok(gate.includes('cp -R "$matrix_artifacts" audio-gate-evidence/incoming-spoken-paid-matrix'));
  assert.ok(gate.includes('shasum -a 256 "$artifact"'));
  assert.ok(release.includes('shasum -a 256 -c SHA256SUMS'));
  for (const artifact of [
    'source-primary.aiff',
    'captured-input.wav',
    'captured-input-transcript.txt',
    'translated-transcript.txt',
    'translated-audio.wav',
    'translated-audio-transcript.txt',
    'metrics.json',
  ]) {
    assert.ok(gate.includes(artifact));
    assert.ok(release.includes(artifact));
  }
});

test('release evidence requires manual Zoom and hardware recovery attestations', () => {
  const gate = readRepositoryFile('.github/workflows/macos-audio-gate.yml');
  const release = readRepositoryFile('.github/workflows/release.yml');

  for (const input of [
    'zoom_half_volume_bidirectional_verified',
    'output_disconnect_recovery_verified',
    'sleep_wake_recovery_verified',
  ]) {
    assert.ok(gate.includes(`${input}:`), `macOS gate does not declare ${input}`);
  }

  for (const field of [
    'zoom_half_volume_bidirectional',
    'output_disconnect_recovery',
    'sleep_wake_recovery',
  ]) {
    assert.ok(gate.includes(`${field}: $`), `macOS gate does not persist ${field}`);
    assert.ok(release.includes(`.manual_attestations.${field} == true`));
  }
  assert.ok(gate.includes('manual_attestation_actor: $manual_attestation_actor'));
  assert.ok(release.includes('(.manual_attestation_actor | length) > 0'));
  assert.ok(release.includes('.manual_attestation_run_id == $audio_gate_run_id'));
});

test('normal CI and release share the same keyless quality gate workflow', () => {
  const ci = readRepositoryFile('.github/workflows/ci.yml');
  const quality = readRepositoryFile('.github/workflows/quality-gates.yml');
  const release = readRepositoryFile('.github/workflows/release.yml');
  const reusableWorkflow = 'uses: ./.github/workflows/quality-gates.yml';

  assert.ok(ci.includes('pull_request:'));
  assert.ok(ci.includes(reusableWorkflow));
  assert.ok(release.includes(reusableWorkflow));
  assert.ok(quality.includes('workflow_call:'));
  assert.ok(!quality.includes('OPENAI_E2E_API_KEY'));
  assert.ok(!quality.includes('VOICETEXT_RUN_PAID_E2E'));
});
