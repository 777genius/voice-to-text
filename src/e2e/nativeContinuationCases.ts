import type { Pinia } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useAppConfigStore } from '@/stores/appConfig';
type Control = { operation: string; delivered: boolean; result: { decision: string; pause_epoch?: number | null } };
type State = { sessionId: number; preparedCaptureTokenCount: number; fixture: {
  activeCaptures: number; activeProviders: number; captureStarts: number;
  providerStarts: number; providerAudioChunks: number; controlResults: Control[]; firstBWrites: unknown[] } };
const state = () => invoke<State>('native_e2e_state');
const sleep = (durationMs: number) => invoke('native_e2e_delay', { durationMs });
const press = () => invoke('native_e2e_hotkey', { action: 'press' });
const release = () => invoke('native_e2e_hotkey', { action: 'release' });
async function toggle() { await press(); await release(); }
function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }
async function poll(predicate: (s: State) => boolean, label: string, timeout = 10000) {
  const deadline = performance.now() + timeout;
  do { const s = await state(); if (predicate(s)) return s; await sleep(10); }
  while (performance.now() < deadline);
  throw new Error(label);
}
/** A complete native production intent path with a delayed fake control response.
 * Each invocation owns one A/B pair, avoiding cross-case resets of logical state. */
export async function runNativeContinuationCase(pinia: Pinia, selected: string) {
  if ((afterWriteCases as readonly string[]).includes(selected)) return runAfterWriteCase(pinia, selected);
  if ((nativeEventCases as readonly string[]).includes(selected)) return runNativeEventCase(pinia, selected);
  const report = { mode: 'continuation-case', case: selected, passed: false,
    errors: [] as string[], micReleasedBeforeAccepted: false, cleanup: false,
    restored: false, fallbackAfterRefusal: false, firstBWrites: -1, final: null as State | null };
  const unlisten = await listen('transcription:error', event => report.errors.push(JSON.stringify(event.payload)));
  try {
    check(['seal-stop', 'seal-hold', 'seal-close', 'seal-toggle', 'stale-epoch', 'terminal-before-write'].includes(selected), 'Unknown case');
    const config = useAppConfigStore(pinia); await config.startSync();
    const hold = selected === 'seal-hold';
    await invoke('update_app_config', { holdToRecord: hold, showMiniRecordingWindow: true,
      hideRecordingWindowOnHotkey: true, autoCopyToClipboard: false, autoPasteText: false,
      playCompletionSound: false });
    await config.refresh();
    await invoke('native_e2e_configure', { config: { audioDelayMs: 0, stopDelayMs: 0,
      keepAlive: false, controlDelayMs: 600 } });
    if (hold) await press(); else await toggle();
    await poll(s => s.fixture.providerAudioChunks > 0, 'A did not reach production writer');
    await sleep(120);
    if (hold) await release(); else await toggle();
    await poll(s => s.fixture.activeCaptures === 0 && s.fixture.controlResults.some(c =>
      c.operation === 'pause' && c.delivered && c.result.decision === 'accepted'), 'Pause failed');
    await sleep(120);
    if (hold) await press(); else await toggle();
    await poll(s => s.fixture.captureStarts === 2 && s.fixture.controlResults.some(c =>
      c.operation === 'continue' && !c.delivered && c.result.decision === 'accepted'), 'Delayed Continue never entered');
    const fallback = selected === 'stale-epoch' || selected === 'terminal-before-write';
    if (fallback) {
      const replacement = await poll(s => s.fixture.providerStarts === 2 && s.fixture.activeCaptures === 1 &&
        s.fixture.controlResults.some(c => c.operation === 'continue' && c.delivered), 'Refused B never reached sequential cold fallback');
      check(replacement.fixture.firstBWrites.length === 0, 'Stale acceptance wrote B on original provider');
      report.fallbackAfterRefusal = true;
      await sleep(150);
      await invoke('stop_recording');
      await poll(s => s.fixture.activeCaptures === 0, 'Fallback mic not released', 1000);
    } else {
    // Retain short B PCM before the Stop intent. Stay inside the 600ms response delay.
    await sleep(130);
    if (selected === 'seal-hold') await release();
    else if (selected === 'seal-close') await invoke('native_e2e_close_recording');
    else if (selected === 'seal-toggle') await toggle();
    else await invoke('stop_recording');
    const stopped = await poll(s => s.fixture.activeCaptures === 0, 'B mic not released promptly', 400);
    report.micReleasedBeforeAccepted = stopped.fixture.controlResults.some(c => c.operation === 'continue' && !c.delivered);
    check(report.micReleasedBeforeAccepted, 'Stop waited for Accepted before releasing mic');
    }
    const final = await poll(s => s.fixture.activeCaptures === 0 && s.fixture.activeProviders === 0 &&
      s.preparedCaptureTokenCount === 0, 'Terminal resources retained');
    report.final = final; report.cleanup = true;
    report.firstBWrites = final.fixture.firstBWrites.length;
    report.restored = final.fixture.controlResults.some(c => c.operation === 'restore');
    check(fallback ? report.firstBWrites === 0 : report.firstBWrites === 1 && !report.restored,
      'Sealed B must drain once without Restore');
    check(report.errors.length === 0, 'Native errors prevent passing');
    report.passed = true;
  } catch (error) { report.errors.push(String(error)); }
  finally { unlisten(); }
  await invoke('native_e2e_finish', { report });
}

