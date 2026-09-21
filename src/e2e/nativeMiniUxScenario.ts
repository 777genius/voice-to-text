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
    providerMarkers: Marker[]; finals: number;
  };
}
type PhysicalCounts = { open: number; close: number };
const state = () => invoke<Snapshot>('native_e2e_state');
const physicalCounts = (snapshot: Snapshot): PhysicalCounts => ({
  open: snapshot.fixture.physicalOpenCount,
  close: snapshot.fixture.physicalCloseCount,
});
const markerForRun = (snapshot: Snapshot, runId: number | null) => {
  const association = snapshot.fixture.captureRunAssociations.find(a => a.captureRunId === runId);
  return snapshot.fixture.captureMarkers.find(m => m.captureGeneration === association?.captureGeneration);
};
// Native delay also resumes a hidden WebView; JS background timers are not our test clock.
const delay = (durationMs: number) => invoke('native_e2e_delay', { durationMs: Math.ceil(Math.max(0, durationMs)) });
function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }

export async function runNativeMiniUxScenario(pinia: Pinia): Promise<void> {
  const store = useTranscriptionStore(pinia);
  const config = useAppConfigStore(pinia);
  const report = { mode: 'mini-ux', warmMode: false, warmReopens: 0, idleAcceptedDelta: 0,
    warmReadyFrames: [] as Array<{ runId: number | null; revision: number | null; phase: string }>,
    warmReuseOpenCount: 0, lifecycle: null as null | {
      sleepClosed: boolean; wakeOpenedOnce: boolean; terminalCount: number; recoveryOpenedOnce: boolean;
      physical: {
        warmReopenStart: PhysicalCounts; warmReopenEnd: PhysicalCounts;
        policyActive: PhysicalCounts; policyClosed: PhysicalCounts; policyResumed: PhysicalCounts;
        sleepClosed: PhysicalCounts; wakeOpened: PhysicalCounts;
        terminalClosed: PhysicalCounts; recoveryOpened: PhysicalCounts;
      };
    },
    passed: false, errors: [] as string[], cases: [] as Array<{
    stop: string; hideMs: number; bufferedBeforeStop: boolean; oldProviderStillFinalizing: boolean;
    observations: number; backgroundDidNotReopen: boolean; successorStayedVisible: boolean;
    markerDeliveryComplete: boolean; backgroundStartingBeforeHide?: boolean;
  }>, warmActivationFrames: [] as Array<{
    source: 'render' | 'shown' | 'sample'; revision: number | null; runId: number | null;
    captureReady: boolean; phase: string; statusText: string;
  }>, warmVisibleFrames: [] as Array<{
    attempt: number; source: 'render' | 'shown' | 'sample'; windowEpoch: number;
    revision: number | null; runId: number | null; captureReady: boolean;
    readinessReason: string | undefined; phase: string; statusText: string;
  }>, trace: [] as unknown[], final: null as Snapshot | null };
  const listeners: Array<() => void> = [];
  let lastGestureAt = 0;
  let shown = 0;
  let activeWarmReopen: { attempt: number; windowEpoch: number | null } | null = null;
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
    reopen = activeWarmReopen) => {
    if (!reopen || reopen.windowEpoch === null) return;
    const dot = document.querySelector('.mini-status-dot');
    if (!dot) return;
    if (report.warmVisibleFrames.length >= 512) {
      if (!report.errors.includes('Warm visible frame evidence overflow')) {
        report.errors.push('Warm visible frame evidence overflow');
      }
      return;
    }
    report.warmVisibleFrames.push({ attempt: reopen.attempt, source,
      windowEpoch: reopen.windowEpoch, revision: store.recordingIntentRevision,
      runId: store.captureRunId, captureReady: store.isCaptureReady,
      readinessReason: store.captureReadiness?.reason, phase: dot.className,
      statusText: dot.getAttribute('aria-label') ?? '' });
  };
  const unlisten = await listen<{ windowEpoch: number }>('recording:window-shown', event => {
    shown += 1;
    if (activeWarmReopen && Number.isSafeInteger(event.payload?.windowEpoch)) {
      activeWarmReopen.windowEpoch = event.payload.windowEpoch;
      observeWarmVisibleFrame('shown');
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
      // Native visibility is authoritative if the WebView receives the shown
      // event after this polling sample.
      activeWarmReopen.windowEpoch = native.windowEpoch;
      observeWarmVisibleFrame('sample', activeWarmReopen);
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
        activeWarmReopen = { attempt: attempt + 1, windowEpoch: null };
        await toggle();
        const recording = await until(`warm reopen ${attempt + 1}`, s => s.visible && store.isCaptureReady &&
          (markerForRun(s, store.captureRunId)?.count ?? 0) >= 2);
        let visibleFrames = report.warmVisibleFrames.slice(visibleFrameStart)
          .filter(frame => frame.attempt === attempt + 1 && frame.windowEpoch === recording.windowEpoch);
        // A hidden WebView can coalesce the shown event and its first mutation
        // callback even though the native polling observation already proves
        // that this exact window epoch is visible. Preserve that first sampled
        // frame instead of making the evidence gate depend on callback timing.
        if (visibleFrames.length === 0) {
          activeWarmReopen.windowEpoch = recording.windowEpoch;
          observeWarmVisibleFrame('sample', activeWarmReopen);
          visibleFrames = report.warmVisibleFrames.slice(visibleFrameStart)
            .filter(frame => frame.attempt === attempt + 1 && frame.windowEpoch === recording.windowEpoch);
        }
        check(visibleFrames.length > 0, `Warm reopen ${attempt + 1} produced no first-visible frame evidence`);
        const firstVisible = visibleFrames[0];
        check(!/\b(starting|processing)\b/.test(firstVisible.phase) &&
          ![i18n.global.t('main.starting'), i18n.global.t('main.processing')].includes(firstVisible.statusText),
        `Warm reopen ${attempt + 1} first visible frame was stale Starting/Processing`);
        if (/\brecording\b/.test(firstVisible.phase)) {
          check(firstVisible.captureReady,
            `Warm reopen ${attempt + 1} showed recording before capture readiness`);
        } else {
          check(firstVisible.statusText === '',
            `Warm reopen ${attempt + 1} neutral first frame exposed status text`);
        }
        const generation = markerForRun(recording, store.captureRunId)!.captureGeneration;
        const phase = document.querySelector('.mini-status-dot')?.className ?? '';
        check(/\brecording\b/.test(phase) && !/\b(starting|processing)\b/.test(phase),
          'Admitted warm input failed to render ready recording phase');
        report.warmReadyFrames.push({ runId: store.captureRunId, revision: store.recordingIntentRevision, phase });
        check(!generations.has(generation), 'Warm reopen reused logical lease identity');
        generations.add(generation);
        check(recording.fixture.physicalOpenCount === 1, 'Warm reopen physically reopened input');
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
      await invoke('update_app_config', { keepMicrophoneReady: true });
      const resumedPolicy = await until('policy re-enable resumes cached owner', s => !s.visible &&
        s.fixture.physicalOpenCount === disabledAndStopped.fixture.physicalOpenCount + 1);
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
      const recovered = await until('explicit cold recovery after confirmed close', s => s.visible && store.isCaptureReady);
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
