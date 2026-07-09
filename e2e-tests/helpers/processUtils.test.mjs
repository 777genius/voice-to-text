import assert from 'node:assert/strict';
import test from 'node:test';

import {
  assertTcpPortFree,
  createExpectedProcessExitTracker,
  waitForTcpPort,
} from './processUtils.mjs';

test('waitForTcpPort resolves after a later successful probe', async () => {
  let calls = 0;

  await waitForTcpPort({
    port: 4444,
    timeoutMs: 1_000,
    intervalMs: 1,
    probe: async () => {
      calls += 1;
      return calls === 3;
    },
    sleep: async () => {},
    now: () => Date.now(),
  });

  assert.equal(calls, 3);
});

test('waitForTcpPort rejects invalid ports', async () => {
  await assert.rejects(
    () => waitForTcpPort({ port: 0 }),
    /invalid TCP port: 0/,
  );
});

test('waitForTcpPort times out with host and port in the message', async () => {
  let currentTime = 1_000;

  await assert.rejects(
    () =>
      waitForTcpPort({
        host: '127.0.0.1',
        port: 4444,
        timeoutMs: 10,
        intervalMs: 5,
        probe: async () => false,
        sleep: async (ms) => {
          currentTime += ms;
        },
        now: () => currentTime,
      }),
    /timeout waiting for TCP 127\.0\.0\.1:4444 after 10ms/,
  );
});

test('assertTcpPortFree resolves when the port is not open', async () => {
  await assertTcpPortFree({
    host: '127.0.0.1',
    port: 4444,
    probe: async () => false,
  });
});

test('assertTcpPortFree rejects when the port is already open', async () => {
  await assert.rejects(
    () =>
      assertTcpPortFree({
        host: '127.0.0.1',
        port: 4444,
        probe: async () => true,
      }),
    /TCP 127\.0\.0\.1:4444 is already in use/,
  );
});

test('assertTcpPortFree rejects invalid ports', async () => {
  await assert.rejects(
    () => assertTcpPortFree({ port: -1 }),
    /invalid TCP port: -1/,
  );
});

test('createExpectedProcessExitTracker resets expected exit for a new process', () => {
  const tracker = createExpectedProcessExitTracker();

  assert.equal(tracker.isExpected(), false);

  tracker.markStopping();
  assert.equal(tracker.isExpected(), true);

  tracker.markStarted();
  assert.equal(tracker.isExpected(), false);
});