export const nativeEventCases = ['E04', 'E41', 'E42'] as const;
type EventState = State & { nativeClockMs: number; status: string;
  physicalKeyboard?: { source: string; realKeyboardReads: number; overflow: boolean;
    observations: Array<{ key: number; modifiers: number; observation: string; source: string; downKeys: number[] }>;
    events: Array<{ kind: string; sample: number | null; watcherFinished: boolean | null; handle: { gesture: number; watcher: number } | null; observation: string; result: string }> };
  captureEpisode: { runId: number; generation: number } | null;
  coordinatorTrace: Array<{ sequence: number; phase: string; source: string; reason: string; gesture: number | null; runId: number | null; desiredAfter: string; captureAfter?: string }>;
  fixture: State['fixture'] & { maxActiveCaptures: number; captureStops: number; providerResumes: number;
    captureEvents: Array<{ kind: string; generation: number; atMs: number }>;
    markerViolations: string[]; observationOverflow: boolean;
    providerMarkers: Array<{ captureGeneration: number; firstSequence: number; lastSequence: number; count: number; captureRunId: number; captureFenceGeneration: number; providerSessionId: number }>;
  } };
export function validE42PhysicalEvidence(s: EventState): boolean {
  const p = s.physicalKeyboard;
  if (!p || p.source !== 'fake' || p.realKeyboardReads !== 0 || p.overflow ||
    p.observations.length > 256 || p.events.length > 128 ||
    p.observations.some(o => o.source !== 'fake' || o.key !== 7 || o.modifiers !== 3 ||
      o.observation !== (o.downKeys.includes(7) && (o.downKeys.includes(55) || o.downKeys.includes(54)) &&
        (o.downKeys.includes(56) || o.downKeys.includes(60)) ? 'Down' : 'Up'))) return false;
  if (p.events.some(e => e.kind === 'watcher' ? e.watcherFinished !== true : e.watcherFinished !== null)) return false;
  if (p.events.some(e => e.observation === 'NotRead' ? e.sample !== null :
    !Number.isInteger(e.sample) || e.sample == null || e.sample < 0 ||
    p.observations[e.sample]?.observation !== e.observation)) return false;
  const accepted = p.events.filter(e => e.kind === 'pressed' && e.result === 'Accepted');
  if (accepted.length !== 2 || accepted.some(e => e.observation !== 'Down' || !e.handle ||
    !Number.isSafeInteger(e.handle.gesture) || e.handle.gesture <= 0 ||
    !Number.isSafeInteger(e.handle.watcher) || e.handle.watcher <= 0)) return false;
  const [a, b] = accepted;
  if (!s.coordinatorTrace.some(t => t.source === 'Some(HoldHotkey)' &&
    t.gesture === b.handle!.gesture && t.phase === 'IntentApplied' && t.desiredAfter === 'Off')) return false;
  if (a.handle!.gesture === b.handle!.gesture || a.handle!.watcher === b.handle!.watcher) return false;
  const matches = (e: typeof a, owner: typeof a, kind: string, observation: string, result: string) =>
    e.kind === kind && e.observation === observation && e.result === result &&
    e.handle?.gesture === owner.handle!.gesture && e.handle?.watcher === owner.handle!.watcher;
  const rearm = p.events.findIndex(e => matches(e, a, 'watcher', 'Up', 'Rearmed'));
  const duplicate = p.events.findIndex(e => matches(e, a, 'pressed', 'Down', 'Duplicate'));
  const ignored = p.events.findIndex(e => matches(e, b, 'released', 'Down', 'Stale'));
  const ended = p.events.findIndex(e => matches(e, b, 'watcher', 'Up', 'HoldEnded'));
  const samples = p.events.flatMap(e => e.sample == null ? [] : [e.sample]);
  if (samples.some((sample, i) => i > 0 && sample < samples[i - 1]) ||
    duplicate <= p.events.indexOf(a) || duplicate >= rearm ||
    ignored <= p.events.indexOf(b) || ended <= ignored) return false;
  return rearm > p.events.indexOf(a) && rearm < p.events.indexOf(b) &&
    p.events.some(e => matches(e, a, 'pressed', 'Down', 'Duplicate')) &&
    p.events.slice(p.events.indexOf(b) + 1).some(e => matches(e, a, 'watcher', 'NotRead', 'Stale')) &&
    p.events.slice(p.events.indexOf(b) + 1).some(e => matches(e, b, 'released', 'Down', 'Stale')) &&
    p.events.slice(p.events.indexOf(b) + 1).some(e => matches(e, b, 'watcher', 'Up', 'HoldEnded')) &&
    p.observations.filter(o => o.observation === 'Down').length >= 2 &&
    p.observations.filter(o => o.observation === 'Up').length >= 2;
}

