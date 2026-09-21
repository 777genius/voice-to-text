import type { Pinia } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { useAppConfigStore } from '@/stores/appConfig';
import { boundedSyntheticText, syntheticPhraseOccurrences } from './nativeContinuationMetrics';
import { nativeLivePreflight } from './nativeContinuationLive';
import { validateWarmProviderCanaryPlan,
  type WarmProviderCanaryTrial as Trial } from './nativeWarmProviderCanaryPlan';

type SourceEpisode = { name: string; captureGeneration: number; sourceFrames: number;
  emittedFrames: number; nativeSourceStartMs: number | null; nativeSourceEndMs: number | null;
  sourceGateRequired: boolean; sourceGateReady: { serverReady: boolean; emittedFrames: number } | null };
type NativeState = { status: string; logicalProviderRunId: number; preparedCaptureTokenCount: number;
  qualificationTrial: Trial; qualificationEndpoint: string;
  providerTransport: { serverReady: boolean; connectionRetained: boolean } | null;
  fixture: { captureStarts: number; captureStops: number; activeCaptures: number; maxActiveCaptures: number;
    observationOverflow: boolean; markerViolations: string[];
    sourceEpisodes: SourceEpisode[];
    captureRunAssociations: Array<{ captureRunId: number; captureFenceGeneration: number; captureGeneration: number }>;
    capturePcmLedgers: Array<{ captureGeneration: number; chunks: number; samples: number; hash: string }> } };
type ProviderEvent = { event: string; atMs: number; cycleIndex: number | null; sessionId: number;
  deliverySeq: number | null; text: string | null; markerIds: number[] };

const state = (stopReadback = false) => invoke<NativeState>('native_e2e_state', { stopReadback });
const wait = (durationMs: number) => invoke('native_e2e_delay', { durationMs });
function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
async function toggle() {
  await invoke('native_e2e_hotkey', { action: 'press' });
  await invoke('native_e2e_hotkey', { action: 'release' });
}

