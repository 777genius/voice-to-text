import process from 'node:process';

import {
  parsePositiveIntegerEnv,
  readEnvOpenAiKey,
  runLiveAudioCommand,
} from './helpers/liveAudioRunner.mjs';

const DEFAULT_SOAK_SECONDS = 600;
const soakSeconds = parsePositiveIntegerEnv(
  process.env.LIVE_AUDIO_SOAK_SECONDS,
  DEFAULT_SOAK_SECONDS,
);
const TEST_TIMEOUT_MS = (soakSeconds + 240) * 1000;

const tests = [
  {
    label: 'blackhole-loopback-preflight',
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
    label: 'outgoing-long-live-translation-soak',
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

const openaiApiKey = readEnvOpenAiKey();

if (!openaiApiKey) {
  fail('OPENAI_API_KEY is required. Set it in the environment or src-tauri/.env.');
}

if (process.platform !== 'darwin') {
  fail('This soak runner currently targets macOS BlackHole and ScreenCaptureKit.');
}

if (soakSeconds < 60) {
  console.warn(`[live-audio-soak] LIVE_AUDIO_SOAK_SECONDS=${soakSeconds}; use 600-1800 for release-grade soak.`);
}

for (const { label, testName, command } of tests) {
  console.log(`\n[live-audio-soak] running ${label} (${soakSeconds}s soak window)`);
  runLiveAudioCommand({
    command,
    env: {
      ...process.env,
      OPENAI_API_KEY: openaiApiKey,
      LIVE_AUDIO_SOAK_SECONDS: String(soakSeconds),
    },
    fail,
    label,
    maxBuffer: 24 * 1024 * 1024,
    testName,
    timeoutMs: TEST_TIMEOUT_MS,
  });
}

console.log('\n[live-audio-soak] all long-session checks passed');
