import { nativeLivePreflight } from './nativeContinuationLive';
import { invoke } from '@tauri-apps/api/core';
import type { NativeReadback, ClockExchange } from './nativeContinuationMetrics';

type Snapshot = { readerPreparation: boolean; diagnosticEffectRefused: boolean; qualificationEndpoint: string; nativeClockMs: number; nativeReadback: NativeReadback };
/** Same live preflight, followed by observation only; native effects hard refuse. */
export async function runNativeReaderPreparation() {
  const report = { mode: 'reader-preparation', qualificationPassed: false, passed: false,
    targetDocument: '', preflightComplete: false, unexpectedEvents: 0, interpretation: 'nonreproduction is inconclusive; observation is not causal evidence', errors: [] as string[], clockExchanges: [] as ClockExchange[],
    preparation: [] as Array<{ phase: string; atMs: number }>,
    final: null as Snapshot | null, observationMs: 0 };
  const subscriptions: Array<() => void> = [];
  const state = (stopReadback = false) => invoke<Snapshot>('native_e2e_state', { stopReadback });
  const check = (ok: unknown, reason: string) => { if (!ok) throw new Error(reason); };
  const reader = (value: Snapshot, phase: string) => {
    report.preparation.push({ phase, atMs: performance.now() });
    check(!value.diagnosticEffectRefused && report.unexpectedEvents === 0, `${phase}: forbidden diagnostic effect/event`);
    const r = value.nativeReadback;
    check(value.readerPreparation && r?.armed, `${phase}: owned reader not armed: ${r?.error ?? 'missing evidence'}`);
    check(r.valid && !r.error, `${phase}: owned reader invalid: ${r.error ?? 'missing evidence'}`);
    check(r.records[0]?.text === '', `${phase}: owned reader initial text not empty`);
  };
  try {
    const initial = await state();
    check(initial.readerPreparation, 'Unpaid preparation mode required');
    await nativeLivePreflight(undefined, initial.qualificationEndpoint, subscriptions, () => { report.unexpectedEvents++; });
    check(report.unexpectedEvents === 0, 'Unexpected transcription event during preflight');
    report.preflightComplete = true;
    report.preparation.push({ phase: 'prepare-start', atMs: performance.now() });
    report.targetDocument = await invoke<string>('native_e2e_prepare_live_target');
    await invoke('native_e2e_delay', { durationMs: 500 });
    reader(await state(), 'after-500ms');
    let lo = -Infinity, hi = Infinity;
    for (let i = 0; i < 3; i++) {
      const w0 = performance.now(), value = await state(), w1 = performance.now();
      const p = { phase: 'pre' as const, w0, n: value.nativeClockMs, w1 };
      report.clockExchanges.push(p);
    }
    for (const [i, p] of report.clockExchanges.entries()) {
      const previous = report.clockExchanges[i - 1];
      check([p.w0, p.w1, p.n].every(Number.isFinite) && p.n >= 0 && p.w0 >= 0 && p.w1 >= p.w0 &&
        (!previous || (p.w0 >= previous.w1 && p.n >= previous.n)), 'Invalid pre clock probes');
      lo = Math.max(lo, p.w0 - p.n); hi = Math.min(hi, p.w1 - p.n);
      check(Number.isFinite(lo) && Number.isFinite(hi) && lo <= hi, 'Contradictory pre clock probes');
    }
    const fresh = await state(); reader(fresh, 'after-pre-calibration');
    await invoke('native_e2e_delay', { durationMs: 5000 });
    const observed = await state(); reader(observed, 'after-observation');
    report.observationMs = observed.nativeClockMs - fresh.nativeClockMs;
    check(Number.isFinite(report.observationMs) && report.observationMs >= 5000, 'Observation shorter than 5s');
    report.passed = true;
  } catch (error) { report.errors.push(String(error)); }
  finally {
    try {
      report.final = await state(true);
      reader(report.final, 'after-stop');
      check(report.final.nativeReadback.stopped, 'Reader did not stop');
    } catch (error) { report.errors.push(String(error)); }
    subscriptions.forEach(unlisten => unlisten());
    if (report.unexpectedEvents) report.errors.push('Unexpected transcription event during diagnostic');
    report.passed = report.passed && report.errors.length === 0;
    await invoke('native_e2e_finish', { report });
  }
}
