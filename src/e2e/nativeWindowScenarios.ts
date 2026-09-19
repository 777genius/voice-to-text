import { runNativeMiniUxScenario } from './nativeMiniUxScenario';
import { runNativeReaderPreparation } from './nativeReaderPreparation';
import { runNativeContinuationCase } from './nativeContinuationCases';
import { runNativeContinuationLive } from './nativeContinuationLive';
import { runNativeContinuationScenarios } from './nativeContinuationScenarios';
import { nativeMeterEvidence } from './nativeMeterEvidence';
/** Real NSPanel/WKWebView/Vue/IPC scenarios; only native audio/STT adapters are fake. */
import type { Pinia } from 'pinia';
import { nextTick, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useTranscriptionStore } from '@/stores/transcription';
import { useAppConfigStore } from '@/stores/appConfig';
import { useAuthStore } from '@/features/auth/store/authStore';
import { createSession } from '@/features/auth/domain/entities/Session';
import { startNativeSampler } from './nativeSampler';

interface NativeState {
  ready: boolean;
  miniUxMode?: boolean;
  liveMode?: boolean;
  continuationMode?: boolean;
  continuationCase?: string;
  qualificationTrial?: unknown;
  readerPreparation?: boolean;
  terminalMode?: boolean;
  status: string;
  sessionId: number;
  windowEpoch: number;
  visible: boolean;
  preparedCaptureTokenCount: number;
  fixture: {
    livePcmBytes?: number;
    captureStarts: number; captureStops: number; activeCaptures: number; audioChunks: number;
    captureStartLatenciesMs: number[];
    firstPcmLatenciesMs?: Array<{captureGeneration: number; configuredDelayMs: number; elapsedMs: number}>;
    providerStarts: number; providerResumes: number; providerStops: number; providerFailures: number; activeProviders: number;
    maxActiveProviders: number;
    providerAudioChunks: number; finals: number; lastTranscript: string;
    captureMarkers: Array<{ captureGeneration: number; firstSequence: number; lastSequence: number; count: number }>;
    providerMarkers: Array<{ providerSessionId: number; captureRunId: number; captureFenceGeneration: number;
      captureGeneration: number; firstSequence: number; lastSequence: number; count: number }>;
    captureRunAssociations: Array<{ captureRunId: number; captureFenceGeneration: number; captureGeneration: number }>;
    markerViolations: string[];
    autoPasteTargetCaptures: number; autoPastes: number; lastPastedText: string | null;
    lastPastedSessionId: number | null;
  };
}
const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const state = () => invoke<NativeState>('native_e2e_state');
let lastPressAt = 0;
let lastReleaseAt = 0;
const hotkey = (action: 'press' | 'release') => {
  if (action === 'press') lastPressAt = Date.now();
  else lastReleaseAt = Date.now();
  return invoke('native_e2e_hotkey', { action });
};
function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
async function until<T>(sample: () => Promise<T>, accept: (value: T) => boolean, message: string, timeout = 8_000): Promise<T> {
  const started = Date.now();
  let last: T | undefined;
  while (Date.now() - started < timeout) {
    last = await sample();
    if (accept(last)) return last;
    await delay(20);
  }
  throw new Error(`${message}: ${JSON.stringify(last)}`);
}

export async function installNativeWindowHooks(pinia: Pinia): Promise<void> {
  if (import.meta.env.VITE_NATIVE_WINDOW_E2E !== '1') return;
  await until(async () => {
    try { return await state(); } catch { return null; }
  }, (value) => value?.ready === true, 'Native test-only fixture handshake unavailable', 20_000);
  const now = Date.now();
  useAuthStore(pinia).setAuthenticated(createSession({
    accessToken: 'native-fixture-only', refreshToken: 'native-fixture-only',
    accessExpiresAt: new Date(now + 3_600_000), refreshExpiresAt: new Date(now + 86_400_000),
    deviceId: 'native-window-e2e', user: undefined,
  }), 'native-e2e@local');
  if (getCurrentWindow().label === 'main') {
    // The caller mounts Vue once this handshake resolves; scenarios start afterwards.
    setTimeout(() => { void runNativeWindowScenarios(pinia); }, 0);
  }
}

