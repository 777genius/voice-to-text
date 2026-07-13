import { rmSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';

import {
  resolvePaidE2eEnvironment,
  runLiveAudioCommand,
  sanitizedAudioTestEnvironment,
  writeLiveAudioSummary,
} from './helpers/liveAudioRunner.mjs';

const TEST_TIMEOUT_MS = 180_000;
const PAID_MATRIX_ARTIFACT_DIRECTORY = join(
  process.cwd(),
  'src-tauri',
  'target',
  'e2e-artifacts',
  'incoming-spoken-paid-matrix',
);

const tests = [
  {
    label: 'tauri-exit-shutdown-callback-binding',
    paid: false,
    testName: 'tests::tauri_exit_event_invokes_translation_shutdown_callback',
    command: [
      'cargo',
      'test',
      '--lib',
      'tests::tauri_exit_event_invokes_translation_shutdown_callback',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'translation-shutdown-claim-idempotence',
    paid: false,
    testName:
      'presentation::state::tests::translation_shutdown_claim_is_exactly_once_for_duplicate_exit_events',
    command: [
      'cargo',
      'test',
      '--lib',
      'presentation::state::tests::translation_shutdown_claim_is_exactly_once_for_duplicate_exit_events',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'bidirectional-runtime-abort-helper',
    paid: false,
    testName:
      'application::services::live_translation_service::tests::simultaneous_directions_stop_independently_and_app_exit_aborts_both',
    command: [
      'cargo',
      'test',
      '--lib',
      'application::services::live_translation_service::tests::simultaneous_directions_stop_independently_and_app_exit_aborts_both',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'suspension-watchdog-terminal-signal',
    paid: false,
    testName:
      'application::services::realtime_interpretation::session::tests::suspension_watchdog_reports_terminal_cleanup_after_runtime_pause',
    command: [
      'cargo',
      'test',
      '--lib',
      'application::services::realtime_interpretation::session::tests::suspension_watchdog_reports_terminal_cleanup_after_runtime_pause',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-terminal-capture-service-cleanup',
    paid: false,
    testName:
      'application::services::incoming_spoken_translation_service::tests::terminal_capture_failure_cleans_runtime_and_allows_restart',
    command: [
      'cargo',
      'test',
      '--lib',
      'application::services::incoming_spoken_translation_service::tests::terminal_capture_failure_cleans_runtime_and_allows_restart',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'local-websocket-abrupt-close',
    paid: false,
    testName: 'abrupt_close_emits_closed_and_stalled_close_is_bounded',
    command: [
      'cargo',
      'test',
      '--test',
      'realtime_translation_websocket_e2e_test',
      'abrupt_close_emits_closed_and_stalled_close_is_bounded',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'outgoing-unexpected-close-service-cleanup',
    paid: false,
    testName:
      'application::services::live_translation_service::tests::unexpected_openai_close_cleans_session_and_allows_restart',
    command: [
      'cargo',
      'test',
      '--lib',
      'application::services::live_translation_service::tests::unexpected_openai_close_cleans_session_and_allows_restart',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'output-health-terminal-signal',
    paid: false,
    testName:
      'application::services::realtime_interpretation::session::tests::output_health_failure_is_a_terminal_device_error',
    command: [
      'cargo',
      'test',
      '--lib',
      'application::services::realtime_interpretation::session::tests::output_health_failure_is_a_terminal_device_error',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-output-terminal-service-cleanup',
    paid: false,
    testName:
      'application::services::incoming_spoken_translation_service::tests::terminal_output_failure_cleans_runtime_and_allows_restart',
    command: [
      'cargo',
      'test',
      '--lib',
      'application::services::incoming_spoken_translation_service::tests::terminal_output_failure_cleans_runtime_and_allows_restart',
      '--',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'incoming-system-default-playback',
    paid: false,
    testName: 'system_default_output_reaches_selected_blackhole_device',
    command: [
      'cargo',
      'test',
      '--test',
      'blackhole_loopback_test',
      'system_default_output_reaches_selected_blackhole_device',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
  {
    label: 'macos-default-output-switch',
    paid: false,
    testName: 'system_default_output_switch_is_a_terminal_health_error_and_restores_route',
    command: [
      'cargo',
      'test',
      '--test',
      'blackhole_loopback_test',
      'system_default_output_switch_is_a_terminal_health_error_and_restores_route',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
  },
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
    label: 'incoming-spoken-paid-matrix',
    paid: true,
    testName: 'incoming_spoken_translation_returns_realtime_text_and_audio_from_system_capture',
    env: {
      INCOMING_SPOKEN_E2E_SCENARIO: 'all',
      INCOMING_SPOKEN_E2E_ARTIFACTS: PAID_MATRIX_ARTIFACT_DIRECTORY,
    },
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
    timeoutMs: 1_200_000,
  },
  {
    label: 'incoming-spoken-stop-mid-phrase',
    paid: true,
    testName: 'incoming_spoken_translation_paid_stop_mid_phrase_is_bounded',
    command: [
      'cargo',
      'test',
      '--test',
      'incoming_system_audio_translation_e2e_test',
      'incoming_spoken_translation_paid_stop_mid_phrase_is_bounded',
      '--',
      '--ignored',
      '--exact',
      '--nocapture',
    ],
    timeoutMs: 240_000,
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

rmSync(PAID_MATRIX_ARTIFACT_DIRECTORY, { recursive: true, force: true });

const passedLabels = [];
for (const { label, paid, testName, command, env = {}, timeoutMs = TEST_TIMEOUT_MS } of tests) {
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
    timeoutMs,
  });
  passedLabels.push(label);
}

const summaryPath = writeLiveAudioSummary('live-audio-smoke-summary.json', {
  schema_version: 1,
  platform: process.platform,
  passed_labels: passedLabels,
});

console.log(`\n[live-audio-smoke] all live audio smoke tests passed; summary=${summaryPath}`);
