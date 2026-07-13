import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  assertCommandSucceeded,
  assertExpectedTestRan,
  parsePositiveIntegerEnv,
  resolvePaidE2eEnvironment,
  sanitizedAudioTestEnvironment,
  terminateProcessGroup,
  writeLiveAudioSummary,
} from './liveAudioRunner.mjs';

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

test('parsePositiveIntegerEnv accepts only complete positive integer strings', () => {
  assert.equal(parsePositiveIntegerEnv('600', 120), 600);
  assert.equal(parsePositiveIntegerEnv(' 90 ', 120), 90);
  assert.equal(parsePositiveIntegerEnv('5m', 120), 120);
  assert.equal(parsePositiveIntegerEnv('0', 120), 120);
  assert.equal(parsePositiveIntegerEnv('-5', 120), 120);
  assert.equal(parsePositiveIntegerEnv('', 120), 120);
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