export async function runNativeWindowScenarios(pinia: Pinia): Promise<void> {
  if (import.meta.env.VITE_NATIVE_WINDOW_E2E !== '1' || getCurrentWindow().label !== 'main') return;
  // A Vite flag alone never grants auth bypass or runs scenarios in a normal app.
  if ((await state()).miniUxMode) { await runNativeMiniUxScenario(pinia); return; }
  if ((await state()).readerPreparation) { await runNativeReaderPreparation(); return; }
  if ((await state()).qualificationTrial) { await runNativeContinuationLive(pinia); return; }
  const continuationCase = (await state()).continuationCase;
  if (continuationCase === 'E54') { await runRestartCrashScenario(pinia); return; }
  if (continuationCase) { await runNativeContinuationCase(pinia, continuationCase); return; }
  if ((await state()).continuationMode) { await runNativeContinuationScenarios(pinia); return; }
  if ((await state()).liveMode) { await runLiveNativeScenario(pinia); return; }
  if ((await state()).terminalMode) { await runNativeTerminalScenario(pinia); return; }
  const runStarted = Date.now();
  let confirmed = false;
  const report = { lastProgress: null as unknown, failureContext: null as unknown, elapsedMs: 0, passed: false, completedCycles: 0, hiddenIdleMs: 0, scenarios: [] as string[], observations: [] as unknown[], error: '' };
  try {
    await until(async () => {
      try { return await state(); } catch { return null; }
    }, (value) => value?.ready === true, 'Native test-only fixture handshake unavailable', 20_000);
    confirmed = true;
    const now = Date.now();
    const appConfig = useAppConfigStore(pinia);
    const store = useTranscriptionStore(pinia);
    await appConfig.startSync();
    await invoke('update_app_config', { showMiniRecordingWindow: true, hideRecordingWindowOnHotkey: true,
      holdToRecord: true, playCompletionSound: false, autoCopyToClipboard: false, autoPasteText: false });
    await appConfig.refresh();
    await until(async () => Boolean(document.querySelector('.popover.mini')), Boolean, 'Mini Vue component did not mount');
    await invoke('native_e2e_configure', { config: { audioDelayMs: 450, startDelayMs: 0, stopDelayMs: 130, keepAlive: false } });
    await progressStartup();
    async function progressStartup() {
      report.lastProgress = { scenario: 'fixture-ready', window: getCurrentWindow().label };
      await invoke('native_e2e_progress', { report: report.lastProgress });
      // Auth/settings webviews mount independently; require startup window epoch stability.
      let lastEpoch = (await state()).windowEpoch;
      let stableSince = Date.now();
      await until(async () => {
        const epoch = (await state()).windowEpoch;
        if (epoch !== lastEpoch) { lastEpoch = epoch; stableSince = Date.now(); }
        return Date.now() - stableSince;
      }, (stable) => stable >= 800, 'Startup windows never settled');
    }
    const baseline = await state();
    check(baseline.status === 'Idle' && baseline.fixture.activeCaptures === 0, 'Fixture must begin Idle without capture');
    const sessions = new Set<number>();
    const transcripts = new Set<string>();
    let successfulStarts = 0;
    let processingSeen = false;
    let listeningSeen = false;
    let recordingSeen = false;
    let holdMode = true;
    const nativeCaptureStartLatenciesMs: number[] = [];
    const queuedNativeCaptureStartLatenciesMs: number[] = [];

    const assertMarkerEvidence = (snapshot: NativeState) => {
      check(snapshot.fixture.maxActiveProviders <= 1,
        `Fixture observed ${snapshot.fixture.maxActiveProviders} simultaneous providers`);
      check(snapshot.fixture.markerViolations.length === 0,
        `Audio marker invariant failed: ${snapshot.fixture.markerViolations.join('; ')}`);
      const captures = new Map(snapshot.fixture.captureMarkers.map((range) => [range.captureGeneration, range]));
      const providerGenerations = new Set<number>();
      for (const range of snapshot.fixture.providerMarkers) {
        check(range.firstSequence === 1 && range.lastSequence === range.count,
          `Provider ${range.providerSessionId} has a missing/reordered marker range: ${JSON.stringify(range)}`);
        check(!providerGenerations.has(range.captureGeneration),
          `Capture generation ${range.captureGeneration} reached multiple provider sessions`);
        providerGenerations.add(range.captureGeneration);
        const captured = captures.get(range.captureGeneration);
        check(captured && captured.firstSequence === 1 && captured.lastSequence >= range.lastSequence,
          `Provider received marker range absent from capture ledger: ${JSON.stringify(range)}`);
        check(range.captureRunId > 0 && range.captureFenceGeneration > 0,
          `Provider marker lacks an authoritative run/generation fence: ${JSON.stringify(range)}`);
      }
    };
    const latestCaptureGeneration = (snapshot: NativeState) =>
      snapshot.fixture.captureMarkers[snapshot.fixture.captureMarkers.length - 1]?.captureGeneration ?? 0;

    const domText = () => document.querySelector('.mini-transcription-text-inner')?.textContent?.trim() ||
      document.querySelector('.transcription-text')?.textContent?.trim() || '';
    const transcriptMatches = (actual: string, expected: string) => actual.replace(/\s+/g, ' ').trim() === expected;
    const uiSnapshot = () => ({ status: store.status, sessionId: store.sessionId,
      error: store.error, errorType: store.errorType, hasError: store.hasError,
      displayText: store.displayText, partialText: store.partialText, accumulatedText: store.accumulatedText,
      finalText: store.finalText, visiblePartialText: store.visiblePartialText,
      visibleAccumulatedText: store.visibleAccumulatedText, visibleFinalText: store.visibleFinalText,
      isConnecting: store.isConnecting, lastAcceptedRecordingStatus: store.lastAcceptedRecordingStatus,
      popoverClass: document.querySelector('.popover')?.className,
      textClass: document.querySelector('.mini-transcription-text, .transcription-text')?.className });
    const progress = async (name: string, details: Record<string, unknown> = {}) => {
      report.lastProgress = { scenario: name, completedCycles: report.completedCycles, elapsedMs: Date.now() - now, ...details };
      console.info('[native-e2e]', name, JSON.stringify(report));
      await invoke('native_e2e_progress', { report: report.lastProgress });
    };
    const configure = (config: Record<string, unknown>) => invoke('native_e2e_configure', { config });
    const mode = async (mini: boolean) => {
      await invoke('update_app_config', { showMiniRecordingWindow: mini, hideRecordingWindowOnHotkey: mini });
      await appConfig.refresh();
      await nextTick();
    };
    const waitForNewRecording = async (previous: NativeState, requireListening = false, timeout = 8_000,
      listeningWasObserved: () => boolean = () => false) => {
      const recording = await until(state, (s) => s.status === 'Recording' && s.visible && s.sessionId > previous.sessionId,
        'Hotkey did not create a visible recording', timeout);
      check(!sessions.has(recording.sessionId), 'A new recording reused a session ID');
      sessions.add(recording.sessionId);
      successfulStarts += 1;
      if (requireListening) {
        await until(async () => ({ observed: listeningWasObserved(), listening: store.isListeningPlaceholder,
          text: domText(), recording: !!document.querySelector('.mini-status-dot.recording, .record-button.recording') }),
        (ui) => ui.observed || (ui.listening && ui.text.length > 0 && ui.recording),
        'Visible Listening/Recording UI missing before audio');
        listeningSeen = true;
      } else {
        // Native hotkey can produce audio before a suspended WebView resumes its JS.
        await until(async () => ({ ui: uiSnapshot(), recording: store.isRecording,
          indicator: !!document.querySelector('.mini-status-dot.recording, .record-button.recording') }),
        (ui) => ui.recording && ui.indicator && ui.ui.sessionId === recording.sessionId,
        'Resumed native recording has no matching visible Recording UI');
      }
      recordingSeen = true;
      const expected = `Native fixture session ${recording.fixture.providerStarts + recording.fixture.providerResumes}`;
      await until(async () => ({ expected, text: domText(), ui: uiSnapshot(), backend: await state() }),
        (s) => transcriptMatches(s.text, expected) && s.backend.fixture.providerAudioChunks > previous.fixture.providerAudioChunks,
        'Fresh audio did not produce a visible unique transcript');
      check(!transcripts.has(expected), 'New session reused the old visible transcript');
      transcripts.add(expected);
      await until(async () => Boolean(document.querySelector('.mini-opening, .mini-closing, .mini-animation-reset')), (busy) => !busy, 'Opening animation never settled');
      return { ...(await state()), expected };
    };
    const start = async (requireListening = false) => {
      const previous = await state();
      report.lastProgress = { phase: 'start', completedCycles: report.completedCycles, previousSessionId: previous.sessionId, previousEpoch: previous.windowEpoch };
      await delay(Math.max(0, 130 - (Date.now() - lastPressAt), 60 - (Date.now() - lastReleaseAt)));
      let listeningObserved = false;
      let greenBeforeCaptureReady = false;
      const sampleTransientUi = () => {
        const green = !!document.querySelector('.mini-status-dot.recording');
        listeningObserved ||= store.isListeningPlaceholder && domText().length > 0 && green;
        greenBeforeCaptureReady ||= green && !store.isCaptureReady;
      };
      const observer = new MutationObserver(sampleTransientUi);
      observer.observe(document.documentElement, { subtree: true, childList: true, characterData: true,
        attributes: true, attributeFilter: ['class'] });
      try {
        sampleTransientUi();
        await hotkey('press');
        if (!holdMode) await hotkey('release');
        const ready = await until(async () => ({ backend: await state(), captureReady: store.isCaptureReady }),
          (sample) => sample.captureReady && sample.backend.fixture.captureStarts > previous.fixture.captureStarts &&
            sample.backend.fixture.captureStartLatenciesMs.length > previous.fixture.captureStartLatenciesMs.length,
        'Accepted start did not publish capture readiness', 2_000);
        nativeCaptureStartLatenciesMs.push(ready.backend.fixture.captureStartLatenciesMs[
          ready.backend.fixture.captureStartLatenciesMs.length - 1
        ]);
        const recording = await waitForNewRecording(previous, requireListening, 8_000, () => listeningObserved);
        check(!greenBeforeCaptureReady, 'Recording indicator became green before capture readiness');
        return recording;
      } finally { observer.disconnect(); }
    };
    // Watches every DOM class mutation (including old class values), plus native IPC samples.
    const observe = async (duration: number, action: () => Promise<unknown>, expected: string, allowClosing = false) => {
      let sawClosing = !!document.querySelector('.mini-closing');
      const observer = new MutationObserver((changes) => {
        sawClosing ||= !!document.querySelector('.mini-closing') || changes.some((change) =>
          (change.oldValue || '').split(/\s+/).includes('mini-closing'));
      });
      observer.observe(document.documentElement, { subtree: true, attributes: true, attributeFilter: ['class'], attributeOldValue: true });
      const started = Date.now();
      const beforeAction = await state();
      let samples = 0;
      try {
        await action();
        do {
          const snapshot = await state();
          const actualDomText = domText();
          let violation = !snapshot.visible ? 'Current native panel became hidden during protected interval' : '';
          if (!allowClosing) {
            if (sawClosing) violation ||= 'Late callback briefly animated current mini panel closed';
            if (!transcriptMatches(actualDomText, expected)) violation ||= 'Late callback suppressed or replaced current visible transcript';
            if (!store.isRecording || snapshot.status !== 'Recording') violation ||= 'Late callback changed current recording status';
          }
          if (violation) {
            report.failureContext = { expectedTranscript: expected, actualDomText, ui: uiSnapshot(), backend: snapshot,
              windowEpochBeforeAction: beforeAction.windowEpoch, sessionIdBeforeAction: beforeAction.sessionId,
              elapsedMs: Date.now() - started, samples, sawClosing, lastProgress: report.lastProgress };
            throw new Error(`${violation}: ${JSON.stringify(report.failureContext)}`);
          }
          samples += 1;
          await delay(15);
        } while (Date.now() - started < duration);
        report.observations.push({ durationMs: Date.now() - started, samples, sawClosing });
        return sawClosing;
      } finally { observer.disconnect(); }
    };
    const stop = async (timeout = 8_000) => {
      report.lastProgress = { phase: 'stop', completedCycles: report.completedCycles, ui: uiSnapshot() };
      if (holdMode) await hotkey('release');
      else { await hotkey('press'); await hotkey('release'); }
      await until(state, (s) => s.status === 'Idle' && !s.visible && s.fixture.activeCaptures === 0 &&
        s.preparedCaptureTokenCount === 0,
      'Stop did not reach Idle/hidden/no capture/no prepared token', timeout);
      await until(async () => store.isIdle, Boolean, 'Backend stopped but UI did not reach Idle');
    };

    // Observe the transient placeholder from before dispatch. Later rapid cycles
    // prove readiness and fresh transcripts without racing this short-lived frame.
    await progress('listening-placeholder-before-audio-starting');
    let current = await start(true);
    await stop();
    report.scenarios.push('visible-listening-before-first-audio');

    // Positive current-epoch close, then stale real native hide refusal after reopen.
    await progress('current-and-stale-close-starting');
    current = await start();
    check(await observe(300, () => emit('recording:window-will-hide-for-hotkey-stop', { windowEpoch: current.windowEpoch }), current.expected, true),
      'Positive control: current close event never animated the real Vue mini panel');
    check(await invoke('hide_recording_window_if_current', { windowEpoch: current.windowEpoch }) === true, 'Current epoch close refused');
    check(!(await state()).visible, 'Current epoch close did not hide NSPanel');
    await invoke('show_recording_window');
    await until(async () => domText(), (text) => transcriptMatches(text, current.expected), 'Reopen did not restore transcript');
    await until(async () => Boolean(document.querySelector('.mini-opening, .mini-closing, .mini-animation-reset')), (busy) => !busy, 'Reopen animation never settled');
    await observe(800, async () => {
      check(await invoke('hide_recording_window_if_current', { windowEpoch: current.windowEpoch }) === false, 'Old epoch hid replacement panel');
      await emit('recording:window-will-hide-for-hotkey-stop', { windowEpoch: current.windowEpoch });
    }, current.expected);
    await stop();
    report.scenarios.push('current-and-stale-native-close');

    // Production users normally use toggle mode and may leave native hotkey-hide
    // disabled. Vue then performs the delayed auto-hide after finalization. The
    // coordinator's last completed panel state used to remain Shown, so the next
    // toggle started audio without issuing another native show. Exercise that
    // exact path, including the safe fixture substitute for external-app paste.
    // Let the previous scenario's hotkey-stop grace expire while auto-paste is
    // still disabled, so its transcript cannot contaminate these exact counters.
    await invoke('native_e2e_delay', { durationMs: 1_550 });
    await progress('toggle-auto-hide-autopaste-restart-starting');
    await invoke('update_app_config', {
      holdToRecord: false,
      hideRecordingWindowOnHotkey: false,
      autoPasteText: true,
    });
    await appConfig.refresh();
    holdMode = false;
    const beforeToggleRestart = await state();
    current = await start();
    check(current.fixture.autoPasteTargetCaptures === beforeToggleRestart.fixture.autoPasteTargetCaptures + 1,
      'Toggle start did not capture exactly one auto-paste target before showing the panel');
    const firstToggleTranscript = current.expected;
    const firstToggleSessionId = current.sessionId;
    await stop();
    const firstToggleStopped = await until(state,
      (sample) => sample.fixture.autoPastes === beforeToggleRestart.fixture.autoPastes + 1 &&
        sample.fixture.lastPastedText === firstToggleTranscript &&
        sample.fixture.lastPastedSessionId === firstToggleSessionId,
      'Recognized final text did not reach the safe auto-paste fixture exactly once');
    const hiddenToggleEpoch = firstToggleStopped.windowEpoch;

    current = await start();
    check(current.windowEpoch > hiddenToggleEpoch,
      'Toggle restart after frontend auto-hide did not issue a fresh native show');
    check(current.fixture.autoPasteTargetCaptures === firstToggleStopped.fixture.autoPasteTargetCaptures + 1,
      'Toggle restart did not replace the auto-paste target exactly once');
    const secondToggleTranscript = current.expected;
    const secondToggleSessionId = current.sessionId;
    await stop();
    await until(state,
      (sample) => sample.fixture.autoPastes === firstToggleStopped.fixture.autoPastes + 1 &&
        sample.fixture.lastPastedText === secondToggleTranscript &&
        sample.fixture.lastPastedSessionId === secondToggleSessionId,
      'Second recognized final text did not reach the safe auto-paste fixture exactly once');
    report.scenarios.push('toggle-auto-hide-reopens-and-pastes-recognized-text');
    await invoke('update_app_config', {
      holdToRecord: true,
      hideRecordingWindowOnHotkey: true,
      autoPasteText: false,
    });
    await appConfig.refresh();
    holdMode = true;

    await progress('duplicate-direct-start-starting');
    current = await start();
    const beforeDuplicate = await state();
    check(await invoke('start_recording') === 'Recording already active', 'Duplicate direct start was not rejected as busy');
    const afterDuplicate = await state();
    check(afterDuplicate.windowEpoch > beforeDuplicate.windowEpoch && afterDuplicate.sessionId === current.sessionId &&
      afterDuplicate.fixture.providerStarts === beforeDuplicate.fixture.providerStarts,
      'Duplicate direct start changed the recording instead of only advancing window intent');
    await observe(600, async () => {}, current.expected);
    const minimize = document.querySelector<HTMLButtonElement>('.mini-icon-button:has(.mdi-window-minimize)');
    check(minimize, 'Real mini minimize button missing');
    minimize.click();
    await until(state, (s) => !s.visible && s.status === 'Recording', 'Duplicate start left the UI closing with a stale window epoch');
    await invoke('show_recording_window');
    await stop();
    report.scenarios.push('duplicate-direct-start-syncs-epoch-and-ui-minimize');

    await configure({ audioDelayMs: 0 });
    await progress('recording-cycles-starting');
    for (let cycle = 0; cycle < 50; cycle += 1) {
      if (cycle === 25) await configure({ keepAlive: true });
      current = await start();
      await observe(90, async () => {}, current.expected);
      await stop();
      report.completedCycles += 1;
      if (cycle % 5 === 0) await progress(`recording-cycle-${cycle + 1}`);
    }
    report.scenarios.push('50-audio-transcript-stop-hide-reopen-cycles');
    const afterRapidCycles = await state();
    check(afterRapidCycles.preparedCaptureTokenCount === 0,
      'Rapid recording cycles retained prepared capture registry entries');
    check(afterRapidCycles.fixture.providerResumes > baseline.fixture.providerResumes, 'Keepalive cycles never resumed a provider');
    report.scenarios.push('cold-and-keepalive-provider-sessions');
    await configure({ keepAlive: false, audioDelayMs: 450 });

    // Toggle ownership supports replacement intent while the previous stop is still closing.
    await progress('replacement-during-close-starting');
    await invoke('update_app_config', { holdToRecord: false });
    holdMode = false;
    current = await start();
    await hotkey('press');
    await hotkey('release');
    await until(async () => Boolean(document.querySelector('.mini-closing')), Boolean, 'No closing interval before delayed native hide', 1000);
    const old = current;
    let replacementError = '';
    let replacementSeen = false;
    const replacementObserver = new MutationObserver((changes) => {
      const isNew = store.sessionId !== null && store.sessionId > old.sessionId;
      if (isNew && (document.querySelector('.mini-closing') || (replacementSeen && changes.some((change) =>
        (change.oldValue || '').split(/\s+/).includes('mini-closing'))))) {
        replacementError ||= 'Replacement briefly entered mini-closing during start';
      }
      replacementSeen ||= isNew;
    });
    replacementObserver.observe(document.documentElement, { subtree: true, attributes: true, attributeFilter: ['class'], attributeOldValue: true });
    let seenEpoch = false;
    let replacementSampleCount = 0;
    let lastReplacementSampleMs = 0;
    const replacementSamples = startNativeSampler(async () => {
      const snapshot = await state();
      replacementSampleCount += 1;
      lastReplacementSampleMs = Date.now() - runStarted;
      seenEpoch ||= snapshot.windowEpoch > old.windowEpoch && snapshot.sessionId > old.sessionId;
      if (seenEpoch && !snapshot.visible) replacementError ||= 'Replacement native panel hid during start';
    });
    let replacementStartReturned = false;
    try { current = await start(); replacementStartReturned = true; }
    catch (error) {
      // Cleanup can fail independently; retain the first failure in the report.
      report.failureContext = { scenario: 'replacement-start',
        startError: error instanceof Error ? error.stack || error.message : String(error), ui: uiSnapshot() };
      throw error;
    }
    finally {
      // Diagnostic IPC failures must not replace the original start failure or
      // prevent sampler/observer cleanup. A real pending IPC still hits the watchdog.
      await progress('replacement-sampler-joining', { startReturned: replacementStartReturned,
        sampleCount: replacementSampleCount, lastSampleMs: lastReplacementSampleMs })
        .catch((error) => console.warn('[native-e2e] cleanup progress failed', error));
      const joinStarted = Date.now();
      try { await replacementSamples.stop(); }
      catch (error) {
        report.failureContext = { scenario: 'replacement-sampler-cleanup', startFailure: report.failureContext,
          cleanupError: error instanceof Error ? error.stack || error.message : String(error) };
        throw error;
      }
      finally { replacementObserver.disconnect(); }
      report.observations.push({ scenario: 'replacement-sampler-joined', sampleCount: replacementSampleCount,
        lastSampleMs: lastReplacementSampleMs, joinElapsedMs: Date.now() - joinStarted });
      await progress('replacement-sampler-joined', { startReturned: replacementStartReturned,
        sampleCount: replacementSampleCount, lastSampleMs: lastReplacementSampleMs, joinElapsedMs: Date.now() - joinStarted })
        .catch((error) => console.warn('[native-e2e] cleanup progress failed', error));
    }
    check(!replacementError, replacementError);
    await observe(800, async () => {
      await invoke('stop_recording', { expectedSessionId: old.sessionId });
      await emit('recording:status', { session_id: old.sessionId, status: 'Idle', stopped_via_hotkey: true });
      await emit('recording:window-will-hide-for-hotkey-stop', { windowEpoch: old.windowEpoch });
      await emit('recording:window-shown', { windowEpoch: old.windowEpoch });
    }, current.expected);
    check((await state()).sessionId === current.sessionId, 'Old stop changed replacement session');
    await stop();
    report.scenarios.push('second-press-during-220ms-close', 'stale-status-shown-hide-and-session-stop');
    await invoke('update_app_config', { holdToRecord: true });
    holdMode = true;

    await progress('stop-during-starting-starting');
    await configure({ startDelayMs: 650, audioDelayMs: 1400 });
    const queuedBefore = await state();
    await Promise.all([hotkey('press'), hotkey('press'), hotkey('press')]);
    await until(state, (s) => s.status === 'Starting', 'Delayed provider never reached Starting');
    await hotkey('release');
    await hotkey('release');
    await until(state, (s) => s.status === 'Idle' && s.fixture.activeCaptures === 0, 'Stop during Starting leaked capture');
    await until(async () => store.isIdle && !store.isConnecting, Boolean, 'Released Starting left UI stuck connecting');
    const queuedAfter = await state();
    check(queuedAfter.fixture.providerStarts - queuedBefore.fixture.providerStarts <= 1, 'Queued presses created duplicate providers');
    await configure({ startDelayMs: 0, audioDelayMs: 450 });
    current = await start();
    await stop();
    report.scenarios.push('duplicate-press-release-stop-during-starting');

    await progress('early-hold-release-starting');
    await mode(false);
    await invoke('show_recording_window');
    const beforeEarlyRelease = await state();
    await delay(Math.max(0, 130 - (Date.now() - lastPressAt), 60 - (Date.now() - lastReleaseAt)));
    lastPressAt = Date.now();
    const gatedPress = invoke('native_e2e_hotkey', { action: 'press-before-start' });
    try {
      await until(async () => ({ backend: await state(), starting: store.isStarting }),
        (s) => s.starting && s.backend.status === 'Idle' && s.backend.sessionId === 0 &&
          s.backend.windowEpoch > beforeEarlyRelease.windowEpoch && s.backend.visible,
        'Early-release fixture never exposed a real provisional Starting UI', 2_000);
    } finally {
      await hotkey('release');
      await gatedPress;
    }
    await until(async () => ({ backend: await state(), idle: store.isIdle, connecting: store.isConnecting }),
      (s) => s.idle && !s.connecting && s.backend.status === 'Idle' && s.backend.visible &&
        s.backend.fixture.activeCaptures === 0 && s.backend.fixture.providerStarts === beforeEarlyRelease.fixture.providerStarts,
      'Early hold release left the visible regular UI Starting or allocated a provider');
    await mode(true);
    current = await start();
    await stop();
    report.scenarios.push('early-hold-release-cancels-visible-provisional-start');

    // Hold press while a real UI stop finalizes creates pending start, released hold cancels it.
    await progress('processing-pending-hold-starting');
    await configure({ stopDelayMs: 5000 });
    current = await start();
    const uiStop = store.stopRecording('native-e2e-processing');
    await until(async () => ({ backend: await state(), processing: !!document.querySelector('.mini-status-dot.processing, .record-button.processing') }),
      (s) => s.backend.status === 'Processing' && s.processing, 'Real UI stop never rendered Processing');
    processingSeen = true;
    await hotkey('release');
    await delay(60);
    const pendingBefore = await state();
    const pendingEpoch = pendingBefore.windowEpoch;
    await hotkey('press');
    const pendingReady = await until(async () => ({ backend: await state(), readiness: store.captureReadiness,
      green: !!document.querySelector('.mini-status-dot.recording:not(.starting):not(.processing)') }),
    (sample) => sample.backend.windowEpoch > pendingEpoch && sample.backend.status === 'Processing' &&
      sample.backend.fixture.activeCaptures === 1 && sample.readiness?.state === 'buffering' && sample.green,
    'Pending start did not capture audio or render green during previous finalize', 2_000);
    const bufferedPendingBackend = await until(state,
      (sample) => {
        const generation = latestCaptureGeneration(sample);
        return generation > latestCaptureGeneration(pendingBefore) &&
          sample.fixture.captureRunAssociations.some((association) =>
            association.captureGeneration === generation && association.captureRunId === pendingReady.readiness?.runId);
      },
      'Pending capture readiness produced no new deterministic PCM marker', 2_000);
    const cancelledCaptureRange = bufferedPendingBackend.fixture.captureMarkers[
      bufferedPendingBackend.fixture.captureMarkers.length - 1
    ];
    const cancelledGeneration = cancelledCaptureRange?.captureGeneration;
    check(cancelledGeneration && cancelledCaptureRange.firstSequence === 1,
      'Pending capture produced no deterministic first audio marker');
    check(pendingReady.backend.fixture.providerStarts === pendingBefore.fixture.providerStarts,
      'Pending capture opened a second provider before previous finalize');
    await hotkey('release');
    await uiStop;
    const cancelledPending = await state();
    const afterPending = await until(
      state,
      (sample) => sample.status === 'Idle' && sample.fixture.activeCaptures === 0 &&
        sample.preparedCaptureTokenCount === 0,
      'Five-second finalize did not preserve and then cancel pending hold intent',
      8_000,
    );
    check(afterPending.status === 'Idle' && afterPending.fixture.activeCaptures === 0 &&
      afterPending.preparedCaptureTokenCount === 0 &&
      afterPending.fixture.providerStarts === cancelledPending.fixture.providerStarts,
      'Released pending hold started after finalize');
    check(!afterPending.fixture.providerMarkers.some((range) => range.captureGeneration === cancelledGeneration),
      'Cancelled pending capture later delivered audio to a provider');
    assertMarkerEvidence(afterPending);
    await invoke('hide_recording_window_if_current', { windowEpoch: afterPending.windowEpoch });
    await configure({ stopDelayMs: 130 });
    report.scenarios.push('processing-ui-and-cancelled-pending-hold-start');

    // Prove that every marker captured while the previous provider finalizes is
    // delivered exactly once, in order, to only the replacement provider.
    for (const finalizeDelayMs of [0, 850, 5_000, 12_000]) {
      await progress(`queued-prebuffer-${finalizeDelayMs}ms-starting`);
      await configure({ stopDelayMs: finalizeDelayMs, audioDelayMs: 0, keepAlive: false });
      current = await start();
      if (finalizeDelayMs === 0) {
        await stop();
        assertMarkerEvidence(await state());
        report.scenarios.push('normal-start-marker-order-finalize-0ms');
        continue;
      }

      const previousRun = current;
      const stopPrevious = store.stopRecording(`native-e2e-queued-${finalizeDelayMs}`);
      await until(state, (sample) => sample.status === 'Processing' && sample.fixture.activeCaptures === 0,
        `Previous run never entered Processing for ${finalizeDelayMs}ms finalize`);
      await hotkey('release');
      await delay(60);
      const beforeQueuedIntent = await state();
      let queuedOpeningEpoch: number | null = null;
      let unassignedOpeningStarts = 0;
      const openingStartsByEpoch = new Map<number, number>();
      const openingSamples: Array<{ elapsedMs: number; animationName: string }> = [];
      const openingWatchStarted = Date.now();
      const recordQueuedOpening = (event: AnimationEvent) => {
        if (!event.animationName.startsWith('mini-pop-in') ||
          !(event.target instanceof Element) || !event.target.matches('.popover.mini')) return;
        openingSamples.push({ elapsedMs: Date.now() - openingWatchStarted, animationName: event.animationName });
        if (queuedOpeningEpoch === null) unassignedOpeningStarts += 1;
        else openingStartsByEpoch.set(queuedOpeningEpoch, (openingStartsByEpoch.get(queuedOpeningEpoch) ?? 0) + 1);
      };
      document.addEventListener('animationstart', recordQueuedOpening);
      await hotkey('press');
      const buffered = await until(async () => ({ backend: await state(), readiness: store.captureReadiness,
        green: !!document.querySelector('.mini-status-dot.recording:not(.starting):not(.processing)') }),
      (sample) => sample.backend.fixture.activeCaptures === 1 && sample.readiness?.state === 'buffering' && sample.green &&
        sample.backend.fixture.captureStartLatenciesMs.length > beforeQueuedIntent.fixture.captureStartLatenciesMs.length,
      `Replacement capture was not ready during ${finalizeDelayMs}ms finalize`, 2_000);
      queuedNativeCaptureStartLatenciesMs.push(buffered.backend.fixture.captureStartLatenciesMs[
        buffered.backend.fixture.captureStartLatenciesMs.length - 1
      ]);
      check(buffered.backend.fixture.activeProviders === 1 &&
        buffered.backend.fixture.providerStarts === beforeQueuedIntent.fixture.providerStarts,
      'Queued capture created a provider before the previous provider finalized');
      const markerBufferedBackend = await until(state,
        (sample) => {
          const generation = latestCaptureGeneration(sample);
          return generation > latestCaptureGeneration(beforeQueuedIntent) &&
            sample.fixture.captureRunAssociations.some((association) =>
              association.captureGeneration === generation && association.captureRunId === buffered.readiness?.runId);
        },
        `Queued capture produced no new PCM marker during ${finalizeDelayMs}ms finalize`, 2_000);
      const earlyRange = markerBufferedBackend.fixture.captureMarkers[
        markerBufferedBackend.fixture.captureMarkers.length - 1
      ];
      check(earlyRange && earlyRange.firstSequence === 1 && earlyRange.count > 0,
        'Queued capture did not record an early marker range');
      const captureAssociation = markerBufferedBackend.fixture.captureRunAssociations.find((association) =>
        association.captureGeneration === earlyRange.captureGeneration);
      check(captureAssociation && captureAssociation.captureRunId === buffered.readiness?.runId &&
        captureAssociation.captureFenceGeneration > 0,
      `Queued PCM marker was not fenced to the readiness run: ${JSON.stringify({ earlyRange, captureAssociation,
        readiness: buffered.readiness })}`);
      const bufferedWindowEpoch = buffered.backend.windowEpoch;
      queuedOpeningEpoch = bufferedWindowEpoch;
      openingStartsByEpoch.set(bufferedWindowEpoch,
        (openingStartsByEpoch.get(bufferedWindowEpoch) ?? 0) + unassignedOpeningStarts);
      try {
        await stopPrevious;
        current = await waitForNewRecording(previousRun, false, finalizeDelayMs + 8_000);
      } finally {
        document.removeEventListener('animationstart', recordQueuedOpening);
      }
      const streamed = await state();
      const openingCounts = Object.fromEntries(openingStartsByEpoch);
      const openingCount = [...openingStartsByEpoch.values()].reduce((sum, count) => sum + count, 0);
      check(bufferedWindowEpoch > beforeQueuedIntent.windowEpoch && streamed.windowEpoch === bufferedWindowEpoch &&
        openingCount === 1 && openingStartsByEpoch.get(bufferedWindowEpoch) === 1,
      `Buffering to streaming transition changed epoch or did not preserve exactly one initial opening: ${JSON.stringify({
        beforeEpoch: beforeQueuedIntent.windowEpoch, bufferedEpoch: bufferedWindowEpoch,
        streamedEpoch: streamed.windowEpoch, openingCounts, openingSamples })}`);
      check(streamed.fixture.finals === beforeQueuedIntent.fixture.finals + 1,
        'Previous provider tail was not finalized exactly once before replacement streaming');
      const replacementProviderSession = streamed.fixture.providerStarts + streamed.fixture.providerResumes;
      const delivered = streamed.fixture.providerMarkers.find((range) =>
        range.providerSessionId === replacementProviderSession &&
        range.captureGeneration === earlyRange.captureGeneration);
      check(delivered && delivered.firstSequence === 1 && delivered.lastSequence >= earlyRange.lastSequence,
        `Early queued audio was lost or delivered out of order: ${JSON.stringify({ earlyRange, delivered })}`);
      check(delivered.captureRunId === current.sessionId &&
        delivered.captureFenceGeneration === captureAssociation.captureFenceGeneration,
      'Streamed marker run/generation differs from its buffered capture fence');
      assertMarkerEvidence(streamed);
      report.observations.push({ scenario: 'queued-prebuffer-order', finalizeDelayMs,
        captureGeneration: earlyRange.captureGeneration, earlyCount: earlyRange.count,
        deliveredCount: delivered.count, providerSessionId: replacementProviderSession });
      await stop(finalizeDelayMs + 8_000);
      report.scenarios.push(`queued-prebuffer-order-finalize-${finalizeDelayMs}ms`);
    }
    await configure({ stopDelayMs: 130, audioDelayMs: 450 });

    await configure({ failNextStart: true, startDelayMs: 300, audioDelayMs: 0 });
    await progress('start-failure-ui-error');
    const beforeFailedStart = await state();
    const failureUiStarted = Date.now();
    const failureUiTransitions: unknown[] = [];
    const captureFailureUi = () => ({ elapsedMs: Date.now() - failureUiStarted, ui: uiSnapshot(),
      statusDotClass: document.querySelector('.mini-status-dot')?.className,
      errorText: document.querySelector('.error-message')?.textContent,
      error: !!document.querySelector('.mini-status-dot.error, .error-message') });
    const stopFailureTrace = watch(() => [store.status, store.error, store.sessionId], () => {
      if (failureUiTransitions.length < 64) failureUiTransitions.push(captureFailureUi());
    }, { flush: 'sync' });
    let lastFailureSample: unknown;
    try {
      await hotkey('press');
      const failedBuffered = await until(state, (sample) => sample.fixture.activeCaptures === 1 &&
        sample.fixture.captureMarkers.length > beforeFailedStart.fixture.captureMarkers.length,
      'Failed provider startup never captured deterministic prebuffer audio', 2_000);
      const failedCaptureGeneration = failedBuffered.fixture.captureMarkers[
        failedBuffered.fixture.captureMarkers.length - 1
      ].captureGeneration;
      await until(async () => {
        const sample = { backend: await state(), ...captureFailureUi() };
        lastFailureSample = sample;
        return sample;
      }, (s) => s.backend.fixture.activeCaptures === 0 && s.error,
      'Failed start did not expose UI error and release capture');
      const afterFailure = await state();
      check(!afterFailure.fixture.providerMarkers.some((range) =>
        range.captureGeneration === failedCaptureGeneration),
      'Failed provider startup delivered buffered audio to a stale provider');
      assertMarkerEvidence(afterFailure);
      report.observations.push({ scenario: 'start-failure-ui-error', transitions: failureUiTransitions, last: lastFailureSample });
    } catch (error) {
      report.failureContext = { scenario: 'start-failure-ui-error', transitions: failureUiTransitions, last: lastFailureSample };
      throw error;
    } finally { stopFailureTrace(); }
    await hotkey('release');
    current = await start();
    await stop();
    await configure({ startDelayMs: 0, audioDelayMs: 450 });
    report.scenarios.push('start-failure-then-successful-audio-retry');

    // A failed first connection emits Starting with a session, then clears native
    // ownership to zero. Retrying must treat that scoped stop as already stopped.
    await progress('failed-scoped-start-auto-retry-starting');
    await configure({ failNextStart: true, startDelayMs: 300 });
    await invoke('show_recording_window');
    const beforeRetryFailure = await state();
    const automaticRetry = store.startRecording();
    const startingFailure = await until(async () => ({ backend: await state(), uiSession: store.sessionId, starting: store.isStarting }),
      (s) => s.starting && s.backend.status === 'Starting' && s.backend.sessionId > 0 && s.uiSession === s.backend.sessionId,
      'Failure fixture did not expose the first scoped Starting event');
    // transcription:error closes the public UI session, while the private retry
    // operation retains the failed ID for its scoped cleanup before attempt two.
    const failedBackoff = await until(async () => ({ backend: await state(), uiSession: store.sessionId, connecting: store.isConnecting, attempt: store.connectAttempt }),
      (s) => s.connecting && s.attempt === 1 && s.uiSession === null &&
        s.backend.status === 'Idle' && s.backend.sessionId === 0 && s.backend.fixture.activeCaptures === 0 &&
        s.backend.fixture.providerFailures === beforeRetryFailure.fixture.providerFailures + 1,
      'Failed first connection did not clear native ownership during UI retry backoff');
    current = await waitForNewRecording(startingFailure.backend);
    await automaticRetry;
    check(!store.isConnecting && store.isRecording && store.sessionId === current.sessionId &&
      current.fixture.providerStarts === beforeRetryFailure.fixture.providerStarts + 1,
      'Automatic retry did not finish on its fresh Recording session');
    report.observations.push({ scenario: 'failed-scoped-start-auto-retry', startingFailure, failedBackoff,
      recovered: { backend: current, ui: uiSnapshot() } });
    // This session belongs to a UI start; there is no held hotkey to release.
    await store.stopRecording('native-e2e-auto-retry');
    const retryStopped = await until(state, (s) => s.status === 'Idle' && s.fixture.activeCaptures === 0,
      'UI stop after automatic retry did not release capture');
    await until(async () => store.isIdle, Boolean, 'UI stop after automatic retry left a non-Idle UI');
    check(retryStopped.fixture.activeProviders === 0 && retryStopped.fixture.activeCaptures === 0 &&
      retryStopped.fixture.captureStarts === retryStopped.fixture.captureStops,
      'Successful retry left native capture/provider resources alive');
    await invoke('hide_recording_window_if_current', { windowEpoch: retryStopped.windowEpoch });
    await configure({ startDelayMs: 0 });
    report.scenarios.push('failed-scoped-start-clears-native-session-and-auto-retries-with-fresh-audio');

    // UI retry backoff must lose ownership to a newer native hotkey session.
    await progress('ui-retry-native-replacement-starting');
    await configure({ failNextStart: true });
    const beforeUiFailure = await state();
    const failedUi = store.startRecording();
    await until(async () => ({ backend: await state(), connecting: store.isConnecting, attempt: store.connectAttempt }),
      (s) => s.connecting && s.attempt === 1 && s.backend.status === 'Idle' && s.backend.fixture.activeCaptures === 0 &&
        s.backend.fixture.providerFailures === beforeUiFailure.fixture.providerFailures + 1,
      'UI did not enter first retry backoff');
    current = await start();
    const ownedCounters = await state();
    await observe(2100, async () => { await failedUi; }, current.expected);
    const afterRetry = await state();
    check(afterRetry.fixture.providerStarts === ownedCounters.fixture.providerStarts &&
      afterRetry.fixture.captureStops === ownedCounters.fixture.captureStops && afterRetry.sessionId === current.sessionId,
      'Orphan UI retry started or stopped the hotkey replacement');
    await stop();
    report.scenarios.push('ui-retry-cancelled-by-new-native-hotkey');

    await progress('manual-stop-during-backoff-starting');
    await configure({ failNextStart: true });
    const beforeManualFailure = await state();
    const cancelledUi = store.startRecording();
    await until(async () => ({ backend: await state(), connecting: store.isConnecting, attempt: store.connectAttempt }),
      (s) => s.connecting && s.attempt === 1 && s.backend.status === 'Idle' && s.backend.fixture.activeCaptures === 0 &&
        s.backend.fixture.providerFailures === beforeManualFailure.fixture.providerFailures + 1,
      'Manual-cancel fixture did not enter retry backoff');
    await store.stopRecording('native-e2e-manual-cancel');
    const cancelledCounters = await state();
    const cancelledUntil = Date.now() + 2100;
    do {
      const snapshot = await state();
      check(snapshot.status === 'Idle' && snapshot.fixture.activeCaptures === 0 &&
        snapshot.fixture.providerStarts === cancelledCounters.fixture.providerStarts,
        'Manual stop during backoff resurrected a recording');
      await delay(20);
    } while (Date.now() < cancelledUntil);
    await cancelledUi;
    report.scenarios.push('manual-stop-cancels-ui-backoff');

    await progress('full-and-mini-layouts-starting');
    await mode(false);
    current = await start();
    await stop();
    await mode(true);
    current = await start();
    await stop();
    report.scenarios.push('full-and-mini-close-modes');

    await progress('hidden-idle-and-wake-starting');
    // Native monotonic time/heartbeats continue even when hidden WKWebView JS suspends.
    // Observe before issuing the command: native wake may precede IPC resolution by >1.2s.
    let wakeOpeningSeen = false;
    let wakeClosingAfterOpening = false;
    const wakeObserver = new MutationObserver((changes) => {
      // Reconstruct each target's successive class values in mutation order. An old
      // hidden-panel close removed by the first opening must not count as a new close.
      for (let index = 0; index < changes.length; index += 1) {
        const change = changes[index];
        const following = changes.slice(index + 1).find((next) => next.target === change.target);
        const classes = new Set((following ? following.oldValue : (change.target as Element).getAttribute('class'))?.split(/\s+/) || []);
        if (classes.has('mini-opening')) wakeOpeningSeen = true;
        if (wakeOpeningSeen && classes.has('mini-closing')) wakeClosingAfterOpening = true;
      }
    });
    wakeObserver.observe(document.documentElement, { subtree: true, attributes: true, attributeFilter: ['class'], attributeOldValue: true });
    try {
      const idleBegin = Date.now();
      report.lastProgress = { phase: 'native-hidden-idle', durationMs: 180_000 };
      const idleResult = await invoke<{
        hiddenIdleMs: number; before: NativeState; firstVisibleMs: number;
        wakeSamples: Array<{ elapsedMs: number; visible: boolean; status: string; sessionId: number; windowEpoch: number; providerAudioChunks: number }>;
        visibilityTransitions: Array<{ elapsedMs: number; visible: boolean; windowEpoch: number }>;
      }>('native_e2e_idle_then_press', { durationMs: 180_000 });
      check(Number.isFinite(idleResult.hiddenIdleMs) && idleResult.hiddenIdleMs >= 180_000 &&
        Date.now() - idleBegin >= 180_000, 'Hidden idle did not last 180 seconds of real native and wall time');
      check(idleResult.before.status === 'Idle' && !idleResult.before.visible &&
        idleResult.before.fixture.activeCaptures === 0, 'Native hidden-idle baseline was not idle/hidden');
      check(Number.isFinite(idleResult.firstVisibleMs) && idleResult.firstVisibleMs >= 0 && idleResult.wakeSamples.length > 1,
        'Native wake visibility observation is missing');
      const firstVisibleIndex = idleResult.wakeSamples.findIndex((sample) => sample.visible);
      const visibleSamples = firstVisibleIndex < 0 ? [] : idleResult.wakeSamples.slice(firstVisibleIndex);
      check(visibleSamples.length > 1 && visibleSamples.every((sample) => sample.visible) &&
        visibleSamples[visibleSamples.length - 1].elapsedMs - idleResult.firstVisibleMs >= 1200,
        'Native wake did not remain visible throughout the full initial 1200ms');
      report.hiddenIdleMs = idleResult.hiddenIdleMs;
      report.observations.push({ nativeHiddenIdleMs: idleResult.hiddenIdleMs, webviewElapsedMs: Date.now() - idleBegin,
        beforeNativeWake: idleResult.before, firstVisibleMs: idleResult.firstVisibleMs,
        positiveWakeSamples: visibleSamples.length, wakeSamples: idleResult.wakeSamples, visibilityTransitions: idleResult.visibilityTransitions });
      const checkWakeClosing = () => {
        if (wakeClosingAfterOpening) {
          report.failureContext = { phase: 'native-wake-initial-appearance', wakeOpeningSeen, wakeClosingAfterOpening,
            actualDomText: domText(), ui: uiSnapshot(), firstVisibleMs: idleResult.firstVisibleMs, wakeSamples: idleResult.wakeSamples };
          throw new Error(`Mini panel closed after its first native wake opening: ${JSON.stringify(report.failureContext)}`);
        }
      };
      checkWakeClosing();
      // Do not press again: the native command has already started this exact new session.
      current = await waitForNewRecording(idleResult.before, false);
      check(wakeOpeningSeen, 'Positive control: native wake never reached the real mini opening animation');
      checkWakeClosing();
      await observe(800, async () => {}, current.expected);
      checkWakeClosing();
    } finally { wakeObserver.disconnect(); }
    await stop();
    report.scenarios.push('real-hidden-idle-180s-and-fresh-audio');
    const final = await state();
    assertMarkerEvidence(final);
    check(final.preparedCaptureTokenCount === 0,
      'Prepared capture token registry retained entries after full teardown');
    const captureLatencyP95 = [...nativeCaptureStartLatenciesMs].sort((a, b) => a - b)[
      Math.max(0, Math.ceil(nativeCaptureStartLatenciesMs.length * 0.95) - 1)
    ];
    const queuedCaptureLatencyP95 = [...queuedNativeCaptureStartLatenciesMs].sort((a, b) => a - b)[
      Math.max(0, Math.ceil(queuedNativeCaptureStartLatenciesMs.length * 0.95) - 1)
    ];
    check(nativeCaptureStartLatenciesMs.length >= 20 && captureLatencyP95 <= 250,
      `Native hotkey-to-capture-start p95 exceeded 250ms: ${captureLatencyP95}ms`);
    check(queuedNativeCaptureStartLatenciesMs.length === 3 && queuedCaptureLatencyP95 <= 250,
      `Queued native hotkey-to-capture-start p95 exceeded 250ms: ${queuedCaptureLatencyP95}ms`);
    check(final.fixture.captureStarts - baseline.fixture.captureStarts === final.fixture.captureStops - baseline.fixture.captureStops,
      'Capture start/stop counters do not balance');
    check(final.fixture.activeCaptures === 0, 'Capture remained active after final stop');
    check(final.fixture.finals - baseline.fixture.finals === successfulStarts, 'Completed provider sessions/finals differ from exact successful recordings');
    check(transcripts.size === successfulStarts && sessions.size === successfulStarts, 'Unique session/transcript count mismatch');
    check(listeningSeen && recordingSeen && processingSeen, 'Missing positive UI status controls');
    report.observations.push({ successfulStarts, distinctTranscripts: transcripts.size,
      nativeHotkeyToCaptureStartMs: { samples: nativeCaptureStartLatenciesMs,
        p95: captureLatencyP95, queuedSamples: queuedNativeCaptureStartLatenciesMs, queuedP95: queuedCaptureLatencyP95,
        qualification: 'Native monotonic dispatch-to-fake-capture timing measures application scheduling; physical device-open latency requires separate evidence.' },
      baseline: baseline.fixture, final: final.fixture });
    const firstPcm = ((await state()).fixture.firstPcmLatenciesMs ?? [])
      .filter(row => row.configuredDelayMs === 0).map(row => row.elapsedMs).sort((a,b) => a-b);
    const firstPcmP95 = firstPcm[Math.ceil(firstPcm.length * 0.95) - 1];
    report.observations.push({ firstPcm: {samples: firstPcm.length, p95Ms: firstPcmP95,
      source: 'synthetic capture, real native intent and callback dispatch; not physical microphone opening'} });
    check(firstPcm.length >= 50 && firstPcmP95 <= 250, 'Prepared fixture first PCM p95 exceeds 250ms');
    const meter = nativeMeterEvidence();
    report.observations.push({ nativeMeter: meter });
    check(meter.runs >= 50 && meter.samples >= 50, 'Meter provenance lacks 50 run samples');
    check(meter.invalidClockSamples === 0, 'Clock discontinuity invalidates meter ages');
    check(meter.p95AgeMs !== null && meter.p95AgeMs <= 100, 'Meter sample age p95 exceeds 100ms');
    check(meter.maxAgeMs !== null && meter.maxAgeMs < 1000, 'Meter rendered seconds-old audio despite p95 passing');
    report.passed = true;
    await progress('complete');
  } catch (error) {
    report.error = error instanceof Error ? `${error.message}\n${error.stack || ''}` : String(error);
    console.error('[native-e2e] FAIL', report.error);
  }
  report.elapsedMs = Date.now() - runStarted;
  if (confirmed) await invoke('native_e2e_finish', { report });
}

