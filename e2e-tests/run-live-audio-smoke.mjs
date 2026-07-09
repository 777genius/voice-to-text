import process from 'node:process';

import {
  readEnvOpenAiKey,
  runLiveAudioCommand,
} from './helpers/liveAudioRunner.mjs';

const TEST_TIMEOUT_MS = 180_000;

const tests = [
  {
    label: 'blackhole-loopback',
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
    label: 'outgoing-live-translation-service',
    testName: 'live_translation_service_synthetic_voice_reaches_blackhole',
    command: [
      'cargo',
      'test',
      '--test',
      'openai_realtime_translation_e2e_test',
      'live_translation_service_synthetic_voice_reaches_blackhole',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-system-audio-translation-service',
    testName: 'incoming_translation_service_captures_system_audio_and_emits_translated_text',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'incoming_translation_service_captures_system_audio_and_emits_translated_text',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
];

function fail(message) {
  console.error(`[live-audio-smoke] ${message}`);
  process.exit(1);
}

const openaiApiKey = readEnvOpenAiKey();

if (!openaiApiKey) {
  fail('OPENAI_API_KEY is required. Set it in the environment or src-tauri/.env.');
}

if (process.platform !== 'darwin') {
  fail('This smoke runner currently targets macOS BlackHole and ScreenCaptureKit.');
}

for (const { label, testName, command } of tests) {
  console.log(`\n[live-audio-smoke] running ${label}`);
  runLiveAudioCommand({
    command,
    env: { ...process.env, OPENAI_API_KEY: openaiApiKey },
    fail,
    label,
    maxBuffer: 20 * 1024 * 1024,
    testName,
    timeoutMs: TEST_TIMEOUT_MS,
  });
}

console.log('\n[live-audio-smoke] all live audio smoke tests passed');
