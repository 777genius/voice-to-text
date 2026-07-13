import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  assertCommandSucceeded,
  assertExpectedTestRan,
  liveAudioCargoEnvironment,
  liveAudioCargoPreflightCommands,
  nativeSpokenSoakMetricViolations,
  parsePositiveIntegerEnv,
  resolvePaidE2eEnvironment,
  sanitizedAudioTestEnvironment,
  spokenRestartStressMetricViolations,
  terminateProcessGroup,
  writeLiveAudioSummary,
} from './liveAudioRunner.mjs';

const validNativeSpokenMetrics = Object.freeze({
  schema_version: 1,
  soak_seconds: 1800,
  release_grade: true,
  source_play_count: 7,
  translated_text_chars: 120,
  translated_audible_samples: 48_000,
  playback_dropped_batches: 0,
  playback_pending_high_water_ms: 850,
  playback_pending_at_close_ms: 20,
  rss_samples_kib: [100_000, 101_000],
  rss_growth_kib: 1000,
  last_translation_text_age_ms: 25_000,
  last_translation_audio_age_ms: 26_000,
  errors: [],
});

const validRestartStressMetrics = Object.freeze({
  schema_version: 1,
  cycles: 25,
  capture_starts: 25,
  capture_stops: 25,
  output_opens: 25,
  output_closes: 25,
  translation_sessions_created: 25,
  translation_sessions_dropped: 25,
  translation_finishes: 25,
  translation_aborts: 0,
  post_stop_send_attempts: 25,
  error_callbacks: 0,
  active_capture_high_water: 1,
  active_output_high_water: 1,
  active_translation_session_high_water: 1,
  active_translation_task_high_water: 1,
  active_websocket_high_water: 1,
  active_server_task_high_water: 1,
  rss_samples_kib: [100_000, 100_096],
  rss_growth_kib: 96,
});

function throwingFail(message) {
  throw new Error(message);
}

test('resolvePaidE2eEnvironment accepts only the dedicated acknowledged key', () => {
  const result = resolvePaidE2eEnvironment({
    env: {
      VOICETEXT_RUN_PAID_E2E: '1',
      OPENAI_E2E_API_KEY: '  dedicated-test-key  ',
      OPENAI_API_KEY: 'general-key-must-be-ignored',
    },
  });

  assert.deepEqual(result, {
    acknowledged: true,
    apiKey: 'dedicated-test-key',
  });
});

test('resolvePaidE2eEnvironment does not treat other acknowledgement values as consent', () => {
  const result = resolvePaidE2eEnvironment({
    env: {
      VOICETEXT_RUN_PAID_E2E: 'true',
      OPENAI_E2E_API_KEY: 'dedicated-test-key',
    },
  });

  assert.equal(result.acknowledged, false);
  assert.equal(result.apiKey, 'dedicated-test-key');
});

test('resolvePaidE2eEnvironment ignores the general OpenAI key', () => {
  const result = resolvePaidE2eEnvironment({
    env: {
      VOICETEXT_RUN_PAID_E2E: '1',
      OPENAI_API_KEY: 'general-key-must-be-ignored',
    },
  });

  assert.equal(result.acknowledged, true);
  assert.equal(result.apiKey, '');
});

test('sanitizedAudioTestEnvironment removes all paid credentials from native child processes', () => {
  const result = sanitizedAudioTestEnvironment({
    env: {
      PATH: '/test/bin',
      OPENAI_API_KEY: 'general-key',
      OPENAI_E2E_API_KEY: 'dedicated-key',
      VOICETEXT_RUN_PAID_E2E: '1',
    },
  });

  assert.deepEqual(result, { PATH: '/test/bin' });
});

test('liveAudioCargoEnvironment isolates stable non-incremental test artifacts', () => {
  const result = liveAudioCargoEnvironment({
    env: { PATH: '/test/bin' },
    root: '/repository',
  });

  assert.deepEqual(result, {
    PATH: '/test/bin',
    CARGO_TARGET_DIR: join(
      '/repository',
      'src-tauri',
      'target',
      'live-audio-cargo',
    ),
    CARGO_INCREMENTAL: '0',
    CARGO_PROFILE_TEST_SPLIT_DEBUGINFO: 'packed',
  });
});

test('liveAudioCargoEnvironment preserves explicit Cargo overrides', () => {
  const result = liveAudioCargoEnvironment({
    env: {
      CARGO_TARGET_DIR: ' /tmp/audio-target ',
      CARGO_INCREMENTAL: ' 1 ',
      CARGO_PROFILE_TEST_SPLIT_DEBUGINFO: ' off ',
    },
    root: '/repository',
  });

  assert.equal(result.CARGO_TARGET_DIR, '/tmp/audio-target');
  assert.equal(result.CARGO_INCREMENTAL, '1');
  assert.equal(result.CARGO_PROFILE_TEST_SPLIT_DEBUGINFO, 'off');
});

test('liveAudioCargoPreflightCommands compiles each Cargo target once without test filters', () => {
  const commands = liveAudioCargoPreflightCommands([
    {
      testName: 'module::first_case',
      command: [
        'cargo',
        'test',
        '--lib',
        'module::first_case',
        '--',
        '--exact',
      ],
    },
    {
      testName: 'module::second_case',
      command: [
        'cargo',
        'test',
        '--lib',
        'module::second_case',
        '--',
        '--exact',
      ],
    },
    {
      testName: 'captures_audio',
      command: [
        'cargo',
        'test',
        '--test',
        'incoming_audio_test',
        'captures_audio',
        '--',
        '--ignored',
        '--exact',
      ],
    },
  ]);

  assert.deepEqual(commands, [
    ['cargo', 'test', '--locked', '--lib', '--no-run'],
    [
      'cargo',
      'test',
      '--locked',
      '--test',
      'incoming_audio_test',
      '--no-run',
    ],
  ]);
});