/** ASR and native paste stay real; only the microphone source is synthetic PCM. */
async function runLiveNativeScenario(pinia: Pinia): Promise<void> {
  const started = Date.now();
  const report = { mode: 'live-elevenlabs', passed: false, elapsedMs: 0, targetDocument: '',
    finalText: '', pcmBytes: 0, actualPasteVerified: false, error: '' };
  try {
    const config = useAppConfigStore(pinia);
    const store = useTranscriptionStore(pinia);
    await config.startSync();
    await invoke('update_app_config', { showMiniRecordingWindow: true, holdToRecord: false,
      hideRecordingWindowOnHotkey: false, playCompletionSound: false,
      autoCopyToClipboard: false, autoPasteText: true });
    await config.refresh();
    await invoke('native_e2e_progress', { report: { scenario: 'live-target-preparation' } });
    check(await invoke<boolean>('check_accessibility_permission'), 'Native test binary needs macOS Accessibility permission before real paste');
    report.targetDocument = await invoke<string>('native_e2e_prepare_live_target');
    await invoke('native_e2e_delay', { durationMs: 500 });
    const waitNative = async (accept: (s: NativeState) => boolean, message: string, timeout: number) => {
      const epoch = Date.now();
      while (Date.now() - epoch < timeout) {
        if (accept(await state())) return;
        await invoke('native_e2e_delay', {durationMs: 50});
      }
      throw new Error(message);
    };
    await hotkey('press'); await hotkey('release');
    await waitNative(s => s.status === 'Recording', 'Live provider never reached Recording', 15000);
    await invoke('native_e2e_progress', { report: { scenario: 'live-recording' } });
    await waitNative(s => s.fixture.livePcmBytes === 788288, 'Synthetic PCM capture incomplete', 35000);
    await invoke('native_e2e_progress', { report: { scenario: 'live-PCM-complete' } });
    await hotkey('press'); await hotkey('release');
    await waitNative(s => s.status === 'Idle' && s.fixture.activeCaptures === 0,
      'Live recording failed to finish', 35000);
    // Wait on a native timer because the successful stop may have hidden WebView.
    await invoke('native_e2e_delay', { durationMs: 2000 });
    report.finalText = store.finalText;
    report.pcmBytes = (await state()).fixture.livePcmBytes ?? 0;
    check(report.finalText.includes('книга') && report.finalText.includes('самол'), 'Live transcript lost start/tail');
    report.passed = true;
  } catch (error) { report.error = String(error); }
  report.elapsedMs = Date.now() - started;
  await invoke('native_e2e_finish', { report });
}


