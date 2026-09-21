import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { liveTrials, verifyQualificationConnections } from './nativeContinuation.mjs';

// Evaluate the exact collector source without importing the runner's optional jsdom dependency.
const runner = await readFile(new URL('../run-native-window-e2e.mjs', import.meta.url), 'utf8');
const source = runner.slice(runner.indexOf('export function createQualificationCollector('),
  runner.indexOf('export function assertOwnedProcessGroupGone('));
assert.ok(source.startsWith('export function createQualificationCollector('));
const createQualificationCollector = new Function('verifyQualificationConnections', 'marker',
  `${source.replace('export function', 'function')}; return createQualificationCollector;`)(
  verifyQualificationConnections, 'VOICETEXT_NATIVE_WINDOW_E2E_V1');
const close = code => ({ event: 'fault_proxy_close', direction: 'upstream', code });
const eventsFor = trial => Array.from({ length: trial.id.startsWith('cold-') ? 2 : 1 },
  () => [{ event: 'fault_proxy_connected' }, close(1000)]).flat();
const boundary = () => ({ event: 'qualification_pre_teardown', atMs: 10, clock: 'runner-performance-now', nativeProcessAlive: true });

test('all planned modes reject every retained backend error and unclean upstream closure', () => {
  for (const trial of liveTrials) {
    verifyQualificationConnections(trial, [...eventsFor(trial), boundary()]);
    const emptyCloseEvents = eventsFor(trial).map(event => event.event === 'fault_proxy_close' ? close(1005) : event);
    verifyQualificationConnections(trial, [...emptyCloseEvents, boundary()]);
    for (const code of [undefined, '1000', 1001, 1006, 1011]) {
      const events = eventsFor(trial); events[1] = close(code);
      assert.throws(() => verifyQualificationConnections(trial, [...events, boundary()]), /Unclean upstream close/);
    }
    for (const details of [{}, { code: 'INTERNAL_ERROR' }, { code: 'UNKNOWN' }]) {
      for (const index of [0, 1, eventsFor(trial).length + 1]) {
        const events = [...eventsFor(trial), boundary()];
        events.splice(index, 0, { event: 'backend_control', type: 'error', ...details });
        assert.throws(() => verifyQualificationConnections(trial, events), /Retained backend error/);
      }
    }
  }
});

for (const fault of ['backend-error', 'abnormal-close', 'none']) {
  test(`actual collector otherwise-PASS native envelope: ${fault}`, async () => {
    const trial = liveTrials[0];
    const events = eventsFor(trial);
    if (fault === 'backend-error') events.splice(1, 0, { event: 'backend_control', type: 'error', code: 'INTERNAL_ERROR' });
    if (fault === 'abnormal-close') events[1] = close(1006);
    let now = 0;
    const collector = createQualificationCollector(trial, events, async () => ({
      marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', passed: true, report: { trial, passed: true, errors: [] },
      preTeardown: { normalStopReleased: true, cleanupDeferredToRunner: true },
    }), () => now);
    if (fault === 'none') {
      assert.equal(await collector(() => true), true);
      assert.equal(events.at(-1).event, 'qualification_pre_teardown');
    } else {
      assert.equal(await collector(() => true), false);
      now = 5000;
      await assert.rejects(collector(() => true), /Retained backend error|Unclean upstream close/);
      assert.equal(events.some(e => e.event === 'qualification_pre_teardown'), false);
    }
  });
}
