import process from 'node:process';

import {
  parsePositiveIntegerEnv,
  resolvePaidE2eEnvironment,
  runLiveAudioCommand,
  sanitizedAudioTestEnvironment,
} from './helpers/liveAudioRunner.mjs';

const DEFAULT_SOAK_SECONDS = 1800;
const soakSeconds = parsePositiveIntegerEnv(
  process.env.LIVE_AUDIO_SOAK_SECONDS,
  DEFAULT_SOAK_SECONDS,
);
const allowShortSoak = process.env.LIVE_AUDIO_ALLOW_SHORT_SOAK === '1';
const TEST_TIMEOUT_MS = (soakSeconds + 240) * 1000;

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

const paidE2e = resolvePaidE2eEnvironment();
const childBaseEnv = sanitizedAudioTestEnvironment();

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
}

console.log(
  allowShortSoak
    ? '\n[live-audio-soak] development-only short checks passed'
    : '\n[live-audio-soak] release-grade long-session checks passed',
);
