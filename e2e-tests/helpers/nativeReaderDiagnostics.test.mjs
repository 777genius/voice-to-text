import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';

// Execute the actual TEST orchestrator with mocked RPCs, without optional UI dependencies.
const source = await readFile(new URL('../../src/e2e/nativeContinuationLive.ts', import.meta.url), 'utf8');
const metricsSource = await readFile(new URL('../../src/e2e/nativeContinuationMetrics.ts', import.meta.url), 'utf8');
const metrics = await import(`data:text/javascript;base64,${Buffer.from(stripTypeScriptTypes(metricsSource)).toString('base64')}`);
const body = stripTypeScriptTypes(source.replace(/^import .*;\n/gm, '')).replace(/export /g, '');
for (const fault of ['not-armed', 'invalid-after-arm', 'third-pre-probe', 'after-pre-probes']) {
  test(`${fault}: retain native cause and stop/join with zero hotkeys/captures`, async () => {
    let calls = 0; let report; const commands = [];
    const reader = { armed: true, valid: true, stopped: false, error: null, records: [{ text: '' }] };
    const invoke = async (command, args) => {
      commands.push(command);
      if (command === 'native_e2e_state') {
        calls++;
        if (calls === ({ 'not-armed': 2, 'invalid-after-arm': 2, 'third-pre-probe': 5, 'after-pre-probes': 6 })[fault]) {
          reader.armed = fault !== 'not-armed'; reader.valid = false;
          reader.error = 'AXFocusedUIElement: native-error code=Some(-25204)';
        }
        if (args?.stopReadback) reader.stopped = true;
        return { nativeClockMs: performance.now(), nativeReadback: structuredClone(reader),
          qualificationTrial: { id: 'warm-baseline-1', episodes: ['a', 'b'] },
          fixture: { activeCaptures: 0, captureStarts: 0, observationOverflow: false },
          nativeInsertionTrace: { overflow: false, records: [] } };
      }
      if (command === 'check_accessibility_permission') return true;
      if (command === 'native_e2e_prepare_live_target') return 'owned.txt';
      if (command === 'native_e2e_finish') report = args.report;
    };
    const names = Object.keys(metrics);
    const run = new Function(...names, 'invoke', 'listen', 'useAppConfigStore', 'useTranscriptionStore',
      `${body}; return runNativeContinuationLive;`)(...Object.values(metrics), invoke, async () => () => {},
      () => ({ startSync: async () => {}, refresh: async () => {} }), () => ({ finalText: '' }));
    await run({});
    assert.equal(report.passed, false);
    assert.equal(commands.includes('native_e2e_hotkey'), false);
    assert.equal(report.final.fixture.captureStarts, 0);
    assert.equal(report.final.nativeReadback.stopped, true);
    assert.equal(report.final.nativeReadback.armed, fault !== 'not-armed');
    assert.ok(report.errors.some(e => e.includes(reader.error)));
    assert.ok(report.errors.some(e => e.includes(fault === 'not-armed' ? 'not armed' : 'reader invalid')));
    if (fault.includes('pre')) {
      assert.equal(report.clockExchanges.filter(e => e.phase === 'pre').length, 3);
      assert.ok(report.errors.some(e => e.includes('after-pre-calibration')));
    }
  });
}
