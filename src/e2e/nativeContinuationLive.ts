import { clockCalibration, syntheticReadbackMetrics, type ClockExchange, type NativeReadback, nativeInsertionMetrics, boundedSyntheticText, syntheticPhraseOccurrences, type NativeInsertionTrace, continuationMetrics, type ContinuationObservation, type ContinuationEpisode } from './nativeContinuationMetrics';
import type { Pinia } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useAppConfigStore } from '@/stores/appConfig';
import { useTranscriptionStore } from '@/stores/transcription';

type Trial = { id: string; continuation: boolean; configDelayMs: number; episodes: string[] };
type Native = { nativeReadback: NativeReadback; providerTransport: { serverReady: boolean; connectionRetained: boolean } | null; nativeClockMs: number; nativeInsertionTrace: NativeInsertionTrace; logicalProviderRunId: number; captureEpisode: unknown; pausedContinuation: unknown;
  completedReport: unknown; historyEntryCount: number; status: string; sessionId: number; preparedCaptureTokenCount: number;
  qualificationTrial: Trial; qualificationEndpoint: string;
  fixture: { observationOverflow: boolean; activeCaptures: number; captureStarts: number; captureStops: number;
    sourceEpisodes: Array<{ name: string; bytes: number; sourceFrames: number; emittedFrames: number;
      captureGeneration: number; sourceDurationMs: number; providerSourceOffsetMs?: number; nativeSourceStartMs: number; nativeSourceEndMs: number; sourceGateRequired: boolean; sourceGateReady: { serverReady: boolean; status: string; nativeReadyMs: number; emittedFrames: number } | null; lastSourceFrameElapsedMs: number }> } };
