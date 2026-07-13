import process from 'node:process';

import {
  resolvePaidE2eEnvironment,
  runLiveAudioCommand,
  sanitizedAudioTestEnvironment,
} from './helpers/liveAudioRunner.mjs';

const TEST_TIMEOUT_MS = 180_000;

const tests = [
  {
    label: 'blackhole-loopback',
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
    label: 'incoming-native-capture-format',
    paid: false,
    testName: 'isolated_realtime_capture_emits_24khz_mono_and_stops_callbacks',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'isolated_realtime_capture_emits_24khz_mono_and_stops_callbacks',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-playback-nine-second-burst',
    paid: false,
    testName: 'incoming_spoken_profile_accepts_nine_second_burst_without_drop',
    command: [
      'cargo',
      'test',
      '--test',
      'blackhole_loopback_test',
      'incoming_spoken_profile_accepts_nine_second_burst_without_drop',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-native-self-exclusion',
    paid: false,
    testName: 'system_default_playback_is_drained_and_excluded_from_system_capture',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'system_default_playback_is_drained_and_excluded_from_system_capture',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'outgoing-live-translation-service',
    paid: true,
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
    label: 'incoming-captions-regression',
    paid: true,
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
  {
    label: 'incoming-spoken-half-volume',
    paid: true,
    testName: 'incoming_spoken_translation_returns_realtime_text_and_audio_from_system_capture',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'incoming_spoken_translation_returns_realtime_text_and_audio_from_system_capture',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
    env: {
      INCOMING_SPOKEN_E2E_SCENARIO: 'half_volume_source',
    },
  },
  {
    label: 'paid-full-duplex-independent-stop',
    paid: true,
    testName: 'simultaneous_incoming_and_outgoing_routes_translate_and_stop_independently',
    command: [
      'cargo',
      'test',
      '--test',
      'openai_realtime_translation_e2e_test',
      'simultaneous_incoming_and_outgoing_routes_translate_and_stop_independently',
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

const paidE2e = resolvePaidE2eEnvironment();
const childBaseEnv = sanitizedAudioTestEnvironment();

if (process.platform !== 'darwin') {
  fail('This smoke runner currently targets macOS BlackHole and ScreenCaptureKit.');
}

if (!paidE2e.acknowledged) {
  fail('VOICETEXT_RUN_PAID_E2E=1 is required to acknowledge paid API usage.');
}

if (!paidE2e.apiKey) {
  fail('OPENAI_E2E_API_KEY is required; OPENAI_API_KEY and .env are intentionally ignored.');
}

for (const { label, paid, testName, command, env = {} } of tests) {
  console.log(`\n[live-audio-smoke] running ${label}`);
  runLiveAudioCommand({
    command,
    env: {
      ...childBaseEnv,
      ...env,
      ...(paid
        ? {
            VOICETEXT_RUN_PAID_E2E: '1',
            OPENAI_E2E_API_KEY: paidE2e.apiKey,
          }
        : {}),
    },
    fail,
    label,
    maxBuffer: 20 * 1024 * 1024,
    testName,
    timeoutMs: TEST_TIMEOUT_MS,
  });
}

console.log('\n[live-audio-smoke] all live audio smoke tests passed');
