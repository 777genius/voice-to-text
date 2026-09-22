/** The real panel, store, coordinator and IPC; only audio/STT are test adapters. */
import type { Pinia } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTranscriptionStore } from '@/stores/transcription';
import { useAppConfigStore } from '@/stores/appConfig';
import { i18n } from '@/i18n';

type Marker = { captureGeneration: number; firstSequence: number; lastSequence: number; count: number };
interface Snapshot {
  status: string; visible: boolean; windowEpoch: number; sessionId: number;
  preparedCaptureTokenCount: number;
  fixture: {
    physicalOpenCount: number; physicalCloseCount: number; warmTerminalCount: number;
    rawIdleCallbackCount: number; audioChunks: number;
    captureStarts: number; activeCaptures: number; providerStarts: number; activeProviders: number;
    maxActiveProviders: number; markerViolations: string[]; captureMarkers: Marker[];
    captureRunAssociations: Array<{ captureGeneration: number; captureRunId: number }>;
    capturePcmLedgers: Array<{ captureGeneration: number; samples: number; hash: string }>;
    providerPcmLedgers: Array<{ captureGeneration: number; samples: number; hash: string }>;
    providerMarkers: Marker[]; finals: number;
  };
}
type PhysicalCounts = { open: number; close: number };
export type WarmVisibleFrame = {
  attempt: number; source: 'render' | 'shown' | 'sample'; windowEpoch: number;
  revision: number | null; runId: number | null; captureReady: boolean;
  readinessReason: string | undefined; phase: string; statusText: string;
};
export type WarmReopenEvidence = {
  attempt: number; baselineWindowEpoch: number; baselineRevision: number | null;
  windowEpoch: number | null; closed: boolean;
  acceptingObservations?: boolean;
  pendingFrames?: Array<Omit<WarmVisibleFrame, 'attempt' | 'windowEpoch'>>;
  observedFrameSignatures?: Set<string>;
};

export function reserveWarmVisibleFrameObservation(
  reopen: WarmReopenEvidence,
  frame: Omit<WarmVisibleFrame, 'attempt' | 'windowEpoch'>,
  expectedWindowEpoch?: number,
) {
  reopen.observedFrameSignatures ??= new Set<string>();
  const signature = JSON.stringify([
    frame.source,
    expectedWindowEpoch ?? reopen.windowEpoch,
    frame.revision,
    frame.runId,
    frame.captureReady,
    frame.readinessReason,
    frame.phase,
    frame.statusText,
  ]);
  if (reopen.observedFrameSignatures.has(signature)) {
    // A repeated shown/sample is still the native proof carrier for render
    // frames queued since its previous occurrence. Do not let signature
    // coalescing strand those newly admitted DOM observations.
    return frame.source !== 'render' && (reopen.pendingFrames?.length ?? 0) > 0;
  }
  reopen.observedFrameSignatures.add(signature);
  return true;
}