/** Focused native terminal proof; does not claim the separate 50-cycle soak. */
async function runNativeTerminalScenario(pinia: Pinia) {
  const report = { mode: 'terminal-cleanup', passed: false, sleepReleased: false,
    wakeDidNotRestart: false, explicitRestart: false, holdSleepReleased: false,
    retiredHoldReleaseIgnored: false, holdRestartReleased: false, deviceErrorObserved: false,
    deviceReleased: false, error: '', elapsedMs: 0 };
  const started = Date.now();
  const errors: Array<{ session_id: number; error: string }> = [];
  const unlisten = await listen<{ session_id: number; error: string }>('transcription:error', event => errors.push(event.payload));
  const poll = async (predicate: (s: NativeState) => boolean, label: string) => {
    const deadline = performance.now() + 10_000;
    do {
      const current = await state();
      if (predicate(current)) return current;
      await invoke('native_e2e_delay', { durationMs: 30 });
    } while (performance.now() < deadline);
    throw new Error(label);
  };
  const released = (s: NativeState) => s.fixture.activeCaptures === 0 &&
    s.fixture.activeProviders === 0 && s.preparedCaptureTokenCount === 0;
  const press = async () => { await hotkey('press'); await hotkey('release'); };
  const start = async () => {
    const before = await state();
    await invoke('native_e2e_delay', { durationMs: 200 });
    await press();
    return poll(s => s.status === 'Recording' && s.fixture.captureStarts > before.fixture.captureStarts &&
      s.fixture.providerAudioChunks > before.fixture.providerAudioChunks, 'No new capture/audio after explicit intent');
  };
  try {
    await poll(s => s.ready, 'Fixture did not become ready');
    const config = useAppConfigStore(pinia);
    await config.startSync();
    await invoke('update_app_config', { holdToRecord: false, showMiniRecordingWindow: true,
      hideRecordingWindowOnHotkey: true, playCompletionSound: false, autoCopyToClipboard: false, autoPasteText: false });
    await config.refresh();
    await invoke('native_e2e_configure', { config: {audioDelayMs: 20, startDelayMs: 0, stopDelayMs: 130, keepAlive: false} });
    await start();
    await invoke('native_e2e_hotkey', {action: 'sleep'});
    const slept = await poll(released, 'Sleep did not release resources');
    report.sleepReleased = true;
    await invoke('native_e2e_hotkey', {action: 'wake'});
    await invoke('native_e2e_delay', {durationMs: 300});
    const woke = await state();
    check(released(woke) && woke.fixture.captureStarts === slept.fixture.captureStarts &&
      woke.fixture.providerStarts === slept.fixture.providerStarts && woke.fixture.providerResumes === slept.fixture.providerResumes,
    'Wake restarted a cancelled run');
    report.wakeDidNotRestart = true;
    await start();
    await press();
    await poll(released, 'Explicit restart did not stop cleanly');
    report.explicitRestart = true;
    await invoke('update_app_config', { holdToRecord: true });
    await config.refresh();
    const beforeHold = await state();
    await hotkey('press');
    await poll(s => s.status === 'Recording' && s.fixture.providerAudioChunks > beforeHold.fixture.providerAudioChunks,
      'Hold did not start before sleep');
    await invoke('native_e2e_hotkey', {action: 'sleep'});
    const heldSleep = await poll(released, 'Sleep did not release held capture');
    report.holdSleepReleased = true;
    await invoke('native_e2e_hotkey', {action: 'wake'});
    // Complete the physical gesture interrupted by sleep. Fake input has no
    // physical-key watcher; omitting this release would leave intentional debt.
    await hotkey('release');
    await invoke('native_e2e_delay', {durationMs: 100});
    const retired = await state();
    report.retiredHoldReleaseIgnored = released(retired) &&
      retired.fixture.captureStarts === heldSleep.fixture.captureStarts;
    check(report.retiredHoldReleaseIgnored, 'Retired pre-sleep release revived capture');
    await hotkey('press');
    await poll(s => s.status === 'Recording' && s.fixture.providerAudioChunks > retired.fixture.providerAudioChunks,
      'Fresh hold after wake did not start');
    await hotkey('release');
    await poll(released, 'Fresh hold after wake did not stop');
    report.holdRestartReleased = true;
    await invoke('update_app_config', { holdToRecord: false });
    await config.refresh();
    const failed = await start();
    await invoke('native_e2e_hotkey', {action: 'device-loss'});
    await poll(released, 'Native-thread device loss did not release resources');
    await invoke('native_e2e_delay', {durationMs: 200});
    report.deviceErrorObserved = errors.some(e => e.session_id === failed.sessionId && e.error.includes('injected native device loss'));
    check(report.deviceErrorObserved, 'Run-scoped native device error was not delivered');
    report.deviceReleased = released(await state());
    report.passed = report.deviceReleased;
  } catch (error) { report.error = String(error); }
  finally { unlisten(); }
  report.elapsedMs = Date.now() - started;
  await invoke('native_e2e_finish', { report });
}

