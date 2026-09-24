import type { Pinia } from 'pinia';
import { nextTick } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { i18n } from '@/i18n';
import { useAppConfigStore } from '@/stores/appConfig';
import { useTranscriptionStore } from '@/stores/transcription';

type Snapshot = { visible: boolean; status: string; sessionId: number; windowEpoch: number; limitErrorEmissions: number;
  fixture: { captureStarts: number; captureStops: number; providerStarts: number;
    providerAudioChunks: number; activeCaptures: number; activeProviders: number;
    observationOverflow: boolean; markerViolations: string[] } };
type ErrorEvent = { session_id: number; error_type: string; error_details?: { category?: string; serverCode?: string } };
const state = () => invoke<Snapshot>('native_e2e_state');
const wait = (durationMs: number) => invoke('native_e2e_delay', { durationMs });
function check(valid: unknown, message: string): asserts valid {
  if (!valid) throw new Error(message);
}
async function poll(accept: (value: Snapshot) => boolean, label: string, timeoutMs = 8_000) {
  const deadline = performance.now() + timeoutMs;
  let last: Snapshot | null = null;
  do { const current = await state(); last = current; if (accept(current)) return current; await wait(25); }
  while (performance.now() < deadline);
  throw new Error(`${label}: ${JSON.stringify({ visible: last?.visible, status: last?.status,
    windowEpoch: last?.windowEpoch, limitErrorEmissions: last?.limitErrorEmissions,
    activeCaptures: last?.fixture.activeCaptures, activeProviders: last?.fixture.activeProviders,
    captureStarts: last?.fixture.captureStarts, providerStarts: last?.fixture.providerStarts })}`);
}
async function toggle() {
  await invoke('native_e2e_hotkey', { action: 'press' });
  await invoke('native_e2e_hotkey', { action: 'release' });
}

/** Uses the fixture transport callback and production store, projection, Vue and NSPanel. */
export async function runNativeLimitErrorScenario(pinia: Pinia) {
  const store = useTranscriptionStore(pinia);
  const report = { mode: 'limit-error', passed: false, errors: [] as string[],
    errorEvents: [] as ErrorEvent[], a: null as Snapshot | null, errorFrame: null as Snapshot | null,
    afterIdle: null as Snapshot | null, dismissed: null as Snapshot | null,
    b: null as Snapshot | null, afterStale: null as Snapshot | null, final: null as Snapshot | null,
    localizedText: '', licenseAction: '', elapsedMs: 0,
    failure: null as null | { native: Snapshot | null; store: { status: string; errorType: string | null; error: string | null } } };
  const began = performance.now();
  const unlisten = await listen<ErrorEvent>('transcription:error', event => report.errorEvents.push(event.payload));
  try {
    const config = useAppConfigStore(pinia);
    await config.startSync();
    await invoke('update_app_config', { holdToRecord: false, showMiniRecordingWindow: false,
      hideRecordingWindowOnHotkey: true, autoCopyToClipboard: false, autoPasteText: false,
      playCompletionSound: false });
    await config.refresh();
    // Wait for the component's real listener readiness before the first hotkey.
    // Otherwise fixture startup can overtake its window-shown registration.
    await poll(() => {
      const start = document.querySelector<HTMLButtonElement>('.record-button');
      return Boolean(start && !start.disabled);
    }, 'Recording UI listeners never became ready');
    await invoke('update_app_config', { showMiniRecordingWindow: true });
    await config.refresh();
    await nextTick();
    await invoke('native_e2e_configure', { config: { audioDelayMs: 0, stopDelayMs: 0, keepAlive: false } });
    await toggle();
    report.a = await poll(s => s.status === 'Recording' && s.visible && s.fixture.captureStarts === 1 &&
      s.fixture.providerAudioChunks > 0, 'A never reached visible recording with real provider callback');
    await invoke('native_e2e_configure', { config: { emitLimitError: 'A' } });
    report.errorFrame = await poll(s => s.limitErrorEmissions === 1 && s.fixture.activeCaptures === 0 &&
      s.fixture.activeProviders === 0 && s.visible && store.errorType === 'limit_exceeded',
    'Limit error did not retain visible panel after resource cleanup');
    check(report.errorEvents.some(e => e.session_id === report.a!.sessionId &&
      e.error_type === 'limit_exceeded' && e.error_details?.category === 'limit_exceeded' &&
      e.error_details?.serverCode === 'LIMIT_EXCEEDED'),
    'The real callback did not deliver structured LIMIT_EXCEEDED');
    await nextTick();
    const message = document.querySelector<HTMLElement>('.mini-transcription-text');
    const license = [...document.querySelectorAll<HTMLButtonElement>('.mini-actions button')]
      .find(button => button.title === i18n.global.t('errors.actions.activateLicense'));
    const expectedText = i18n.global.t('errors.limitExceededShort');
    report.localizedText = message?.textContent?.trim() ?? '';
    report.licenseAction = license?.title ?? '';
    check(message && message.getBoundingClientRect().width > 0 &&
      report.localizedText === expectedText && !report.localizedText.includes('Native fixture') &&
      message.scrollWidth <= message.clientWidth + 2 && message.scrollHeight <= message.clientHeight + 2,
    'Localized limit message is absent or clipped');
    check(license && license.getBoundingClientRect().width > 0,
      'Visible license action is missing from the error panel');
    // Outlive the normal delayed Idle/projection auto-hide window, observing every sample.
    for (let elapsed = 0; elapsed < 1800; elapsed += 100) {
      await wait(100);
      const current = await state();
      check(current.visible && store.errorType === 'limit_exceeded' &&
        current.fixture.activeCaptures === 0 && current.fixture.activeProviders === 0,
      'Delayed Idle or projection hid the limit panel or retained resources');
      report.afterIdle = current;
    }
    const dismiss = [...document.querySelectorAll<HTMLButtonElement>('.mini-actions button')]
      .find(button => button.title === i18n.global.t('main.minimize'));
    check(dismiss, 'User dismiss control is missing');
    dismiss.click();
    report.dismissed = await poll(s => !s.visible, 'User dismiss did not hide NSPanel');
    await toggle();
    report.b = await poll(s => s.status === 'Recording' && s.visible && s.fixture.captureStarts === 2 &&
      s.fixture.activeCaptures === 1 && s.fixture.providerAudioChunks > report.a!.fixture.providerAudioChunks,
    'B did not start after dismissing A');
    await invoke('native_e2e_configure', { config: { emitLimitError: 'stale-A' } });
    for (let elapsed = 0; elapsed < 500; elapsed += 50) {
      await wait(50);
      const current = await state();
      check(current.visible && current.windowEpoch === report.b.windowEpoch &&
        current.status === 'Recording' && current.fixture.activeCaptures === 1 &&
        current.fixture.captureStarts === 2 && store.errorType !== 'limit_exceeded',
      'Stale A callback affected B panel or capture');
      report.afterStale = current;
    }
    check(report.afterStale?.limitErrorEmissions === 2, 'Stale A callback was not invoked');
    await invoke('stop_recording');
    report.final = await poll(s => s.fixture.activeCaptures === 0 && s.fixture.activeProviders === 0 &&
      s.fixture.captureStops === 2, 'B did not release resources through normal Stop');
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error));
    report.failure = { native: await state().catch(() => null),
      store: { status: String(store.status), errorType: store.errorType, error: store.error } };
  }
  finally { unlisten(); }
  report.elapsedMs = performance.now() - began;
  await invoke('native_e2e_finish', { report });
}
