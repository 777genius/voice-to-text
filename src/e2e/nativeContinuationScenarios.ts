/** Explicit qualification only. All capture/intent/provider work crosses native IPC. */
import type { Pinia } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useAppConfigStore } from '@/stores/appConfig';

type Control = { operation: string; logicalRunId: number; cumulativeBytes: number;
  result: { decision: string; pause_epoch: number; provider_session_id: string } };
type Snapshot = { historyEntryCount: number; status: string; sessionId: number; windowEpoch: number; visible: boolean;
  logicalProviderRunId: number;
  preparedCaptureTokenCount: number; fixture: {
    captureStarts: number; captureStops: number; activeCaptures: number; activeProviders: number;
    providerStarts: number; providerResumes: number; maxActiveProviders: number; providerAudioChunks: number;
    markerViolations: string[]; controlResults: Control[]; finals: number;
    firstPcmLatenciesMs: Array<{ captureGeneration: number; elapsedMs: number }>;
  } };
const state = () => invoke<Snapshot>('native_e2e_state');
const sleep = (durationMs: number) => invoke('native_e2e_delay', { durationMs });
function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }
async function poll(predicate: (s: Snapshot) => boolean, label: string, timeout = 10_000) {
  const deadline = performance.now() + timeout;
  do { const s = await state(); if (predicate(s)) return s; await sleep(20); }
  while (performance.now() < deadline);
  throw new Error(label);
}
async function toggle() {
  await invoke('native_e2e_hotkey', { action: 'press' });
  await invoke('native_e2e_hotkey', { action: 'release' });
}
export async function runNativeContinuationScenarios(pinia: Pinia) {
  const started = performance.now();
  const report = { mode: 'continuation-fake', passed: false, completedCycles: 0,
    errors: [] as string[], stableDeliveries: [] as Array<{ sessionId: number; deliverySeq: number;
      text: string }>, terminals: [] as Array<{ sessionId: number; complete: boolean;
      stableSnapshot: string }>, transcriptEvents: [] as Array<{ event: 'final' | 'terminal';
      sessionId: number; deliverySeq: number | null; text: string; complete: boolean | null }>,
    terminalCount: 0, logicalRunId: null as number | null,
    cycles: [] as unknown[], elapsedMs: 0, p95FirstPcmMs: -1,
    final: null as Snapshot | null };
  const unlisten = await listen('transcription:error', event => {
    report.errors.push(JSON.stringify(event.payload));
  });
  const stableKeys = new Set<string>();
  const listeners = await Promise.all(['transcription:partial', 'transcription:final', 'transcription:terminal'].map(name => listen(name, event => {
    const payload = event.payload as Record<string, unknown>;
    const sessionId = Number(payload.session_id);
    if (name === 'transcription:terminal') {
      report.terminalCount++;
      const complete = !payload.error && payload.delivery_complete === true;
      const stableSnapshot = typeof payload.stable_snapshot === 'string' ? payload.stable_snapshot : '';
      report.terminals.push({ sessionId, complete, stableSnapshot });
      report.transcriptEvents.push({ event: 'terminal', sessionId, deliverySeq: null,
        text: stableSnapshot, complete });
      if (!Number.isSafeInteger(sessionId) || sessionId <= 0 || !complete || !stableSnapshot.trim()) {
        report.errors.push('Incomplete terminal');
      }
    } else if (typeof payload.delivery_seq === 'number' && (name === 'transcription:final' || payload.is_segment_final === true)) {
      const key = `${payload.session_id}:${payload.delivery_seq}`;
      if (stableKeys.has(key)) report.errors.push('Duplicate stable delivery');
      const text = typeof payload.text === 'string' ? payload.text : '';
      stableKeys.add(key); report.stableDeliveries.push({ sessionId,
        deliverySeq: Number(payload.delivery_seq), text });
      report.transcriptEvents.push({ event: 'final', sessionId,
        deliverySeq: Number(payload.delivery_seq), text, complete: null });
      if (!Number.isSafeInteger(sessionId) || sessionId <= 0 || !Number.isSafeInteger(payload.delivery_seq) ||
          Number(payload.delivery_seq) <= 0 || !text.trim()) report.errors.push('Malformed stable delivery');
    } else if (name === 'transcription:final') {
      report.transcriptEvents.push({ event: 'final', sessionId, deliverySeq: null,
        text: typeof payload.text === 'string' ? payload.text : '', complete: null });
      report.errors.push('Final delivery is missing identity');
    }
  })));
  try {
    const config = useAppConfigStore(pinia);
    await config.startSync();
    await invoke('update_app_config', { holdToRecord: false, showMiniRecordingWindow: true,
      hideRecordingWindowOnHotkey: true, playCompletionSound: false,
      autoCopyToClipboard: false, autoPasteText: false });
    await config.refresh();
    await invoke('native_e2e_configure', { config: { audioDelayMs: 0, stopDelayMs: 0, keepAlive: false } });
    await toggle();
    const initial = await poll(s => s.fixture.providerAudioChunks > 0, 'Initial source never reached provider');
    check(Number.isSafeInteger(initial.logicalProviderRunId) && initial.logicalProviderRunId > 0,
      'Initial logical provider owner is missing');
    report.logicalRunId = initial.logicalProviderRunId;
    let previousPauseEpoch = 0;
    for (let cycle = 0; cycle < 50; cycle++) {
      const before = await state();
      await toggle();
      const paused = await poll(s => s.fixture.activeCaptures === 0 &&
        s.fixture.controlResults.filter(c => c.operation === 'pause').length === cycle + 1,
      'Stop did not release mic and negotiate Pause');
      check(paused.fixture.activeProviders === 1, 'Pause released logical provider');
      await sleep(120); // native hotkey gesture debounce; original Pause deadline is not renewed
      await toggle();
      const continued = await poll(s => s.fixture.captureStarts === before.fixture.captureStarts + 1 &&
        s.fixture.providerAudioChunks > before.fixture.providerAudioChunks &&
        s.fixture.controlResults.filter(c => c.operation === 'continue' && c.result.decision === 'accepted').length === cycle + 1,
      'Reopen did not deliver first B through accepted Continue');
      check(continued.fixture.providerStarts === initial.fixture.providerStarts &&
        continued.fixture.providerResumes === 0 && continued.fixture.maxActiveProviders === 1,
      'Continue opened another provider');
      check(continued.fixture.activeCaptures === 1 && continued.fixture.markerViolations.length === 0,
        'Capture ownership or ordered marker evidence failed');
      const controls = continued.fixture.controlResults.slice(-2);
      check(controls.length === 2 && controls[0].logicalRunId === controls[1].logicalRunId &&
        controls[0].logicalRunId === report.logicalRunId &&
        controls[0].result.pause_epoch === controls[1].result.pause_epoch,
      'Logical owner/epoch changed across Continue');
      check(controls[0].result.pause_epoch > previousPauseEpoch,
        'Pause epoch did not advance across Continue cycles');
      previousPauseEpoch = controls[0].result.pause_epoch;
      report.cycles.push({ cycle, captureGeneration: continued.fixture.firstPcmLatenciesMs[continued.fixture.firstPcmLatenciesMs.length - 1]?.captureGeneration,
        windowEpoch: continued.windowEpoch, providerStarts: continued.fixture.providerStarts,
        micOffOnStop: paused.fixture.activeCaptures === 0, controls });
      report.completedCycles++;
      await invoke('native_e2e_progress', { report: { mode: report.mode, cycle, errors: report.errors } });
      check(report.errors.length === 0, 'Native error during rapid cycles');
      let closedAgain = false;
      const observer = new MutationObserver(changes => {
        closedAgain ||= !!document.querySelector('.mini-closing') || changes.some(change =>
          (change.oldValue || '').split(/\s+/).includes('mini-closing'));
      });
      observer.observe(document.documentElement, { subtree: true, attributes: true, attributeFilter: ['class'], attributeOldValue: true });
      try { await sleep(120); } finally { observer.disconnect(); }
      check(!closedAgain && (await state()).visible, 'Continued window closed or replayed its close animation');
    }
    await toggle();
    const final = await poll(s => s.fixture.activeCaptures === 0 && s.fixture.activeProviders === 0 &&
      s.preparedCaptureTokenCount === 0 && s.historyEntryCount === 1 && report.terminalCount === 1, 'Terminal cleanup did not release resources');
    report.final = final;
    check(final.fixture.captureStarts === final.fixture.captureStops && final.fixture.finals === 1,
      'Logical run did not finalize exactly once');
    const latencies = final.fixture.firstPcmLatenciesMs.map(x => x.elapsedMs).sort((a,b) => a-b);
    report.p95FirstPcmMs = latencies[Math.ceil(latencies.length * .95) - 1];
    check(latencies.length >= 51 && report.p95FirstPcmMs <= 250, 'Native Start-to-firstPCM gate failed');
    const stableDelivery = report.stableDeliveries[0];
    const terminal = report.terminals[0];
    const expectedTranscript = stableDelivery
      ? `Native fixture session ${stableDelivery.sessionId}` : '';
    check(report.stableDeliveries.length === 1 && report.terminals.length === 1 &&
      report.transcriptEvents.length === 2 && report.transcriptEvents[0]?.event === 'final' &&
      report.transcriptEvents[1]?.event === 'terminal' &&
      Number.isSafeInteger(stableDelivery?.deliverySeq) && Number(stableDelivery?.deliverySeq) > 0 &&
      stableDelivery?.sessionId === terminal?.sessionId && terminal?.complete === true &&
      stableDelivery?.text === expectedTranscript && terminal?.stableSnapshot === expectedTranscript &&
      final.historyEntryCount === 1,
    'Stable delivery/history ownership must occur once');
    check(report.errors.length === 0, 'Error evidence prevents passing');
    report.passed = true;
  } catch (error) { report.errors.push(String(error)); report.passed = false; }
  finally { unlisten(); listeners.forEach(stop => stop()); }
  report.elapsedMs = performance.now() - started;
  await invoke('native_e2e_finish', { report });
}