const state = (stopReadback = false) => invoke<Native>('native_e2e_state', { stopReadback });
const wait = (durationMs: number) => invoke('native_e2e_delay', { durationMs });
async function toggle() {
  await invoke('native_e2e_hotkey', { action: 'press' });
  await invoke('native_e2e_hotkey', { action: 'release' });
}
function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }
// RunTerminalPayload.session_id is run_id.get() in commands.rs, not the UI capture session.
export function verifyLiveTerminals(continuation: boolean, baseline: boolean,
  episodes: Array<{ logicalRunId?: number }>, terminals: Array<{ sessionId: number; complete: boolean }>) {
  const owners = episodes.map(e => e.logicalRunId);
  const distinct = new Set(owners);
  check(owners.length === 2 && owners.every(id => Number.isSafeInteger(id) && Number(id) > 0) &&
    distinct.size === (baseline || continuation ? 1 : 2), 'Expected distinct logical run ownership');
  check(terminals.length === distinct.size && terminals.every(t => t.complete && distinct.has(t.sessionId)) &&
    [...distinct].every(id => terminals.filter(t => t.sessionId === id).length === 1),
    'Exactly one complete terminal per expected logical run required');
}
// Shared TEST preflight; diagnostic uses the active Pinia and a refusing fixture transport.
export async function nativeLivePreflight(pinia: Pinia | undefined, endpoint: string,
  subscriptions: Array<() => void>, observe: (name: string, payload: Record<string, unknown>) => void,
  options: { keepAlive?: boolean; autoPasteText?: boolean } = {}) {
  for (const name of ['transcription:partial', 'transcription:final', 'transcription:terminal', 'transcription:error']) {
    subscriptions.push(await listen(name, event => observe(name, event.payload as Record<string, unknown>)));
  }
  const config = useAppConfigStore(pinia);
  const store = useTranscriptionStore(pinia);
  await config.startSync();
  await invoke('update_app_config', { showMiniRecordingWindow: true, holdToRecord: false,
    hideRecordingWindowOnHotkey: true, playCompletionSound: false,
    autoCopyToClipboard: false, autoPasteText: options.autoPasteText ?? true });
  await config.refresh();
  await invoke('native_e2e_configure', { config: { qualificationEndpoint: endpoint,
    keepAlive: options.keepAlive ?? false, audioDelayMs: 0, stopDelayMs: 0 } });
  check(await invoke<boolean>('check_accessibility_permission'), 'Accessibility unavailable');
  return store;
}
export async function runNativeContinuationLive(pinia: Pinia) {
  const started = performance.now();
  const report = { mode: 'continuation-live', passed: false, trial: null as Trial | null,
    targetDocument: '', expectedInsertion: '', actualPasteVerified: false,
    eventOverflow: false, terminals: [] as Array<{ sessionId: number; complete: boolean }>,
    syntheticMarkerInterpretation: 'offline synthetic phrase detection only; occurrence ordinals are per raw event, not causal run identity; no word alignment or earliest-B latency proven',
    syntheticMarkers: [] as Array<{ eventId: number; sessionId: number; deliverySeq: number | null; atMs: number; occurrences: ReturnType<typeof syntheticPhraseOccurrences> }>,
    preparation: [] as Array<{ phase: string; atMs: number }>,
    clockExchanges: [] as ClockExchange[], clockReadIntervals: [] as Array<[number, number]>,
    calibration: null as ReturnType<typeof clockCalibration> | null,
    osReadback: null as ReturnType<typeof syntheticReadbackMetrics> | null,
    latencyQualification: 'not proven; instrumentation does not satisfy the causal attribution or comparison gate',
    nativeInsertions: null as ReturnType<typeof nativeInsertionMetrics> | null,
    errors: [] as string[], events: [] as Array<ContinuationObservation & ReturnType<typeof boundedSyntheticText>>, episodes: [] as ContinuationEpisode[],
    metrics: [] as ReturnType<typeof continuationMetrics>,
    final: null as Native | null, elapsedMs: 0, clock: 'webview-performance-now' };
  const now = () => performance.now() - started;
  const subscriptions: Array<() => void> = [];
  const calibrate = async (phase: 'pre' | 'post') => {
    for (let i = 0; i < 3; i++) {
      const w0 = now(); const value = await state(); const w1 = now();
      report.clockExchanges.push({ phase, w0, n: value.nativeClockMs, w1 });
      report.clockReadIntervals.push([w0 - value.nativeClockMs, w1 - value.nativeClockMs]);
    }
  };
  try {
    const initial = await state(); report.trial = initial.qualificationTrial;
    check(report.trial?.episodes.length === 2, 'Missing fixed trial');
    const store = await nativeLivePreflight(pinia, initial.qualificationEndpoint, subscriptions, (name, payload) => {
        if (report.events.length >= 512) { report.eventOverflow = true; return; }
        const syntheticText = boundedSyntheticText(payload.text);
        if (!syntheticText.syntheticTextValid) report.eventOverflow = true;
        const occurrences = syntheticPhraseOccurrences(syntheticText.rawSyntheticText ?? '');
        if (occurrences.length) report.syntheticMarkers.push({ eventId: report.events.length, sessionId: Number(payload.session_id),
          deliverySeq: typeof payload.delivery_seq === 'number' ? payload.delivery_seq : null, atMs: now(), occurrences });
        report.events.push({ ...syntheticText, event: name, atMs: now(), sessionId: Number(payload.session_id),
          deliverySeq: typeof payload.delivery_seq === 'number' ? payload.delivery_seq : null,
          nonempty: typeof payload.text === 'string' && payload.text.trim().length > 0,
          stable: name === 'transcription:final' || payload.is_segment_final === true,
          timingKnown: payload.timing_known === true, sourceStartSeconds: Number(payload.start),
          sourceDurationSeconds: Number(payload.duration) });
        if (name === 'transcription:terminal') report.terminals.push({ sessionId: Number(payload.session_id), complete: payload.delivery_complete === true && !payload.error });
        if (name === 'transcription:terminal' && (payload.error || payload.delivery_complete !== true)) report.errors.push('Incomplete terminal delivery');
        if (name === 'transcription:error') report.errors.push(JSON.stringify(payload));
    });
    report.preparation.push({ phase: 'prepare-start', atMs: now() });
    report.targetDocument = await invoke<string>('native_e2e_prepare_live_target');
    await wait(500);
    const armed = await state();
    const requireReader = (value: Native, phase: string) => {
      report.preparation.push({ phase, atMs: now() });
      const reader = value.nativeReadback;
      check(reader?.armed, `${phase}: owned OS reader not armed: ${reader?.error ?? 'no native error'}`);
      check(reader.valid, `${phase}: owned OS reader invalid: ${reader.error ?? 'no native error'}`);
      check(reader.records[0]?.text === '', `${phase}: owned OS reader initial text not empty`);
    };
    requireReader(armed, 'after-500ms');
    report.preparation.push({ phase: 'pre-calibration-start', atMs: now() });
    await calibrate('pre');
    requireReader(await state(), 'after-pre-calibration');
    const poll = async (accept: (value: Native) => boolean, label: string, timeoutMs: number) => {
      const deadline = performance.now() + timeoutMs;
      let lastHeartbeat = 0;
      do {
        const value = await state();
        if (accept(value)) return value;
        if (performance.now() - lastHeartbeat > 2000) {
          await invoke('native_e2e_progress', { report: { trial: report.trial?.id, phase: label, atMs: now() } });
          lastHeartbeat = performance.now();
        }
        await wait(20);
      } while (performance.now() < deadline);
      throw new Error(label);
    };
    const baseline = report.trial.id.startsWith('warm-baseline-');
    const gated = baseline || report.trial.continuation;
    const initialHistoryCount = initial.historyEntryCount;
    const preRunText = store.finalText;
    for (let episode = 0; episode < 2; episode++) {
      const startMs = now();
      const previousText = store.finalText;
      if (!baseline || episode === 0) await toggle();
      if (episode === 0 && gated) {
        const ready = await poll(s => s.status === 'Recording' && s.providerTransport?.serverReady === true && s.providerTransport.connectionRetained === true, 'initial actual Server Ready', 30000);
        check(ready.fixture.sourceEpisodes.every(row => row.emittedFrames === 0), 'Source escaped initial Ready gate');
        await invoke('native_e2e_configure', { config: { sourceGateReady: true } });
      }
      const complete = await poll(s => {
        const row = s.fixture.sourceEpisodes[episode];
        return !!row && row.emittedFrames === row.sourceFrames && typeof row.nativeSourceEndMs === 'number';
      }, 'complete source capture', 70000);
      const source = complete.fixture.sourceEpisodes[episode];
      check(source.name === report.trial.episodes[episode], 'Unplanned source episode');
      const sourceCompleteMs = now();
      if (baseline && episode === 0) {
        report.episodes.push({ episode, startMs, sourceCompleteMs, micReleasedMs: null,
          logicalRunId: complete.logicalProviderRunId, source,
          nativeEvidence: { captureEpisode: complete.captureEpisode, pausedContinuation: null } });
        continue;
      }
      await toggle();
      const stopped = await poll(s => s.fixture.activeCaptures === 0, 'mic release', 1000);
      report.episodes.push({ episode, startMs, sourceCompleteMs, micReleasedMs: now(),
        logicalRunId: complete.logicalProviderRunId, source,
        nativeEvidence: { captureEpisode: complete.captureEpisode, pausedContinuation: stopped.pausedContinuation } });
      if (episode === 0 && report.trial.continuation) {
        await wait(120);
      } else {
        await poll(s => s.status === 'Idle', 'terminal drain', 35000);
        await wait(1000);
        check(store.finalText.length > 0 && store.finalText !== (baseline ? preRunText : previousText), 'No stable transcript');
        report.expectedInsertion += store.finalText;
        await wait(120);
      }
    }
    report.final = await state();
    check(report.final.fixture.captureStarts === (baseline ? 1 : 2) && report.final.fixture.captureStops === (baseline ? 1 : 2) &&
      report.final.fixture.activeCaptures === 0 && report.final.preparedCaptureTokenCount === 0, 'Capture cleanup failed');
    check(report.final.fixture.sourceEpisodes.length === 2, 'Source episode count mismatch');
    if (gated) {
      const first = report.final.fixture.sourceEpisodes[0];
      check(first.sourceGateRequired && first.sourceGateReady?.status === 'Recording' && first.sourceGateReady.serverReady === true &&
        first.sourceGateReady.emittedFrames === 0 && first.nativeSourceStartMs >= first.sourceGateReady.nativeReadyMs,
        'Missing real Ready source gate evidence');
    }
    check(report.final.historyEntryCount === initialHistoryCount + (report.trial.continuation || baseline ? 1 : 2), 'History count mismatch');
    check(report.final.status === 'Idle' && report.final.providerTransport?.connectionRetained === false, 'Normal Stop retained native connection');
    check(report.errors.length === 0, 'Retained transcription error');
    // Runner must additionally verify exact external document and provider connection evidence.
    report.passed = true;
  } catch (error) { report.errors.push(String(error)); report.passed = false; }
  finally {
    // Stop also aborts a pending source gate/timer on an error path.
    try {
      const current = await state();
      if (current.fixture.activeCaptures > 0) await invoke('stop_recording');
    } catch (error) { report.errors.push(String(error)); report.passed = false; }
    subscriptions.forEach(unlisten => unlisten());
    try { await calibrate('post'); } catch (error) { report.errors.push(String(error)); report.passed = false; }
    try { report.final = await state(true); } catch (error) { report.errors.push(String(error)); report.passed = false; }
  }
  if (report.passed && report.trial) {
    try {
      verifyLiveTerminals(report.trial.continuation, report.trial.id.startsWith('warm-baseline-'), report.episodes, report.terminals);
      check(report.errors.length === 0, 'Retained transcription error');
    } catch (error) { report.errors.push(String(error)); report.passed = false; }
  }
  if (report.final) report.nativeInsertions = nativeInsertionMetrics(report.final.nativeInsertionTrace, report.events, report.eventOverflow || report.final.fixture.observationOverflow);
  if (report.eventOverflow || report.final?.nativeInsertionTrace.overflow || report.final?.fixture.observationOverflow) { report.passed = false; report.errors.push('Observation overflow invalidates evidence'); }
  report.calibration = clockCalibration(report.clockExchanges);
  if (report.calibration.error) { report.passed = false; report.errors.push(report.calibration.error); }
  if (!report.final?.nativeReadback?.valid || !report.final.nativeReadback.stopped) {
    report.passed = false; report.errors.push('OS readback invalid or not joined');
  } else report.osReadback = syntheticReadbackMetrics(report.final.nativeReadback, report.episodes, report.expectedInsertion);
  report.metrics = continuationMetrics(report.episodes, report.events, report.clockExchanges);
  report.elapsedMs = now();
  await invoke('native_e2e_finish', { report });
}