export async function runNativeWarmProviderCanary(pinia: Pinia) {
  const started = performance.now();
  const now = () => performance.now() - started;
  let activeCycle: number | null = null;
  const subscriptions: Array<() => void> = [];
  const report = { mode: 'warm-provider-canary', passed: false, trialId: '', targetDocument: '',
    expectedInsertion: '', actualPasteVerified: false, cycles: [] as Array<Record<string, unknown>>,
    events: [] as ProviderEvent[], terminalSessions: [] as number[], duplicateDeliveries: [] as string[],
    errors: [] as string[], final: null as NativeState | null, elapsedMs: 0 };
  const poll = async (accept: (value: NativeState) => boolean, label: string, timeoutMs: number) => {
    const deadline = performance.now() + timeoutMs;
    let last: NativeState | null = null;
    while (performance.now() < deadline) {
      last = await state();
      if (accept(last)) return last;
      await wait(20);
    }
    throw new Error(`${label}: ${JSON.stringify(last)}`);
  };
  try {
    const initial = await state();
    const trial = initial.qualificationTrial;
    validateWarmProviderCanaryPlan(trial);
    report.trialId = trial.id;
    const seenDeliveries = new Set<string>();
    const store = await nativeLivePreflight(pinia, initial.qualificationEndpoint, subscriptions,
      (name, payload) => {
        if (report.events.length >= 2048) { report.errors.push('Provider event evidence overflow'); return; }
        const sessionId = Number(payload.session_id);
        const deliverySeq = typeof payload.delivery_seq === 'number' ? payload.delivery_seq : null;
        const bounded = boundedSyntheticText(payload.text);
        if (!bounded.syntheticTextValid) report.errors.push('Provider text evidence overflow');
        const deliveryKey = deliverySeq === null ? null : `${sessionId}:${deliverySeq}:${name}`;
        if (deliveryKey && seenDeliveries.has(deliveryKey)) report.duplicateDeliveries.push(deliveryKey);
        if (deliveryKey) seenDeliveries.add(deliveryKey);
        const markerIds = [...new Set(syntheticPhraseOccurrences(bounded.rawSyntheticText ?? '')
          .map(marker => marker.markerId))];
        report.events.push({ event: name, atMs: now(), cycleIndex: activeCycle, sessionId,
          deliverySeq, text: bounded.rawSyntheticText, markerIds });
        if (name === 'transcription:terminal') {
          if (payload.delivery_complete !== true || payload.error) report.errors.push('Incomplete provider terminal');
          report.terminalSessions.push(sessionId);
        }
        if (name === 'transcription:error') report.errors.push(JSON.stringify(payload));
      }, { keepAlive: true, autoPasteText: false });
    report.targetDocument = await invoke<string>('native_e2e_prepare_live_target');
    await wait(500);

    for (const cycle of trial.cycles) {
      activeCycle = cycle.index;
      const eventStart = report.events.length;
      const before = await state();
      const startedAtMs = now();
      await toggle();
      const active = await poll(value => value.fixture.captureStarts === before.fixture.captureStarts + 1 &&
        value.fixture.activeCaptures === 1 && value.fixture.sourceEpisodes.length === cycle.index + 1,
      `cycle ${cycle.index} capture start`, 15_000);
      const generation = cycle.index + 1;
      const source = () => state().then(value => value.fixture.sourceEpisodes[cycle.index]);
      let trigger: Record<string, unknown> = {};
      if (cycle.stopPhase === 'before-ready') {
        check(active.status !== 'Recording' || active.providerTransport?.serverReady !== true,
          `cycle ${cycle.index} reached provider Ready before early stop`);
        trigger = { readyBeforeStop: false };
      } else {
        const ready = await poll(value => value.status === 'Recording' &&
          value.providerTransport?.serverReady === true && value.providerTransport.connectionRetained === true &&
          value.fixture.sourceEpisodes[cycle.index]?.emittedFrames === 0,
        `cycle ${cycle.index} actual provider Ready`, 30_000);
        check(ready.fixture.sourceEpisodes[cycle.index].sourceGateRequired === true,
          `cycle ${cycle.index} source was not held behind provider Ready`);
        await invoke('native_e2e_configure', { config: { sourceGateReady: true } });
      }
      if (cycle.stopPhase === 'after-first-pcm') {
        const observed = await poll(value => (value.fixture.sourceEpisodes[cycle.index]?.emittedFrames ?? 0) > 0,
          `cycle ${cycle.index} first PCM`, 30_000);
        trigger = { emittedFrames: observed.fixture.sourceEpisodes[cycle.index].emittedFrames };
      } else if (cycle.stopPhase !== 'before-ready') {
        const wanted = cycle.stopPhase === 'during-partial' ? 'transcription:partial' : 'transcription:final';
        const deadline = performance.now() + 70_000;
        while (performance.now() < deadline && !report.events.slice(eventStart).some(event => event.event === wanted)) await wait(20);
        check(report.events.slice(eventStart).some(event => event.event === wanted),
          `cycle ${cycle.index} never observed ${wanted}`);
        trigger = { event: wanted };
      }
      const beforeStop = await state();
      const association = beforeStop.fixture.captureRunAssociations.find(row => row.captureGeneration === generation) ?? null;
      if (cycle.stopPhase !== 'before-ready') {
        check(association?.captureRunId === beforeStop.logicalProviderRunId && association.captureFenceGeneration > 0,
          `cycle ${cycle.index} lost capture generation to logical run ownership`);
      }
      const triggerAtMs = now();
      await toggle();
      const stopped = await poll(value => value.fixture.activeCaptures === 0 &&
        value.fixture.captureStops === before.fixture.captureStops + 1, `cycle ${cycle.index} capture stop`, 5_000);
      const captureStoppedAtMs = now();
      await poll(value => value.status === 'Idle', `cycle ${cycle.index} terminal drain`, 45_000);
      const idleAtMs = now();
      const sourceAtStop = await source();
      check(sourceAtStop?.name === cycle.episode && sourceAtStop.captureGeneration === generation,
        `cycle ${cycle.index} source identity mismatch`);
      report.cycles.push({ ...cycle, startedAtMs, triggerAtMs,
        captureStoppedAtMs,
        idleAtMs, captureGeneration: generation,
        logicalRunId: beforeStop.logicalProviderRunId, association, trigger, source: sourceAtStop,
        eventStart, eventEnd: report.events.length, activeCapturesAfterStop: stopped.fixture.activeCaptures });
      await invoke('native_e2e_progress', { report: { scenario: 'warm-provider-churn',
        completedCycles: cycle.index + 1, stopPhase: cycle.stopPhase } });
      await wait(cycle.jitterMs);
    }

    activeCycle = trial.finalEpisodeIndex;
    const config = useAppConfigStore(pinia);
    await invoke('update_app_config', { autoPasteText: true });
    await config.refresh();
    const beforeFinal = await state();
    await toggle();
    const ready = await poll(value => value.status === 'Recording' &&
      value.providerTransport?.serverReady === true && value.providerTransport.connectionRetained === true &&
      value.fixture.sourceEpisodes.length === trial.finalEpisodeIndex + 1,
    'final full proof provider Ready', 30_000);
    check(ready.fixture.sourceEpisodes[trial.finalEpisodeIndex].emittedFrames === 0,
      'Final source escaped the real provider Ready gate');
    await invoke('native_e2e_configure', { config: { sourceGateReady: true } });
    const complete = await poll(value => {
      const source = value.fixture.sourceEpisodes[trial.finalEpisodeIndex];
      return source?.emittedFrames === source?.sourceFrames && typeof source.nativeSourceEndMs === 'number';
    }, 'final full PCM proof', 90_000);
    const finalAssociation = complete.fixture.captureRunAssociations.find(row =>
      row.captureGeneration === trial.finalEpisodeIndex + 1);
    check(finalAssociation?.captureRunId === complete.logicalProviderRunId,
      'Final proof generation does not belong to the live provider run');
    await toggle();
    await poll(value => value.fixture.captureStops === beforeFinal.fixture.captureStops + 1 &&
      value.fixture.activeCaptures === 0 && value.status === 'Idle' &&
      value.providerTransport?.connectionRetained === true, 'final full proof pause', 45_000);
    await wait(2_000);
    report.expectedInsertion = store.finalText;
    const finalMarkers = new Set(report.events.filter(event => event.cycleIndex === trial.finalEpisodeIndex)
      .flatMap(event => event.markerIds));
    check(report.expectedInsertion.trim().length > 0 && finalMarkers.size >= 2,
      'Final full PCM did not produce the complete multi-phrase transcript proof');
    // Retire the paused provider through the production configuration invalidation
    // path, after proving that the final capture reused the warm connection.
    await invoke('update_stt_config', { provider: 'backend', language: 'en',
      backendStreamingProvider: 'elevenlabs' });
    await poll(value => value.status === 'Idle' &&
      value.providerTransport?.connectionRetained === false, 'final provider release', 15_000);
    report.final = await state(true);
    check(report.final.fixture.captureStarts === 21 && report.final.fixture.captureStops === 21 &&
      report.final.fixture.activeCaptures === 0 && report.final.fixture.maxActiveCaptures === 1 &&
      report.final.fixture.sourceEpisodes.length === 21 && report.final.preparedCaptureTokenCount === 0,
    'Warm provider canary capture lifecycle is unbalanced');
    check(report.final.fixture.observationOverflow === false && report.final.fixture.markerViolations.length === 0 &&
      report.duplicateDeliveries.length === 0 && report.errors.length === 0,
    'Warm provider canary retained invalid or duplicate evidence');
    const generations = report.final.fixture.capturePcmLedgers.map(row => row.captureGeneration);
    check(generations.length === 21 && new Set(generations).size === 21 &&
      generations.every((generation, index) => generation === index + 1),
    'Warm provider canary PCM generations are incomplete');
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error));
  } finally {
    activeCycle = null;
    try {
      const current = await state();
      if (current.fixture.activeCaptures > 0) await invoke('stop_recording');
    } catch (error) { report.errors.push(String(error)); }
    subscriptions.forEach(unlisten => unlisten());
    if (!report.final) try { report.final = await state(true); } catch (error) { report.errors.push(String(error)); }
  }
  report.elapsedMs = now();
  if (report.errors.length) report.passed = false;
  await invoke('native_e2e_finish', { report });
}
