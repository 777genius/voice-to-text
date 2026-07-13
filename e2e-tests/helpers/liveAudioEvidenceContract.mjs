export const REQUIRED_LIVE_AUDIO_SMOKE_LABELS = Object.freeze([
  'tauri-exit-shutdown-callback-binding',
  'translation-shutdown-claim-idempotence',
  'bidirectional-runtime-abort-helper',
  'suspension-watchdog-terminal-signal',
  'incoming-terminal-capture-service-cleanup',
  'local-websocket-abrupt-close',
  'outgoing-unexpected-close-service-cleanup',
  'output-health-terminal-signal',
  'incoming-output-terminal-service-cleanup',
  'incoming-system-default-playback',
  'macos-default-output-switch',
  'blackhole-loopback',
  'incoming-native-capture-format',
  'incoming-playback-nine-second-burst',
  'incoming-native-self-exclusion',
  'outgoing-live-translation-service',
  'incoming-captions-regression',
  'incoming-spoken-paid-matrix',
  'incoming-spoken-stop-mid-phrase',
  'incoming-spoken-paid-network-interruption',
  'paid-full-duplex-independent-stop',
]);

export const REQUIRED_LIVE_AUDIO_SOAK_LABELS = Object.freeze([
  'blackhole-loopback-preflight',
  'incoming-spoken-runtime-soak',
  'incoming-spoken-restart-stress',
  'incoming-spoken-native-soak',
  'outgoing-long-live-translation-soak',
  'incoming-long-system-audio-soak',
]);

export const REQUIRED_PAID_AUDIO_SCENARIO_IDS = Object.freeze([
  'english_to_russian',
  'names_and_numbers',
  'technical_terms',
  'mixed_english_russian',
  'long_context',
  'pause_and_silence',
  'overlapping_speakers',
  'half_volume_source',
]);

export function sameOrderedLabels(actual, expected) {
  return actual.length === expected.length
    && actual.every((label, index) => label === expected[index]);
}
