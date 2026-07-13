import { readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';

import {
  liveAudioCargoEnvironment,
  nativeSpokenSoakMetricViolations,
  parsePositiveIntegerEnv,
  preflightLiveAudioCommands,
  resolvePaidE2eEnvironment,
  runLiveAudioCommand,
  sanitizedAudioTestEnvironment,
  spokenRestartStressMetricViolations,
  writeLiveAudioSummary,
} from './helpers/liveAudioRunner.mjs';
import {
  REQUIRED_LIVE_AUDIO_SOAK_LABELS,
  sameOrderedLabels,
} from './helpers/liveAudioEvidenceContract.mjs';

const DEFAULT_SOAK_SECONDS = 1800;
const CARGO_PREFLIGHT_TIMEOUT_MS = 30 * 60 * 1000;
const soakSeconds = parsePositiveIntegerEnv(
  process.env.LIVE_AUDIO_SOAK_SECONDS,
  DEFAULT_SOAK_SECONDS,
);
const allowShortSoak = process.env.LIVE_AUDIO_ALLOW_SHORT_SOAK === '1';
const TEST_TIMEOUT_MS = (soakSeconds + 240) * 1000;
const NATIVE_SPOKEN_METRICS_PATH = join(
  process.cwd(),
  'src-tauri',
  'target',
  'e2e-artifacts',
  'incoming-spoken-native-soak',
  'metrics.json',
);
const RESTART_STRESS_METRICS_PATH = join(
  process.cwd(),
  'src-tauri',
  'target',
  'e2e-artifacts',
  'incoming-spoken-restart-stress',
  'metrics.json',
);

const tests = [
  {
    label: 'blackhole-loopback-preflight',
    paid: false,
    testName: 'cpal_output_reaches_blackhole_input',
    command: [
      'cargo',
      'test',
      '--test',
      'blackhole_loopback_test',
      'cpal_output_reaches_blackhole_input',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-spoken-runtime-soak',
    paid: false,
    testName: 'spoken_runtime_long_soak_keeps_audio_flow_bounded_and_stops_cleanly',
    command: [
      'cargo',
      'test',
      '--test',
      'realtime_translation_websocket_e2e_test',
      'spoken_runtime_long_soak_keeps_audio_flow_bounded_and_stops_cleanly',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
    env: {
      SPOKEN_TRANSLATION_SOAK_SECONDS: String(soakSeconds),
    },
  },
  {
    label: 'incoming-spoken-restart-stress',
    paid: false,
    testName: 'spoken_runtime_repeated_start_stop_stress_releases_every_session_and_task',
    command: [
      'cargo',
      'test',
      '--test',
      'realtime_translation_websocket_e2e_test',
      'spoken_runtime_repeated_start_stop_stress_releases_every_session_and_task',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
    env: {
      SPOKEN_TRANSLATION_RESTART_STRESS_CYCLES: '25',
    },
  },
  {
    label: 'incoming-spoken-native-soak',
    paid: true,
    testName: 'incoming_spoken_translation_long_running_native_soak',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'incoming_spoken_translation_long_running_native_soak',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'outgoing-long-live-translation-soak',
    paid: true,
    testName: 'live_translation_service_long_running_synthetic_voice_soak',
    command: [
      'cargo',
      'test',
      '--test',
      'openai_realtime_translation_e2e_test',
      'live_translation_service_long_running_synthetic_voice_soak',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-long-system-audio-soak',
    paid: true,
    testName: 'incoming_translation_service_long_running_system_audio_soak',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'incoming_translation_service_long_running_system_audio_soak',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
];

function fail(message) {
  console.error(`[live-audio-soak] ${message}`);
  process.exit(1);
}

const plannedLabels = tests.map(({ label }) => label);
if (!sameOrderedLabels(plannedLabels, REQUIRED_LIVE_AUDIO_SOAK_LABELS)) {
  fail('Soak test plan does not match the release evidence contract.');
}

const paidE2e = resolvePaidE2eEnvironment();
const childBaseEnv = liveAudioCargoEnvironment({
  env: sanitizedAudioTestEnvironment(),
});

if (process.platform !== 'darwin') {
  fail('This soak runner currently targets macOS BlackHole and ScreenCaptureKit.');
}

if (!paidE2e.acknowledged) {
  fail('VOICETEXT_RUN_PAID_E2E=1 is required to acknowledge paid API usage.');
}

if (!paidE2e.apiKey) {
  fail('OPENAI_E2E_API_KEY is required; OPENAI_API_KEY and .env are intentionally ignored.');
}

if (soakSeconds < 1800) {
  if (!allowShortSoak) {
    fail(`LIVE_AUDIO_SOAK_SECONDS=${soakSeconds}; release gate requires at least 1800 seconds. Set LIVE_AUDIO_ALLOW_SHORT_SOAK=1 only for local development.`);
  }
  console.warn(`[live-audio-soak] development-only short soak enabled (${soakSeconds}s); this is not release evidence.`);
}

preflightLiveAudioCommands({
  tests,
  env: childBaseEnv,
  fail,
  maxBuffer: 24 * 1024 * 1024,
  timeoutMs: CARGO_PREFLIGHT_TIMEOUT_MS,
});

rmSync(NATIVE_SPOKEN_METRICS_PATH, { force: true });
rmSync(RESTART_STRESS_METRICS_PATH, { force: true });

const passedLabels = [];
for (const { label, paid, testName, command, env = {} } of tests) {
  console.log(`\n[live-audio-soak] running ${label} (${soakSeconds}s soak window)`);
  runLiveAudioCommand({
    command,
    env: {
      ...childBaseEnv,
      ...env,
      LIVE_AUDIO_SOAK_SECONDS: String(soakSeconds),
      ...(paid
        ? {
            VOICETEXT_RUN_PAID_E2E: '1',
            OPENAI_E2E_API_KEY: paidE2e.apiKey,
          }
        : {}),
    },
    fail,
    label,
    maxBuffer: 24 * 1024 * 1024,
    testName,
    timeoutMs: TEST_TIMEOUT_MS,
  });
  passedLabels.push(label);
}

const releaseGrade = soakSeconds >= DEFAULT_SOAK_SECONDS;
let nativeSpokenMetrics;
try {
  nativeSpokenMetrics = JSON.parse(readFileSync(NATIVE_SPOKEN_METRICS_PATH, 'utf8'));
} catch (error) {
  fail(`cannot read native spoken soak metrics: ${error.message}`);
}
const nativeMetricViolations = nativeSpokenSoakMetricViolations(nativeSpokenMetrics, {
  soakSeconds,
  releaseGrade,
});
if (nativeMetricViolations.length > 0) {
  fail(`native spoken soak metrics violate ${nativeMetricViolations.join(', ')}: ${JSON.stringify(nativeSpokenMetrics)}`);
}
let restartStressMetrics;
try {
  restartStressMetrics = JSON.parse(readFileSync(RESTART_STRESS_METRICS_PATH, 'utf8'));
} catch (error) {
  fail(`cannot read spoken restart stress metrics: ${error.message}`);
}
const restartStressViolations = spokenRestartStressMetricViolations(restartStressMetrics, {
  expectedCycles: 25,
});
if (restartStressViolations.length > 0) {
  fail(`spoken restart stress metrics violate ${restartStressViolations.join(', ')}: ${JSON.stringify(restartStressMetrics)}`);
}
const summaryPath = writeLiveAudioSummary('live-audio-soak-summary.json', {
  schema_version: 1,
  platform: process.platform,
  soak_seconds: soakSeconds,
  release_grade: releaseGrade,
  passed_labels: passedLabels,
  native_spoken_metrics: nativeSpokenMetrics,
  restart_stress_metrics: restartStressMetrics,
});

console.log(
  releaseGrade
    ? `\n[live-audio-soak] release-grade long-session checks passed; summary=${summaryPath}`
    : `\n[live-audio-soak] development-only short checks passed; summary=${summaryPath}`,
);
