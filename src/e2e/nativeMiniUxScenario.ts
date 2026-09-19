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
    captureStarts: number; activeCaptures: number; providerStarts: number; activeProviders: number;
    maxActiveProviders: number; markerViolations: string[]; captureMarkers: Marker[];
    captureRunAssociations: Array<{ captureGeneration: number; captureRunId: number }>;
    providerMarkers: Marker[]; finals: number;
  };
}
const state = () => invoke<Snapshot>('native_e2e_state');
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
  const report = { mode: 'mini-ux', passed: false, errors: [] as string[], cases: [] as Array<{
    stop: string; hideMs: number; bufferedBeforeStop: boolean; oldProviderStillFinalizing: boolean;
    observations: number; backgroundDidNotReopen: boolean; successorStayedVisible: boolean;
    markerDeliveryComplete: boolean; backgroundStartingBeforeHide?: boolean;
  }>, trace: [] as unknown[], final: null as Snapshot | null };
  const listeners: Array<() => void> = [];
  let lastGestureAt = 0;
  let shown = 0;
  const unlisten = await listen('recording:window-shown', () => { shown += 1; });
  const text = () => document.querySelector('.mini-transcription-text-inner')?.textContent?.trim() ?? '';
  const trace = (label: string, native?: Snapshot) => {
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
      captureRunId: store.captureRunId, intentRevision: store.recordingIntentRevision });
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
      check(captured && delivered.length === 1 && delivered[0].firstSequence === 1 &&
        delivered[0].lastSequence === captured.lastSequence && delivered[0].count === captured.count,
      'Stopped B audio was lost, duplicated or sent to multiple providers');
      result.markerDeliveryComplete = true;
      check(end.fixture.markerViolations.length === 0 && end.fixture.maxActiveProviders === 1,
        'Audio marker order or single-provider invariant violated');
    }
    report.final = await state();
    check(!report.final.visible && report.final.status === 'Idle' && report.final.fixture.activeCaptures === 0 &&
      report.final.fixture.activeProviders === 0 && report.final.preparedCaptureTokenCount === 0, 'Resources were not released');
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error));
    trace('failure');
  } finally {
    unlisten();
    for (const stopListener of listeners) stopListener();
    await invoke('native_e2e_configure', { config: { releaseFinalization: true } });
    await invoke('native_e2e_finish', { report });
  }
}