function markerFor(s: EventState) {
  const rows = s.fixture.providerMarkers.filter(m => m.captureRunId === s.captureEpisode?.runId &&
    m.captureFenceGeneration === s.captureEpisode?.generation);
  return rows.length === 1 ? rows[0] : undefined;
}
function sameMarkerOwner(a: EventState, b: EventState) {
  const x = markerFor(a), y = markerFor(b);
  return !!x && !!y && x.captureGeneration === y.captureGeneration && x.captureRunId === y.captureRunId &&
    x.captureFenceGeneration === y.captureFenceGeneration && x.providerSessionId === y.providerSessionId &&
    x.firstSequence === y.firstSequence;
}
function markerAdvances(before: EventState, after: EventState) {
  const a = markerFor(before), b = markerFor(after);
  return !!a && !!b && a.captureGeneration === b.captureGeneration && a.captureRunId === b.captureRunId &&
    a.captureFenceGeneration === b.captureFenceGeneration && a.providerSessionId === b.providerSessionId &&
    a.firstSequence === b.firstSequence && b.count > a.count && b.lastSequence > a.lastSequence &&
    b.count - a.count === b.lastSequence - a.lastSequence;
}
function currentStopApplied(before: EventState, after: EventState, source: string) {
  const last = before.coordinatorTrace.slice(-1)[0]?.sequence ?? 0;
  const gesture = before.coordinatorTrace.filter(t => t.source === 'Some(HoldHotkey)' && t.phase === 'IntentApplied' && t.desiredAfter.startsWith('On') && t.gesture != null).slice(-1)[0]?.gesture;
  // The service retains the sealed episode while the logical provider is paused.
  // Its identity is historical ownership, not proof that the microphone is active.
  const episode = before.captureEpisode, marker = markerFor(before);
  if (!episode || !marker || before.fixture.activeCaptures !== 1 ||
      (after.captureEpisode != null && (after.captureEpisode.runId !== episode.runId ||
        after.captureEpisode.generation !== episode.generation))) return false;
  const events = after.fixture.captureEvents.filter(e => e.generation === marker.captureGeneration);
  const released = events.filter(e => e.kind === 'capture-off');
  const joined = events.filter(e => e.kind === 'capture-joined');
  if (!Number.isFinite(before.nativeClockMs) || !Number.isFinite(after.nativeClockMs) ||
      released.length !== 1 || joined.length !== 1 ||
      !Number.isFinite(released[0].atMs) || !Number.isFinite(joined[0].atMs) ||
      released[0].atMs < before.nativeClockMs || joined[0].atMs < released[0].atMs ||
      joined[0].atMs > after.nativeClockMs) return false;
  return after.fixture.activeCaptures === 0 && after.preparedCaptureTokenCount === 0 &&
    after.coordinatorTrace.some(t => t.sequence > last &&
      t.phase === 'IntentApplied' && t.source === source && t.desiredAfter === 'Off' &&
      (source !== 'Some(HoldHotkey)' || (gesture != null && t.gesture === gesture)) &&
      after.coordinatorTrace.some(e => e.sequence === t.sequence && e.phase === 'CaptureStopEnqueued' &&
        e.runId === episode.runId) &&
      after.coordinatorTrace.some(e => e.sequence > t.sequence && e.phase === 'CaptureStopped' &&
        e.runId === episode.runId && e.desiredAfter === 'Off' && e.captureAfter === 'Idle'));
}
/** Orchestration only: all events cross IPC into the app's production dispatcher.
 * Mocked IPC tests prove these assertions, never native behavior. */