export async function drainPendingWarmVisibleObservations(pending: Set<Promise<void>>) {
  while (pending.size > 0) await Promise.all([...pending]);
}
export async function sealWarmVisibleObservations(
  reopen: WarmReopenEvidence,
  pending: Set<Promise<void>>,
  sample: () => void,
) {
  // Take the final synchronous UI sample and close admission in one turn.
  // Previously admitted native proofs remain in `pending` and are drained
  // below; closing first prevents a busy renderer from extending that set
  // forever while the capture remains active.
  sample();
  reopen.acceptingObservations = false;
  await drainPendingWarmVisibleObservations(pending);
}
export function warmVisibleFramesHaveNoStaleStatus(
  frames: WarmVisibleFrame[],
  staleStatusTexts: string[],
) {
  const stale = new Set(staleStatusTexts);
  return frames.every(frame => !stale.has(frame.statusText));
}
export function bindWarmVisibleFrameEvidence(
  reopen: WarmReopenEvidence,
  frame: Omit<WarmVisibleFrame, 'attempt' | 'windowEpoch'>,
  native: Pick<Snapshot, 'visible' | 'windowEpoch'>,
  expectedWindowEpoch?: number,
): WarmVisibleFrame[] {
  if (reopen.closed || native.visible !== true || !Number.isSafeInteger(native.windowEpoch) ||
      native.windowEpoch <= reopen.baselineWindowEpoch ||
      (expectedWindowEpoch !== undefined && native.windowEpoch !== expectedWindowEpoch) ||
      (reopen.windowEpoch !== null && reopen.windowEpoch !== native.windowEpoch)) return [];
  reopen.windowEpoch = native.windowEpoch;
  return [{ attempt: reopen.attempt, ...frame, windowEpoch: native.windowEpoch }];
}
export async function bindWarmVisibleFrameAfterNative(
  reopen: WarmReopenEvidence,
  readNative: () => Promise<Pick<Snapshot, 'visible' | 'windowEpoch'>>,
  readFrame: () => Omit<WarmVisibleFrame, 'attempt' | 'windowEpoch'> | null,
  expectedWindowEpoch?: number,
  firstNative?: Pick<Snapshot, 'visible' | 'windowEpoch'>,
) {
  // Capture the callback-time DOM synchronously. Waiting for the first IPC
  // sample before reading it can erase a bad first-visible frame. Native
  // samples taken around/after that captured frame only provide epoch
  // provenance; they must never replace the captured UI evidence.
  const frame = readFrame();
  if (!frame) return [];
  const boundWindowEpochAtCapture = reopen.windowEpoch;
  const hasCaptureTimeProvenance = firstNative !== undefined || expectedWindowEpoch !== undefined ||
    boundWindowEpochAtCapture !== null;
  if (!hasCaptureTimeProvenance) {
    reopen.pendingFrames ??= [];
    if (reopen.pendingFrames.length >= 16) throw new Error('Pending first-visible frame evidence overflow');
    reopen.pendingFrames.push(frame);
    return [];
  }
  const pendingFramesAtCapture = [...(reopen.pendingFrames ?? [])];
  const before = firstNative ?? await readNative();
  const after = await readNative();
  const provenanceEpoch = firstNative?.windowEpoch ?? expectedWindowEpoch ?? boundWindowEpochAtCapture;
  if (before.visible !== true || after.visible !== true || before.windowEpoch !== after.windowEpoch ||
      before.windowEpoch !== provenanceEpoch) return [];
  const frames = [...pendingFramesAtCapture, frame].flatMap(candidate =>
    bindWarmVisibleFrameEvidence(reopen, candidate, after, expectedWindowEpoch));
  if (frames.length > 0 && reopen.pendingFrames) {
    const consumed = new Set(pendingFramesAtCapture);
    reopen.pendingFrames = reopen.pendingFrames.filter(candidate => !consumed.has(candidate));
  }
  return frames;
}
const state = () => invoke<Snapshot>('native_e2e_state');
const physicalCounts = (snapshot: Snapshot): PhysicalCounts => ({
  open: snapshot.fixture.physicalOpenCount,
  close: snapshot.fixture.physicalCloseCount,
});
const markerForRun = (snapshot: Snapshot, runId: number | null) => {
  const association = snapshot.fixture.captureRunAssociations.find(a => a.captureRunId === runId);
  return snapshot.fixture.captureMarkers.find(m => m.captureGeneration === association?.captureGeneration);
};
export const hasMatchingRecoveryPcm = (snapshot: Snapshot, runId: number | null) => {
  const association = snapshot.fixture.captureRunAssociations.find(row => row.captureRunId === runId);
  const capture = snapshot.fixture.capturePcmLedgers.find(row =>
    row.captureGeneration === association?.captureGeneration);
  const provider = snapshot.fixture.providerPcmLedgers.find(row =>
    row.captureGeneration === association?.captureGeneration);
  return Boolean(capture && provider && capture.samples > 0 && provider.samples === capture.samples &&
    provider.hash === capture.hash);
};
// Native delay also resumes a hidden WebView; JS background timers are not our test clock.
const delay = (durationMs: number) => invoke('native_e2e_delay', { durationMs: Math.ceil(Math.max(0, durationMs)) });
function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }

export async function runNativeMiniUxScenario(pinia: Pinia): Promise<void> {
  const store = useTranscriptionStore(pinia);
  const config = useAppConfigStore(pinia);
  const report = { mode: 'mini-ux', warmMode: false, warmReopens: 0, idleAcceptedDelta: 0,
    warmReadyFrames: [] as Array<{ runId: number | null; revision: number | null; phase: string }>,
    warmWindowEpochs: [] as Array<{ attempt: number; windowEpoch: number;
      runId: number | null; revision: number | null; baselineRevision: number | null }>,
    warmReuseOpenCount: 0, lifecycle: null as null | {
      sleepClosed: boolean; wakeOpenedOnce: boolean; terminalCount: number; recoveryOpenedOnce: boolean;
      physical: {
        warmReopenStart: PhysicalCounts; warmReopenEnd: PhysicalCounts;
        policyActive: PhysicalCounts; policyClosed: PhysicalCounts;
        policyColdActive: PhysicalCounts; policyColdStopped: PhysicalCounts;
        policyResumed: PhysicalCounts;
        sleepClosed: PhysicalCounts; wakeOpened: PhysicalCounts;
        terminalClosed: PhysicalCounts; recoveryOpened: PhysicalCounts;
      };
    },
    warmForbiddenStatusTexts: [i18n.global.t('main.starting'), i18n.global.t('main.processing')],
    passed: false, errors: [] as string[], cases: [] as Array<{
    stop: string; hideMs: number; bufferedBeforeStop: boolean; oldProviderStillFinalizing: boolean;
    observations: number; backgroundDidNotReopen: boolean; successorStayedVisible: boolean;
    markerDeliveryComplete: boolean; backgroundStartingBeforeHide?: boolean;
  }>, warmActivationFrames: [] as Array<{
    source: 'render' | 'shown' | 'sample'; revision: number | null; runId: number | null;
    captureReady: boolean; phase: string; statusText: string;
  }>, warmVisibleFrames: [] as WarmVisibleFrame[], trace: [] as unknown[], final: null as Snapshot | null };
  const listeners: Array<() => void> = [];
  let lastGestureAt = 0;
  let shown = 0;
  let activeWarmReopen: WarmReopenEvidence | null = null;
  const pendingVisibleObservations = new Set<Promise<void>>();
  const observeWarmFrame = (source: 'render' | 'shown' | 'sample') => {
    const readiness = store.captureReadiness;
    if (!store.recordingDesiredOn || readiness?.reason !== 'activating-warm-capture' ||
        store.activeRecordingMode !== 'dictation' || store.hasError || store.error ||
        store.isIncomingTranslationActive || store.incomingTranslationError) return;
    const dot = document.querySelector('.mini-status-dot');
    if (!dot) return;
    if (report.warmActivationFrames.length >= 256) {
      if (!report.errors.includes('Warm frame evidence overflow')) report.errors.push('Warm frame evidence overflow');
      return;
    }
    const frame = { source, revision: readiness.revision, runId: readiness.runId,
      captureReady: store.isCaptureReady, phase: dot.className,
      statusText: dot.getAttribute('aria-label') ?? '' };
    report.warmActivationFrames.push(frame);
    if (frame.captureReady || frame.statusText || /\b(recording|starting|processing)\b/.test(frame.phase)) {
      report.errors.push(`Warm activation rendered a non-neutral ${source} frame`);
    }
  };
  const observeWarmVisibleFrame = (source: 'render' | 'shown' | 'sample',
    reopen = activeWarmReopen, native?: Snapshot, expectedWindowEpoch?: number) => {
    if (!reopen || reopen.acceptingObservations === false) return;
    if (report.warmVisibleFrames.length + pendingVisibleObservations.size >= 512) {
      if (!report.errors.includes('Warm visible frame evidence overflow')) {
        report.errors.push('Warm visible frame evidence overflow');
      }
      return;
    }
    const readFrame = () => {
      // The helper brackets this read with two native visibility snapshots.
      const dot = document.querySelector('.mini-status-dot');
      if (!dot) return null;
      return { source,
        revision: store.recordingIntentRevision,
        runId: store.captureRunId, captureReady: store.isCaptureReady,
        readinessReason: store.captureReadiness?.reason, phase: dot.className,
        statusText: dot.getAttribute('aria-label') ?? '' };
    };
    const frame = readFrame();
    if (!frame || !reserveWarmVisibleFrameObservation(reopen, frame, expectedWindowEpoch)) return;
    // MutationObserver can emit a burst while the WebView resumes. Preserve
    // each distinct synchronous DOM frame, but let the next shown/sample
    // observation provide the native epoch proof instead of flooding IPC with
    // two state reads per mutation.
    if (source === 'render' && native === undefined && expectedWindowEpoch === undefined) {
      reopen.pendingFrames ??= [];
      if (reopen.pendingFrames.length >= 16) {
        report.errors.push('Pending first-visible frame evidence overflow');
      } else {
        reopen.pendingFrames.push(frame);
      }
      return;
    }
    const observation = bindWarmVisibleFrameAfterNative(
      reopen, state, () => frame, expectedWindowEpoch, native)
      .then(frames => { report.warmVisibleFrames.push(...frames); }).catch(error => {
      report.errors.push(`Warm visible native observation failed: ${String(error)}`);
    }).finally(() => pendingVisibleObservations.delete(observation));
    pendingVisibleObservations.add(observation);
  };
  const unlisten = await listen<{ windowEpoch: number }>('recording:window-shown', event => {
    shown += 1;
    if (activeWarmReopen && Number.isSafeInteger(event.payload?.windowEpoch)) {
      observeWarmVisibleFrame('shown', activeWarmReopen, undefined, event.payload.windowEpoch);
    }
    observeWarmFrame('shown');
  });
  const renderObserver = new MutationObserver(() => {
    observeWarmVisibleFrame('render');
    observeWarmFrame('render');
  });
  renderObserver.observe(document.body, { subtree: true, childList: true, attributes: true,
    attributeFilter: ['class', 'aria-label'], characterData: true });
  const text = () => document.querySelector('.mini-transcription-text-inner')?.textContent?.trim() ?? '';
  const trace = (label: string, native?: Snapshot) => {
    if (activeWarmReopen && native?.visible && Number.isSafeInteger(native.windowEpoch)) {
      observeWarmVisibleFrame('sample', activeWarmReopen, native);
    } else {
      observeWarmVisibleFrame('sample');
    }
    observeWarmFrame('sample');
    if (report.trace.length >= 1800) throw new Error('Mini UX evidence overflow');
    report.trace.push({ label, at: performance.now(), native: native && {
      visible: native.visible, status: native.status, windowEpoch: native.windowEpoch,
      sessionId: native.sessionId, preparedCaptureTokenCount: native.preparedCaptureTokenCount,
      captureStarts: native.fixture.captureStarts, providerStarts: native.fixture.providerStarts,
      activeCaptures: native.fixture.activeCaptures, activeProviders: native.fixture.activeProviders,
    }, text: text(),
      phase: document.querySelector('.mini-status-dot')?.className,
      panel: document.querySelector('.popover')?.className,
      desiredOn: store.recordingDesiredOn, captureReady: store.isCaptureReady,
      captureRunId: store.captureRunId, intentRevision: store.recordingIntentRevision,
      readinessReason: store.captureReadiness?.reason,
      readinessGeneration: store.captureReadiness?.generation });
  };
  const until = async (label: string, accepts: (s: Snapshot) => boolean, timeout = 8000) => {
    await invoke('native_e2e_progress', { report: { mode: 'mini-ux', label } });
    const start = performance.now();
    let sample: Snapshot;
    do {
      sample = await state();
      trace(label, sample);
      if (accepts(sample)) return sample;
      await delay(25);
    } while (performance.now() - start < timeout);
    trace(`FAILED ${label}`, sample!);
    throw new Error(`Timeout: ${label}`);
  };
  const toggle = async () => {
    await delay(Math.max(0, 150 - (performance.now() - lastGestureAt)));
    await invoke('native_e2e_hotkey', { action: 'press' });
    await invoke('native_e2e_hotkey', { action: 'release' });
    lastGestureAt = performance.now();
  };
  try {
    await config.startSync();
    await invoke('update_app_config', { showMiniRecordingWindow: true, hideRecordingWindowOnHotkey: false,
      keepMicrophoneReady: true,
      holdToRecord: false, playCompletionSound: false, autoCopyToClipboard: false, autoPasteText: false });
    await config.refresh();
    await until('component mounted', () => Boolean(document.querySelector('.popover.mini')));
    // Independent startup settings/auth webviews must finish their visibility effects first.
    let stableEpoch = (await state()).windowEpoch;
    let stableSince = performance.now();
    await until('startup settled', s => {
      if (s.windowEpoch !== stableEpoch) { stableEpoch = s.windowEpoch; stableSince = performance.now(); }
      return performance.now() - stableSince >= 800;
    });
    await invoke('native_e2e_configure', { config: { audioDelayMs: 0, startDelayMs: 100,
      stopDelayMs: 5000, keepAlive: false } });

    for (const stop of ['hotkey', 'native-close', 'background-start-during-hide']) {
      const gated = stop === 'background-start-during-hide';
      await invoke('native_e2e_configure', { config: { stopDelayMs: gated ? 0 : 5000,
        startDelayMs: gated ? 1500 : 100, ...(gated ? { holdNextFinalize: true } : {}) } });
      const before = await state();
      await toggle();
      const a = await until(`${stop}: A recording`, s => s.visible && s.status === 'Recording' &&
        s.fixture.providerStarts === before.fixture.providerStarts + 1 && store.isCaptureReady);
      await toggle();
      await until(`${stop}: A hidden before finalize`, s => !s.visible && s.status === 'Processing', 1000);
      await toggle();
      const b = await until(`${stop}: B captures while A finalizes`, s => s.visible && store.isCaptureReady &&
        s.fixture.captureStarts === a.fixture.captureStarts + 1 && s.fixture.providerStarts === a.fixture.providerStarts &&
        (markerForRun(s, store.captureRunId)?.count ?? 0) >= 2, 1500);
      const bGeneration = markerForRun(b, store.captureRunId)!.captureGeneration;
      trace(`${stop}: B before stop`, b);
      let backgroundStartingBeforeHide: Promise<boolean> | undefined;
      if (gated) {
        listeners.push(await listen<{ status: string; session_id: number }>('recording:status', event => {
          if (event.payload.status === 'Starting' && event.payload.session_id !== a.sessionId) {
            backgroundStartingBeforeHide = state().then(s => s.visible);
            void backgroundStartingBeforeHide.catch(() => {});
          }
        }));
      }
      const closedAt = performance.now();
      if (stop === 'native-close') await invoke('native_e2e_close_recording');
      else await toggle();
      let oldProviderStillFinalizing = false;
      if (gated) {
        const closing = await until('B close animation before releasing A', s => s.visible &&
          Boolean(document.querySelector('.mini-closing')), 500);
        oldProviderStillFinalizing = closing.fixture.providerStarts === a.fixture.providerStarts;
        await invoke('native_e2e_configure', { config: { releaseFinalization: true } });
      }
      let closeObservations = 0;
      const hidden = await until(`${stop}: B hidden while A finalizes`, s => {
        closeObservations += 1;
        if (s.visible && !store.recordingDesiredOn) {
          check(![i18n.global.t('main.starting'), i18n.global.t('main.processing')].includes(text()),
            'Stopped foreground flashed a background Starting/Processing placeholder');
        }
        return !s.visible;
      }, 1000);
      const result = { stop, hideMs: performance.now() - closedAt, bufferedBeforeStop: true,
        oldProviderStillFinalizing: gated ? oldProviderStillFinalizing : hidden.fixture.providerStarts === a.fixture.providerStarts,
        backgroundStartingBeforeHide: gated ? await backgroundStartingBeforeHide : undefined,
        observations: closeObservations, backgroundDidNotReopen: true, successorStayedVisible: false,
        markerDeliveryComplete: false };
      report.cases.push(result);
      check(result.hideMs <= 1000 && result.oldProviderStillFinalizing,
        'B close waited for previous provider instead of its own window ownership');
      if (gated) check(result.backgroundStartingBeforeHide === true,
        'Controlled background Starting was not actually observed before native hide');
      const shownAfterStop = shown;
      if (!gated && hidden.fixture.physicalOpenCount > 0) {
        const startsBeforeRejectedC = hidden.fixture.captureStarts;
        const revisionBeforeRejectedC = store.recordingIntentRevision;
        await toggle();
        await delay(100);
        const rejected = await state();
        check(!rejected.visible && rejected.fixture.captureStarts === startsBeforeRejectedC &&
          store.recordingIntentRevision === revisionBeforeRejectedC,
        'Sealed pending B incorrectly admitted C');
      }
      if (stop !== 'native-close') {
        await until('A/B flush in background', s => {
          result.observations += 1;
          check(!s.visible && shown === shownAfterStop, 'Background B flush reopened the panel');
          return s.status === 'Idle' && s.fixture.activeCaptures === 0 && s.preparedCaptureTokenCount === 0;
        }, 16000);
      } else {
        // The bounded queue intentionally rejects C while sealed B still waits
        // behind A. Once B is admitted, C can capture during B finalization.
        await until('B admitted while hidden', s => {
          check(!s.visible && shown === shownAfterStop, 'Background B admission reopened the panel');
          return s.status === 'Processing' && s.fixture.providerStarts === before.fixture.providerStarts + 2;
        }, 8000);
        await toggle();
        const c = await until('C captures while B finalizes', s => s.visible && store.isCaptureReady &&
          s.fixture.captureStarts === b.fixture.captureStarts + 1, 1500);
        const cRun = store.captureRunId;
        const cEpoch = c.windowEpoch;
        await until('C survives late A/B events', s => {
          result.observations += 1;
          check(s.visible && s.windowEpoch === cEpoch, 'Old A/B work hid or reopened C');
          check(!document.querySelector('.mini-closing'), 'Old A/B work animated C closed');
          check(store.captureRunId === cRun && store.isCaptureReady,
            'Old A/B status replaced C capture readiness');
          return s.status === 'Recording' && s.fixture.providerStarts === before.fixture.providerStarts + 3;
        }, 16000);
        result.successorStayedVisible = true;
        await toggle();
        await until('C closes promptly', s => !s.visible, 1000);
        await until('C finishes hidden', s => {
          check(!s.visible, 'C finalization reopened the panel');
          return s.status === 'Idle' && s.fixture.activeCaptures === 0 && s.preparedCaptureTokenCount === 0;
        }, 8000);
      }
      const end = await state();
      const captured = end.fixture.captureMarkers.find(m => m.captureGeneration === bGeneration);
      const delivered = end.fixture.providerMarkers.filter(m => m.captureGeneration === bGeneration);
      check(captured && delivered.length === 1 && delivered[0].firstSequence === captured.firstSequence &&
        delivered[0].lastSequence === captured.lastSequence && delivered[0].count === captured.count,
      'Stopped B audio was lost, duplicated or sent to multiple providers');
      result.markerDeliveryComplete = true;
      check(end.fixture.markerViolations.length === 0 && end.fixture.maxActiveProviders === 1,
        'Audio marker order or single-provider invariant violated');
    }
    report.warmMode = (await state()).fixture.physicalOpenCount > 0;
    if (report.warmMode) {
      await invoke('native_e2e_configure', { config: { startDelayMs: 0, stopDelayMs: 0 } });
      const generations = new Set<number>();
      const warmReopenStart = await state();
      for (let attempt = 0; attempt < 10; attempt++) {
        const idle = await state();
        await delay(100);
        const later = await state();
        check(later.fixture.rawIdleCallbackCount > idle.fixture.rawIdleCallbackCount, 'No idle raw callbacks exercised');
        report.idleAcceptedDelta += later.fixture.audioChunks - idle.fixture.audioChunks;
        check(report.idleAcceptedDelta === 0, 'Idle native PCM escaped production gate');
        const visibleFrameStart = report.warmVisibleFrames.length;
        activeWarmReopen = { attempt: attempt + 1, baselineWindowEpoch: idle.windowEpoch,
          baselineRevision: store.recordingIntentRevision,
          windowEpoch: null, closed: false, acceptingObservations: true };
        await toggle();
        const recording = await until(`warm reopen ${attempt + 1}`, s => s.visible && store.isCaptureReady &&
          (markerForRun(s, store.captureRunId)?.count ?? 0) >= 2);
        const recordingRunId = store.captureRunId;
        check(recordingRunId !== null, `Warm reopen ${attempt + 1} has no capture run identity`);
        // Keep admissions open while native provenance reads settle. A DOM
        // mutation can happen during either read and its MutationObserver turn
        // must be admitted before this reopen is sealed.
        observeWarmVisibleFrame('sample', activeWarmReopen, recording);
        await sealWarmVisibleObservations(activeWarmReopen, pendingVisibleObservations,
          () => observeWarmVisibleFrame('sample', activeWarmReopen, recording));
        const visibleFrames = report.warmVisibleFrames.slice(visibleFrameStart)
          .filter(frame => frame.attempt === attempt + 1 && frame.windowEpoch === recording.windowEpoch);
        check((activeWarmReopen.pendingFrames?.length ?? 0) === 0,
          `Warm reopen ${attempt + 1} left unbound first-visible frame evidence`);
        check(visibleFrames.length > 0, `Warm reopen ${attempt + 1} produced no first-visible frame evidence`);
        const firstVisible = visibleFrames[0];
        check(visibleFrames.every(frame => !/\b(starting|processing)\b/.test(frame.phase)) &&
          warmVisibleFramesHaveNoStaleStatus(visibleFrames, report.warmForbiddenStatusTexts),
        `Warm reopen ${attempt + 1} retained a stale Starting/Processing frame`);
        if (/\brecording\b/.test(firstVisible.phase)) {
          check(firstVisible.captureReady,
            `Warm reopen ${attempt + 1} showed recording before capture readiness`);
        } else {
          check(firstVisible.statusText === '',
            `Warm reopen ${attempt + 1} neutral first frame exposed status text`);
        }
        const generation = markerForRun(recording, recordingRunId)?.captureGeneration;
        check(generation !== undefined, `Warm reopen ${attempt + 1} has no capture marker identity`);
        const phase = document.querySelector('.mini-status-dot')?.className ?? '';
        check(/\brecording\b/.test(phase) && !/\b(starting|processing)\b/.test(phase),
          'Admitted warm input failed to render ready recording phase');
        report.warmReadyFrames.push({ runId: recordingRunId, revision: store.recordingIntentRevision, phase });
        report.warmWindowEpochs.push({ attempt: attempt + 1, windowEpoch: recording.windowEpoch,
          runId: recordingRunId, revision: store.recordingIntentRevision,
          baselineRevision: activeWarmReopen.baselineRevision });
        check(!generations.has(generation), 'Warm reopen reused logical lease identity');
        generations.add(generation);
        check(recording.fixture.physicalOpenCount === 1, 'Warm reopen physically reopened input');
        activeWarmReopen.closed = true;
        activeWarmReopen = null;
        await toggle();
        await until('warm stop idle', s => !s.visible && s.status === 'Idle' && s.fixture.activeCaptures === 0);
        report.warmReopens++;
      }
      const warmReopenEnd = await state();
      check(report.warmActivationFrames.length > 0, 'Warm mode produced no activation frame evidence');
      const beforePolicyDisable = await state();
      await toggle();
      const policyRun = await until('policy disable run active', s => s.visible && store.isCaptureReady &&
        (markerForRun(s, store.captureRunId)?.count ?? 0) >= 2);
      await invoke('update_app_config', { keepMicrophoneReady: false });
      await delay(100);
      const disabledWhileActive = await state();
      check(disabledWhileActive.fixture.physicalCloseCount === beforePolicyDisable.fixture.physicalCloseCount &&
        disabledWhileActive.fixture.activeCaptures === 1 &&
        (markerForRun(disabledWhileActive, store.captureRunId)?.count ?? 0) >=
          (markerForRun(policyRun, store.captureRunId)?.count ?? 0),
      'Disabling keep-ready interrupted the active logical capture');
      await toggle();
      const disabledAndStopped = await until('policy disable closes after capture release', s => !s.visible &&
        s.status === 'Idle' && s.fixture.activeCaptures === 0 &&
        s.fixture.physicalCloseCount === beforePolicyDisable.fixture.physicalCloseCount + 1);
      const coldProviderLedgers = disabledAndStopped.fixture.providerPcmLedgers.length;
      await toggle();
      const policyColdActive = await until('disabled policy uses cold capture', s => s.visible &&
        s.status === 'Recording' && store.isCaptureReady && s.fixture.activeCaptures === 1 &&
        s.fixture.providerPcmLedgers.length > coldProviderLedgers &&
        s.fixture.providerPcmLedgers[s.fixture.providerPcmLedgers.length - 1].samples > 0);
      check(physicalCounts(policyColdActive).open === physicalCounts(disabledAndStopped).open &&
        physicalCounts(policyColdActive).close === physicalCounts(disabledAndStopped).close,
      'Cold fallback reopened the suspended warm input');
      await toggle();
      const policyColdStopped = await until('disabled policy cold capture stops', s => !s.visible &&
        s.status === 'Idle' && s.fixture.activeCaptures === 0);
      await invoke('update_app_config', { keepMicrophoneReady: true });
      const resumedPolicy = await until('policy re-enable resumes cached owner', s => !s.visible &&
        s.fixture.physicalOpenCount === policyColdStopped.fixture.physicalOpenCount + 1);
      const reuse = resumedPolicy;
      report.warmReuseOpenCount = reuse.fixture.physicalOpenCount;
      await invoke('native_e2e_hotkey', { action: 'sleep' });
      const slept = await until('sleep acknowledges physical close', s => !s.visible &&
        s.fixture.physicalCloseCount === reuse.fixture.physicalCloseCount + 1);
      await delay(100);
      check((await state()).fixture.physicalOpenCount === report.warmReuseOpenCount,
        'Sleep reopened physical input');
      await invoke('native_e2e_hotkey', { action: 'wake' });
      await invoke('native_e2e_hotkey', { action: 'prewarm-input' });
      const awake = await until('wake prewarms exactly once', s => !s.visible &&
        s.fixture.physicalOpenCount === report.warmReuseOpenCount + 1);
      await toggle();
      await until('device loss active lease', s => s.visible && store.isCaptureReady && s.status === 'Recording');
      await invoke('native_e2e_hotkey', { action: 'device-loss' });
      const lost = await until('device loss terminal and physical close', s => !s.visible &&
        s.fixture.warmTerminalCount === awake.fixture.warmTerminalCount + 1 &&
        s.fixture.physicalCloseCount === awake.fixture.physicalCloseCount + 1 && s.fixture.activeCaptures === 0);
      await delay(100);
      check((await state()).fixture.physicalOpenCount === awake.fixture.physicalOpenCount,
        'Device loss reopened without new intent');
      await toggle();
      const recovered = await until('explicit cold recovery after confirmed close', s =>
        s.visible && store.isCaptureReady && hasMatchingRecoveryPcm(s, store.captureRunId));
      check(recovered.fixture.physicalOpenCount === lost.fixture.physicalOpenCount + 1,
        'Recovery did not open exactly one replacement input');
      await toggle();
      await until('recovery stops hidden', s => !s.visible && s.status === 'Idle' && s.fixture.activeCaptures === 0);
      report.lifecycle = { sleepClosed: true, wakeOpenedOnce: true,
        terminalCount: lost.fixture.warmTerminalCount - awake.fixture.warmTerminalCount,
        recoveryOpenedOnce: true,
        physical: {
          warmReopenStart: physicalCounts(warmReopenStart),
          warmReopenEnd: physicalCounts(warmReopenEnd),
          policyActive: physicalCounts(disabledWhileActive),
          policyClosed: physicalCounts(disabledAndStopped),
          policyColdActive: physicalCounts(policyColdActive),
          policyColdStopped: physicalCounts(policyColdStopped),
          policyResumed: physicalCounts(resumedPolicy),
          sleepClosed: physicalCounts(slept),
          wakeOpened: physicalCounts(awake),
          terminalClosed: physicalCounts(lost),
          recoveryOpened: physicalCounts(recovered),
        },
      };
    }
    report.final = await state();
    check(!report.final.visible && report.final.status === 'Idle' && report.final.fixture.activeCaptures === 0 &&
      report.final.fixture.activeProviders === 0 && report.final.preparedCaptureTokenCount === 0, 'Resources were not released');
    check(report.errors.length === 0, 'Warm activation frame contract failed');
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error));
    trace('failure');
  } finally {
    renderObserver.disconnect();
    unlisten();
    for (const stopListener of listeners) stopListener();
    await invoke('native_e2e_configure', { config: { releaseFinalization: true } });
    await invoke('native_e2e_finish', { report });
  }
}