/** E54 deliberately never calls finish: parent owns crash/termination. */
async function runRestartCrashScenario(pinia: Pinia): Promise<void> {
  const report = { mode: 'restart-crash', passed: false, errors: [] as string[], ui: {} };
  const unlisten = await listen('transcription:error', event => report.errors.push(JSON.stringify(event.payload)));
  try {
    const store = useTranscriptionStore(pinia);
    await store.initialize();
    report.ui = { error: store.error };
    check(store.error === null, `E54 store initialization failed: ${store.error}`);
    const config = useAppConfigStore(pinia);
    const configSynced = await config.startSync();
    check(configSynced === true, 'E54 config synchronization failed');
    let reconciledStatus: string | null = null;
    const initial = await invoke<NativeState & { restartPhase: string }>('native_e2e_state');
    if (initial.restartPhase === 'A') {
      await invoke('update_app_config', { holdToRecord: false, showMiniRecordingWindow: true,
        hideRecordingWindowOnHotkey: true, autoCopyToClipboard: false, autoPasteText: false, playCompletionSound: false });
      await config.refresh();
      await invoke('native_e2e_configure', { config: { audioDelayMs: 0, stopDelayMs: 0, keepAlive: false, controlDelayMs: 0 } });
      await hotkey('press'); await hotkey('release');
      await until(state, s => s.fixture.providerAudioChunks > 0, 'E54 A never wrote fake audio');
      await delay(120);
      await hotkey('press'); await hotkey('release');
      await until(() => invoke<NativeState & { pausedContinuation: unknown }>('native_e2e_state'),
        s => s.pausedContinuation !== null && s.fixture.activeCaptures === 0 && s.fixture.activeProviders === 1,
        'E54 A never reached genuine paused context');
    } else {
      check(initial.restartPhase === 'B', 'Invalid restart phase');
      reconciledStatus = await store.reconcileBackendStatus('native_e54_restart');
      check(reconciledStatus === 'Idle', 'E54 backend status reconciliation failed');
    }
    report.ui = { error: store.error, configSynced, reconciledStatus, status: store.status, desiredOn: store.recordingDesiredOn,
      pendingStart: store.recordingStartPending, canRequestContinuation: store.canRequestContinuation };
    check(store.error === null, `E54 store error before checkpoint: ${store.error}`);
    report.passed = report.errors.length === 0;
  } catch (error) { report.errors.push(String(error)); }
  await invoke('native_e2e_progress', { report });
  unlisten();
}