async function runNativeEventCase(pinia: Pinia, selected: string) {
  const report = { mode: 'native-event-case', case: selected, passed: false,
    errors: [] as string[], checkpoints: [] as Array<{ label: string; state: EventState }>,
    releaseToFirstPcmMs: -1, final: null as EventState | null };
  const unlisten = await listen('transcription:error', e => report.errors.push(JSON.stringify(e.payload)));
  const read = () => invoke<EventState>('native_e2e_state');
  const action = (action: string) => invoke('native_e2e_hotkey', { action });
  const physical = (physical: object) => invoke('native_e2e_hotkey', { action: 'press', physical });
  const keys = (downKeys: number[]) => physical({ kind: 'set', available: true, downKeys });
  const callback = (state: string) => physical({ kind: 'callback', state });
  async function observe(label: string, predicate: (s: EventState) => boolean, timeout = 3000) {
    const end = performance.now() + timeout;
    do {
      const s = await read();
      if (predicate(s)) { report.checkpoints.push({ label, state: s }); return s; }
      await sleep(10);
    } while (performance.now() < end);
    throw new Error(label + ' timed out');
  }
  const off = (s: EventState) => s.fixture.activeCaptures === 0 && s.preparedCaptureTokenCount === 0;
  const recording = (n: number) => (s: EventState) => s.fixture.captureStarts === n &&
    s.fixture.activeCaptures === 1 && s.fixture.providerMarkers.some(m => m.captureGeneration === n && m.count > 0);
  try {
    const config = useAppConfigStore(pinia); await config.startSync();
    await invoke('update_app_config', { holdToRecord: true, showMiniRecordingWindow: true,
      hideRecordingWindowOnHotkey: true, autoCopyToClipboard: false, autoPasteText: false, playCompletionSound: false });
    await config.refresh();
    await invoke('native_e2e_configure', { config: { audioDelayMs: 0, stopDelayMs: 0, controlDelayMs: 0, keepAlive: false } });
    if (selected === 'E42') { await keys([7, 55, 56]); await callback('pressed'); }
    else await press();
    await observe('A recording', recording(1));
    if (selected === 'E04') {
      await release();
      // No await of A cleanup or provider Finalize before the next physical Start.
      await press();
      await observe('A stop held', s => s.fixture.captureEvents.some(e => e.kind === 'stop-entered'));
      await sleep(150);
      const held = await observe('B queued behind actual A', s => s.fixture.activeCaptures === 1 && s.fixture.captureStarts === 1);
      check(!held.fixture.captureEvents.some(e => e.generation === 2), 'B emitted before A release');
      await action('release-capture-stop');
      const b = await observe('B recording', recording(2), 1000);
      const started = b.fixture.captureEvents.find(e => e.kind === 'capture-start' && e.generation === 2);
      const released = b.fixture.captureEvents.find(e => e.kind === 'capture-off' && e.generation === 1);
      const first = b.fixture.captureEvents.find(e => e.kind === 'first-pcm' && e.generation === 2);
      const joined = b.fixture.captureEvents.find(e => e.kind === 'capture-joined' && e.generation === 1);
      check(released && joined && started && first && released.atMs <= joined.atMs && joined.atMs <= started.atMs && started.atMs <= first.atMs, 'Missing or unordered native release/join/PCM clocks');
      report.releaseToFirstPcmMs = first.atMs - released.atMs;
      check(report.releaseToFirstPcmMs >= 0 && report.releaseToFirstPcmMs <= 250, 'B release latency outside 0..250ms');
      check(b.fixture.providerMarkers.some(m => m.captureGeneration === 2 && m.firstSequence === 1 && m.count > 0), 'Early B PCM lost');
      await release();
    } else if (selected === 'E41') {
      await action('save-capture-events');
      await release(); await observe('A off', off); await sleep(120); await press();
      const b = await observe('B recording', recording(2));
      for (const name of ['stale-key-release', 'stale-vad']) {
        const before = await observe('before ' + name, recording(2));
        check(sameMarkerOwner(b, before), 'B marker ownership changed before stale event');
        await action(name); await sleep(150);
        const after = await observe(name, recording(2));
        check(JSON.stringify(after.captureEpisode) === JSON.stringify(b.captureEpisode), 'Stale event replaced B');
        check(after.fixture.captureStops === before.fixture.captureStops, 'Stale event stopped B');
        check(markerAdvances(before, after), 'B writer stopped after stale event');
        const last = before.coordinatorTrace.slice(-1)[0]?.sequence ?? 0;
        check(after.coordinatorTrace.some(t => t.sequence > last && t.phase === 'IntentRejected' &&
          t.source === (name === 'stale-vad' ? 'Some(Vad)' : 'Some(HoldHotkey)')), 'Missing production stale rejection');
      }
      const keyBefore = await observe('before current key release', recording(2));
      await release();
      const keyStopped = await observe('current key release stops B', s => currentStopApplied(keyBefore, s, 'Some(HoldHotkey)'));
      check(currentStopApplied(keyBefore, keyStopped, 'Some(HoldHotkey)'), 'Current key release not applied');
      await sleep(120); await press(); await observe('C recording', recording(3));
      const vadBefore = await observe('before current VAD', recording(3));
      await action('current-vad');
      const vadStopped = await observe('current VAD stops C', s => currentStopApplied(vadBefore, s, 'Some(Vad)'));
      check(currentStopApplied(vadBefore, vadStopped, 'Some(Vad)'), 'Current VAD not applied');
      await release();
    } else {
      await action('sleep');
      const slept = await observe('sleep resources off', s => off(s) && s.fixture.activeProviders === 0);
      check(slept.coordinatorTrace.some(t => t.reason === 'Some(SystemSleep)'), 'Sleep did not reach production dispatcher');
      await action('wake'); await sleep(150);
      await observe('wake without phantom', s => off(s) && s.fixture.captureStarts === 1);
      await callback('pressed'); await sleep(150);
      await observe('wake remains off without key-up', s => off(s) && s.fixture.captureStarts === 1);
      await keys([]);
      await observe('A physical Up rearms', s => off(s) && !!s.physicalKeyboard?.events.some(e =>
        e.kind === 'watcher' && e.observation === 'Up' && e.result === 'Rearmed'));
      await keys([7, 55, 56]); await callback('pressed');
      const b = await observe('next gesture recording', recording(2));
      await physical({ kind: 'watcher-step', savedHandle: 'A' });
      await callback('released'); await callback('released'); await sleep(150);
      const held = await observe('delayed A release ignored', recording(2));
      check(sameMarkerOwner(b, held) && markerAdvances(b, held) &&
        held.fixture.captureStops === b.fixture.captureStops, 'Delayed A event interrupted B');
      const before = await observe('before B physical Up', recording(2));
      await keys([]);
      const stopped = await observe('next gesture release off', s => currentStopApplied(before, s, 'Some(HoldHotkey)'));
      check(currentStopApplied(before, stopped, 'Some(HoldHotkey)'), 'B watcher stop/join not completed');
      check(validE42PhysicalEvidence(stopped), 'Invalid fake physical ownership evidence');
    }
    await observe('capture cleanup', off);
    await action('sleep'); await action('wake');
    const final = await observe('terminal cleanup', s => off(s) && s.fixture.activeProviders === 0);
    report.final = final;
    check(final.fixture.captureStarts === (selected === 'E41' ? 3 : 2), 'Unexpected capture count');
    check(final.fixture.captureStops === final.fixture.captureStarts && final.fixture.maxActiveCaptures === 1, 'Capture leak/overlap');
    check(!final.fixture.observationOverflow && final.fixture.markerViolations.length === 0 &&
      !final.fixture.captureEvents.some(e => e.kind === 'stop-timeout'), 'Invalid native evidence');
    if (selected === 'E42') {
      const a = report.checkpoints.find(p => p.label === 'A recording')!.state;
      const b = report.checkpoints.find(p => p.label === 'next gesture recording')!.state;
      check(report.checkpoints.every(p => p.state.fixture.providerResumes === 0) &&
        a.fixture.providerStarts === 1 && b.fixture.providerStarts === 2 && markerFor(a) && markerFor(b) &&
        markerFor(a)!.providerSessionId !== markerFor(b)!.providerSessionId, 'Wake requires distinct cold providers without resumes');
      check(final.fixture.providerStarts === 2 && !final.fixture.controlResults.some(c => c.operation === 'continue'), 'Wake reused stale Continue');
      check(final.coordinatorTrace.some(t => t.reason === 'Some(SystemSleep)'), 'Missing production sleep trace');
    }
    check(report.errors.length === 0, 'Native transcription error');
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error));
    // Failed assertions must still close the paused fake provider through the
    // existing lifecycle seam. Cleanup never changes the qualification result.
    try {
      if (selected === 'E04') await action('release-capture-stop');
      await action('sleep'); await action('wake');
      await observe('failure cleanup', s => off(s) && s.fixture.activeProviders === 0);
    } catch (cleanupError) { report.errors.push('Failure cleanup: ' + String(cleanupError)); }
  }
  finally { unlisten(); }
  await invoke('native_e2e_finish', { report });
}