test('liveAudioCargoPreflightCommands rejects commands without the declared filter', () => {
  assert.throws(
    () =>
      liveAudioCargoPreflightCommands([
        {
          testName: 'missing_case',
          command: ['cargo', 'test', '--lib', 'different_case'],
        },
      ]),
    /does not contain test filter/,
  );
});

test('parsePositiveIntegerEnv accepts only complete positive integer strings', () => {
  assert.equal(parsePositiveIntegerEnv('600', 120), 600);
  assert.equal(parsePositiveIntegerEnv(' 90 ', 120), 90);
  assert.equal(parsePositiveIntegerEnv('5m', 120), 120);
  assert.equal(parsePositiveIntegerEnv('0', 120), 120);
  assert.equal(parsePositiveIntegerEnv('-5', 120), 120);
  assert.equal(parsePositiveIntegerEnv('', 120), 120);
});

test('native spoken soak metrics require complete bounded release evidence', () => {
  assert.deepEqual(
    nativeSpokenSoakMetricViolations(validNativeSpokenMetrics, {
      soakSeconds: 1800,
      releaseGrade: true,
    }),
    [],
  );
});

test('native spoken soak metrics reject stale, null, dropped, and growing evidence', () => {
  const violations = nativeSpokenSoakMetricViolations(
    {
      ...validNativeSpokenMetrics,
      soak_seconds: 60,
      playback_dropped_batches: 1,
      rss_growth_kib: 32_769,
      last_translation_audio_age_ms: null,
    },
    { soakSeconds: 1800, releaseGrade: true },
  );

  assert.deepEqual(violations, [
    'soak_seconds',
    'playback_dropped_batches',
    'rss_growth_kib',
    'last_translation_audio_age_ms',
  ]);
});

test('spoken restart stress metrics require exact lifecycle balance', () => {
  assert.deepEqual(
    spokenRestartStressMetricViolations(validRestartStressMetrics, {
      expectedCycles: 25,
    }),
    [],
  );
});

test('spoken restart stress metrics reject leaked sessions and RSS growth', () => {
  const violations = spokenRestartStressMetricViolations(
    {
      ...validRestartStressMetrics,
      output_closes: 24,
      translation_sessions_dropped: 24,
      active_websocket_high_water: 2,
      rss_growth_kib: 16_385,
    },
    { expectedCycles: 25 },
  );

  assert.deepEqual(violations, [
    'output_closes',
    'translation_sessions_dropped',
    'active_websocket_high_water',
    'rss_growth_kib',
  ]);
});

test('writeLiveAudioSummary creates deterministic machine-readable evidence', () => {
  const root = mkdtempSync(join(tmpdir(), 'voicetext-live-audio-'));
  try {
    const summary = { schema_version: 1, passed_labels: ['suspension-cleanup'] };
    const path = writeLiveAudioSummary('summary.json', summary, { root });

    assert.equal(
      path,
      join(root, 'src-tauri', 'target', 'e2e-artifacts', 'summary.json'),
    );
    assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), summary);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('assertExpectedTestRan accepts escaped cargo test names', () => {
  assertExpectedTestRan(
    'sample',
    'module::case.with[chars]',
    [
      'test module::case.with[chars] ... ok',
      'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out;',
    ].join('\n'),
    throwingFail,
  );
});

test('assertExpectedTestRan rejects zero passed tests', () => {
  assert.throws(
    () =>
      assertExpectedTestRan(
        'sample',
        'module::case',
        'test module::case ... ok\ntest result: ok. 0 passed; 0 failed;',
        throwingFail,
      ),
    /did not report at least one passed test/,
  );
});

test('assertCommandSucceeded terminates process group on non-timeout spawn errors', () => {
  const terminated = [];

  assert.throws(
    () =>
      assertCommandSucceeded(
        'sample',
        {
          error: Object.assign(new Error('stdout maxBuffer exceeded'), { code: 'ENOBUFS' }),
          pid: 123,
        },
        {
          timeoutMs: 1000,
          fail: throwingFail,
          terminate: (pid) => terminated.push(pid),
        },
      ),
    /failed to run: stdout maxBuffer exceeded/,
  );
  assert.deepEqual(terminated, [123]);
});

test('assertCommandSucceeded terminates process group on failed exit status', () => {
  const terminated = [];

  assert.throws(
    () =>
      assertCommandSucceeded(
        'sample',
        { status: 101, signal: null, pid: 456 },
        {
          timeoutMs: 1000,
          fail: throwingFail,
          terminate: (pid) => terminated.push(pid),
        },
      ),
    /failed with exit code 101/,
  );
  assert.deepEqual(terminated, [456]);
});

test('terminateProcessGroup skips Windows process groups', () => {
  let called = false;

  const terminated = terminateProcessGroup(123, {
    platform: 'win32',
    kill: () => {
      called = true;
    },
  });

  assert.equal(terminated, false);
  assert.equal(called, false);
});
