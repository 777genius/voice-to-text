import assert from 'node:assert/strict';
import test from 'node:test';

import {
  assertCommandSucceeded,
  assertExpectedTestRan,
  readEnvOpenAiKey,
  terminateProcessGroup,
} from './liveAudioRunner.mjs';

function throwingFail(message) {
  throw new Error(message);
}

test('readEnvOpenAiKey prefers trimmed environment value', () => {
  const key = readEnvOpenAiKey({
    env: { OPENAI_API_KEY: '  sk-env-test  ' },
    exists: () => false,
  });

  assert.equal(key, 'sk-env-test');
});

test('readEnvOpenAiKey parses quoted dotenv values', () => {
  const envName = 'OPENAI' + '_API_KEY';
  const key = readEnvOpenAiKey({
    env: {},
    exists: () => true,
    readFile: () => `export ${envName} = "  sk-dotenv-test  "\n`,
  });

  assert.equal(key, 'sk-dotenv-test');
});

test('readEnvOpenAiKey strips inline comments from unquoted dotenv values', () => {
  const envName = 'OPENAI' + '_API_KEY';
  const key = readEnvOpenAiKey({
    env: {},
    exists: () => true,
    readFile: () => `${envName}=sk-dotenv-test # local key\n`,
  });

  assert.equal(key, 'sk-dotenv-test');
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