export const afterWriteCases = ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle'] as const;
export function coordinatorReadyForHandoff(s: {
  visible?: boolean; windowEpoch?: number; coordinatorShownEpoch?: number | null;
  captureEpisode?: { runId: number } | null;
  coordinatorCapture?: { recording: boolean; runId: number | null };
}): boolean {
  return s.visible === true && typeof s.windowEpoch === 'number' && s.windowEpoch > 0 &&
    s.coordinatorShownEpoch === s.windowEpoch && s.coordinatorCapture?.recording === true &&
    typeof s.captureEpisode?.runId === 'number' && s.coordinatorCapture.runId === s.captureEpisode.runId;
}
type AfterWriteState = EventState & {
  coordinatorShownEpoch?: number | null; coordinatorCapture?: { recording: boolean; runId: number | null };
  afterWriteService: {
  owner: number; status: string; logicalProviderRunId: number; pausedContinuation: number | null;
  coordinatorIdle: boolean; pendingStart: boolean; processingJobs: number; continuationPending: boolean;
  terminal: Array<{ runId: number; sequence: number; outcome: string; error: number | null }>;
  completedReport: { run_id: number; provider_release: string; error: string | null; shared_failure: boolean;
    continuation_not_started: unknown; audio: { reason: string; remaining_bytes: number; unknown_bytes: number;
      unacknowledged_bytes: number | null; accepted_bytes: number; read_bytes: number; submitted_bytes: number };
    provider: { reason: string; provider_release: string; error?: string } | null } | null;
}; logicalProviderRunId: number; fixture: EventState['fixture'] & {
  fullPcm: Array<{ seam: string; captureGeneration: number; chunks: number; samples: number; valid: boolean }>;
  maxActiveProviders: number; finals: number;
  captureMarkers: Array<{ captureGeneration: number; firstSequence: number; lastSequence: number; count: number }>;
  firstBWrites: Array<{ captureGeneration: number; logicalRunId: number; pauseEpoch: number }>;
} };
// Source ranges and decoded receiver ranges must agree per capture, through terminal stop.
export function serviceTerminal(a: AfterWriteState, b: AfterWriteState, final: AfterWriteState) {
  const s = final.afterWriteService, r = s?.completedReport, d = r?.audio, p = r?.provider;
  const terminal = s?.terminal?.filter(t => t.runId === a.logicalProviderRunId);
  return s?.owner === a.logicalProviderRunId && s.status === 'Idle' && s.logicalProviderRunId === 0 &&
    s.pausedContinuation === null && s.coordinatorIdle === true && s.pendingStart === false &&
    s.processingJobs === 0 && s.continuationPending === false &&
    terminal?.length === 1 && terminal[0].sequence > (b.coordinatorTrace[b.coordinatorTrace.length - 1]?.sequence ?? 0) &&
    terminal[0].outcome === 'Some(FinalizeCommitted)' && terminal[0].error === null &&
    r?.run_id === a.logicalProviderRunId && r.provider_release === 'released' && r.error === null &&
    r.shared_failure === false && r.continuation_not_started === null &&
    p?.reason === 'drained' && p.provider_release === 'released' && p.error == null &&
    d?.reason === 'drained' && d.remaining_bytes === 0 && d.unknown_bytes === 0 &&
    (d.unacknowledged_bytes === null || d.unacknowledged_bytes === 0) &&
    Number.isSafeInteger(d.accepted_bytes) && d.accepted_bytes > 0 &&
    d.accepted_bytes === d.read_bytes && d.read_bytes === d.submitted_bytes &&
    d.submitted_bytes === final.fixture.fullPcm?.filter(r => r.seam === 'provider').reduce((n, r) => n + r.samples * 2, 0);
}
export function verifyAfterWrite(a: AfterWriteState, b: AfterWriteState, final: AfterWriteState, selected: string) {
  const x = markerFor(a), y = markerFor(b);
  check(x && y && x.captureGeneration === 1 && y.captureGeneration === 2 &&
    b.fixture.activeCaptures === 1 && b.fixture.activeProviders === 1 && b.fixture.captureStops === 1 &&
    x.providerSessionId > 0 && x.providerSessionId === y.providerSessionId &&
    x.firstSequence === 1 && y.firstSequence === 1 && x.count > 0 && y.count > 0 &&
    x.count === x.lastSequence && y.count === y.lastSequence && a.logicalProviderRunId > 0 &&
    a.logicalProviderRunId === b.logicalProviderRunId && a.captureEpisode && b.captureEpisode &&
    a.captureEpisode.generation !== b.captureEpisode.generation, 'B must continue A with distinct capture ownership');
  const source = selected === 'after-write-hold' ? 'Some(HoldHotkey)' :
    selected === 'after-write-toggle' ? 'Some(CarbonHotkey)' : 'Some(Frontend)';
  check(currentStopApplied(b, final, source), 'Current B event did not stop its epoch');
  for (const s of [b, final]) {
    const f = s.fixture;
    check(f.firstBWrites.length === 1 && f.firstBWrites[0].captureGeneration === y.captureGeneration &&
      f.firstBWrites[0].logicalRunId === a.logicalProviderRunId && f.firstBWrites[0].pauseEpoch > 0 &&
      f.controlResults.some(c => c.operation === 'continue' && c.delivered && c.result.decision === 'accepted' &&
        c.result.pause_epoch === f.firstBWrites[0].pauseEpoch) &&
      !f.controlResults.some(c => c.operation === 'restore'), 'Missing first B write or unexpected Restore');
    check(!f.observationOverflow && f.markerViolations.length === 0 && f.providerStarts === 1 &&
      f.providerResumes === 0 && f.maxActiveProviders === 1 && f.maxActiveCaptures === 1 &&
      f.captureStarts === 2, 'Invalid capture/provider evidence');
  }
  check(serviceTerminal(a, b, final), 'Service terminal incomplete before finish');
  const f = final.fixture;
  check(f.firstBWrites[0].pauseEpoch === b.fixture.firstBWrites[0].pauseEpoch, 'B write epoch changed');
  for (const generation of [1, 2]) {
    for (const kind of ['capture-start', 'capture-off', 'capture-joined']) {
      check(f.captureEvents.filter(e => e.generation === generation && e.kind === kind).length === 1,
        'Missing or duplicate capture lifecycle');
    }
  }
  check(f.captureStops === 2 && f.activeCaptures === 0 && f.activeProviders === 0 &&
    final.preparedCaptureTokenCount === 0 && f.finals === 1 &&
    !f.captureEvents.some(e => e.kind === 'stop-timeout'), 'Terminal cleanup incomplete');
  check(f.fullPcm?.length === 4, 'Missing full PCM validation');
  for (const generation of [1, 2]) for (const seam of ['capture', 'provider']) {
    const rows = f.fullPcm.filter(r => r.captureGeneration === generation && r.seam === seam);
    const range = (seam === 'capture' ? f.captureMarkers : f.providerMarkers).find(r => r.captureGeneration === generation);
    check(rows.length === 1 && rows[0].valid === true && rows[0].chunks === range?.count &&
      rows[0].samples === rows[0].chunks * 320, 'Full PCM length, format or waveform mismatch');
  }
  check(f.captureMarkers.length === 2 && f.providerMarkers.length === 2, 'Missing per-capture PCM evidence');
  for (const owner of [x, y]) {
    const emitted = f.captureMarkers.filter(m => m.captureGeneration === owner.captureGeneration);
    const received = f.providerMarkers.filter(m => m.captureGeneration === owner.captureGeneration);
    check(emitted.length === 1 && received.length === 1, 'Ambiguous PCM generation');
    const e = emitted[0], r = received[0];
    check(e.firstSequence === 1 && r.firstSequence === 1 && Number.isSafeInteger(e.count) &&
      Number.isSafeInteger(r.count) && e.count > 0 &&
      e.count === e.lastSequence && r.count === r.lastSequence && e.count === r.count &&
      r.count >= owner.count && r.providerSessionId === owner.providerSessionId &&
      r.captureRunId === owner.captureRunId && r.captureFenceGeneration === owner.captureFenceGeneration,
      'Source PCM lost, replayed, discarded or reassigned');
  }
}
async function runAfterWriteCase(pinia: Pinia, selected: string) {
  const report = { mode: 'after-write-case', case: selected, passed: false, errors: [] as string[],
    a: null as AfterWriteState | null, b: null as AfterWriteState | null, final: null as AfterWriteState | null };
  const unlisten = await listen('transcription:error', e => report.errors.push(JSON.stringify(e.payload)));
  let nextId = 0;
  let handoffSubmitted = false;
  const phase = (diagnosticPhase: string, invokeId: number) =>
    invoke('native_e2e_delay', { durationMs: 0, diagnosticPhase, invokeId });
  // Timers help a responsive webview stop awaiting. Native absolute deadlines remain
  // authoritative if the webview or the IPC acknowledgement disappears.
  const bounded = async <T>(work: Promise<T>, ms: number): Promise<T> => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try { return await Promise.race([work, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error('E63 diagnostic IPC timeout')), ms);
    })]); } finally { clearTimeout(timer); }
  };
  const observation = async <T>(kind: 'state' | 'delay', work: () => Promise<T>) => {
    const id = ++nextId;
    await bounded(phase(`${kind}-before`, id), 2000);
    try {
      const value = await bounded(work(), 2000);
      await bounded(phase(`${kind}-after`, id), 2000);
      return value;
    } catch (error) {
      void phase('js-error', id).catch(() => {});
      throw error;
    }
  };
  const delay = (ms: number) => observation('delay', () => sleep(ms));
  const observe = async (predicate: (s: AfterWriteState) => boolean, label: string) => {
    const deadline = performance.now() + 10000;
    do {
      const s = await observation('state', () => state()) as AfterWriteState;
      if (predicate(s)) return s;
      await bounded(phase('predicate-false', nextId), 2000);
      if (performance.now() >= deadline) break;
      // Bound phase volume during the ten-second diagnostic observation window.
      await delay(100);
    } while (performance.now() < deadline);
    throw new Error(label);
  };
  try {
    const config = useAppConfigStore(pinia); await config.startSync();
    const hold = selected === 'after-write-hold';
    await invoke('update_app_config', { holdToRecord: hold, showMiniRecordingWindow: true,
      hideRecordingWindowOnHotkey: true, autoCopyToClipboard: false, autoPasteText: false, playCompletionSound: false, microphoneSensitivity: 100 });
    await config.refresh();
    await invoke('native_e2e_configure', { config: { audioDelayMs: 0, stopDelayMs: 0, controlDelayMs: 0, keepAlive: false } });
    if (hold) await press(); else await toggle();
    report.a = await observe(s => !!markerFor(s), 'A PCM missing');
    await delay(120);
    if (hold) await release(); else await toggle();
    await observe(s => s.fixture.activeCaptures === 0 && s.fixture.controlResults.some(c =>
      c.operation === 'pause' && c.delivered && c.result.decision === 'accepted'), 'A Pause missing');
    await delay(120);
    if (hold) await press(); else await toggle();
    report.b = await observe(s => coordinatorReadyForHandoff(s) && s.fixture.activeCaptures === 1 && s.fixture.captureStarts === 2 &&
      s.fixture.firstBWrites.length === 1 && markerFor(s)?.captureGeneration === 2 &&
      s.logicalProviderRunId === report.a!.logicalProviderRunId &&
      markerFor(s)?.providerSessionId === markerFor(report.a!)?.providerSessionId &&
      s.captureEpisode?.generation !== report.a!.captureEpisode?.generation &&
      s.fixture.firstBWrites[0].captureGeneration === markerFor(s)?.captureGeneration &&
      s.fixture.firstBWrites[0].logicalRunId === s.logicalProviderRunId &&
      s.fixture.controlResults.some(c => c.operation === 'continue' && c.delivered &&
        c.result.decision === 'accepted' && c.result.pause_epoch === s.fixture.firstBWrites[0].pauseEpoch),
      'Actual continued B first write missing');
    const stopId = ++nextId;
    await bounded(phase('arm', stopId), 2000);
    // Set before invoking: a lost acknowledgement must not create a JS fallback
    // Stop/finish. The already armed native watchdog owns a missing delivery too.
    handoffSubmitted = true;
    await bounded(invoke('native_e2e_terminal_handoff', { report, invokeId: stopId }), 2000);
  } catch (e) { report.errors.push(String(e)); }
  finally { unlisten(); }
  if (handoffSubmitted) return;
  const finishId = ++nextId;
  // Finish is submitted even if the marker rejects after a sticky native failure.
  try { await bounded(phase('finish-before', finishId), 2000); }
  catch (e) { report.passed = false; report.errors.push(String(e)); }
  try {
    await bounded(invoke('native_e2e_finish', { report }), 5000);
    await bounded(phase('finish-after', finishId), 2000);
  } catch (e) {
    report.passed = false;
    report.errors.push(`Finish: ${String(e)}`);
    void phase('js-error', finishId).catch(() => {});
    // The pre-finish native copy preserves the original assertion independently.
  }
}
