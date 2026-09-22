import type { Pinia } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { boundedSyntheticText, syntheticPhraseOccurrences } from './nativeContinuationMetrics';
import { nativeLivePreflight } from './nativeContinuationLive';
import { expectedWarmProviderCallbackGenerations, expectedWarmProviderLogicalRuns,
  validateWarmProviderCanaryPlan,
  type WarmProviderCanaryTrial as Trial } from './nativeWarmProviderCanaryPlan';

type SourceEpisode = { name: string; bytes: number; captureGeneration: number; sourceFrames: number;
  emittedFrames: number; nativeSourceStartMs: number | null; nativeSourceEndMs: number | null;
  nativeLastSourceFrameMs?: number | null; lastSourceFrameElapsedMs?: number | null;
  pacingIntervalsChecked?: number; pacingViolations?: number;
  sourceDurationMs: number; cadenceMs: number;
  sourceGateRequired: boolean; sourceGateReady: { serverReady: boolean; emittedFrames: number } | null };
type NativeReadback = { armed: boolean; valid: boolean; error: string | null;
  records: Array<{ text: string }> };
type NativeState = { status: string; logicalProviderRunId: number; preparedCaptureTokenCount: number;
  nativeReadback: NativeReadback;
  captureEpisode: { runId: number; generation: number } | null;
  pausedContinuation: { logicalRunId: number; pauseEpoch: number; connectionGeneration: number;
    providerSessionId: string | null } | null;
  qualificationTrial: Trial; qualificationEndpoint: string;
  providerTransport: { serverReady: boolean; connectionRetained: boolean } | null;
  fixture: { captureStarts: number; captureStops: number; activeCaptures: number; maxActiveCaptures: number;
    observationOverflow: boolean; markerViolations: string[];
    sourceEpisodes: SourceEpisode[];
    captureRunAssociations: Array<{ captureRunId: number; captureFenceGeneration: number; captureGeneration: number }>;
    capturePcmLedgers: Array<{ captureGeneration: number; chunks: number; samples: number; hash: string }>;
    providerPcmLedgers: Array<{ captureGeneration: number; chunks: number; samples: number; hash: string }>;
    providerCallbackGenerations: number[] } };
type ProviderEvent = { event: string; atMs: number; cycleIndex: number | null; sessionId: number;
  deliverySeq: number | null; text: string | null; markerIds: number[]; timingKnown: boolean;
  sourceStartSeconds: number; sourceDurationSeconds: number };
type ProviderTerminal = { sessionId: number; cycleIndex: number | null; complete: boolean };
export type WarmCanaryCaptureFence = { sessionId: number; cycleIndex: number;
  providerStartSamples: number; providerSamples: number; deliverySeqFloor: number };

export type WarmCanaryStopBoundary = {
  statusBeforeStop: string;
  providerTransportBeforeStop: { serverReady: boolean; connectionRetained: boolean } | null;
  nativeBoundaryMs: number;
};

export function warmCanaryBeforeReadyStopEvidence(
  value: WarmCanaryStopBoundary,
) {
  return {
    readyBeforeStop: value.providerTransportBeforeStop?.serverReady === true,
    providerTransportBeforeStop: value.providerTransportBeforeStop,
    statusBeforeStop: value.statusBeforeStop,
    nativeBoundaryMs: value.nativeBoundaryMs,
  };
}

const providerTimingSampleRange = (event: ProviderEvent) => {
  const start = Math.round(event.sourceStartSeconds * 16_000);
  const duration = Math.round(event.sourceDurationSeconds * 16_000);
  return { start, duration, end: start + duration };
};

const state = (stopReadback = false) => invoke<NativeState>('native_e2e_state', { stopReadback });
const wait = (durationMs: number) => invoke('native_e2e_delay', { durationMs });
function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
export function requireWarmProviderCanaryReader(value: Pick<NativeState, 'nativeReadback'>) {
  const reader = value.nativeReadback;
  check(reader?.armed, `paid canary: owned OS reader not armed: ${reader?.error ?? 'no native error'}`);
  check(reader.valid, `paid canary: owned OS reader invalid: ${reader.error ?? 'no native error'}`);
  check(reader.records[0]?.text === '', 'paid canary: owned OS reader initial text not empty');
}
async function toggle() {
  await invoke('native_e2e_hotkey', { action: 'press' });
  await invoke('native_e2e_hotkey', { action: 'release' });
}
const stopWithTransportBoundary = () =>
  invoke<WarmCanaryStopBoundary>('native_e2e_stop_with_transport_boundary');

export async function releaseWarmCanarySourceBeforeAck(
  releaseSource: () => Promise<unknown>,
  waitForAck: (() => Promise<unknown>) | null,
) {
  await releaseSource();
  if (waitForAck) await waitForAck();
}

export function warmCanaryEventMatchesEpisode(
  event: Pick<ProviderEvent, 'event' | 'text' | 'markerIds'>,
  episode: string,
  expectedEvent: 'transcription:partial' | 'transcription:final',
) {
  const markerId = episode === 'episode-a.pcm' ? 0 : episode === 'episode-b.pcm' ? 1 : -1;
  const prefix = markerId === 0 ? 'на столе' : markerId === 1 ? 'за окном' : '';
  const oppositePrefix = markerId === 0 ? 'за окном' : markerId === 1 ? 'на столе' : '';
  const normalized = typeof event.text === 'string'
    ? event.text.toLocaleLowerCase('ru').replace(/ё/g, 'е').replace(/[.,!?]/g, '').replace(/\s+/g, ' ')
    : '';
  const expectedCount = prefix === '' ? 0 : normalized.split(prefix).length - 1;
  const oppositeCount = oppositePrefix === '' ? 0 : normalized.split(oppositePrefix).length - 1;
  return event.event === expectedEvent && expectedCount === 1 && oppositeCount === 0 &&
    (expectedEvent !== 'transcription:final' || event.markerIds.includes(markerId));
}

export function warmCanaryEventMatchesCapture(
  event: ProviderEvent,
  episode: string,
  expectedEvent: 'transcription:partial' | 'transcription:final',
  fence: WarmCanaryCaptureFence,
) {
  if (!warmCanaryEventMatchesEpisode(event, episode, expectedEvent) ||
      event.sessionId !== fence.sessionId || event.cycleIndex !== fence.cycleIndex ||
      !Number.isSafeInteger(fence.deliverySeqFloor) || fence.deliverySeqFloor < 0 ||
      !Number.isSafeInteger(fence.providerStartSamples) ||
      !Number.isSafeInteger(fence.providerSamples) || fence.providerStartSamples < 0 ||
      fence.providerSamples <= 0) return false;
  // Production Backend Stable deliveries intentionally carry no source timing.
  // Their causal proof is a fresh monotonic delivery identity after this
  // generation's callback/PCM fence plus the exact episode phrase. Partials
  // and timed finals retain the stricter sample-overlap proof below.
  if (expectedEvent === 'transcription:final' &&
      (!Number.isSafeInteger(event.deliverySeq) || Number(event.deliverySeq) <= fence.deliverySeqFloor)) return false;
  if (event.timingKnown !== true) return expectedEvent === 'transcription:final';
  if (!Number.isFinite(event.sourceStartSeconds) ||
      !Number.isFinite(event.sourceDurationSeconds) || event.sourceStartSeconds < 0 ||
      event.sourceDurationSeconds <= 0) return false;
  const timing = providerTimingSampleRange(event);
  const fenceEndSamples = fence.providerStartSamples + fence.providerSamples;
  // A delivery from the preceding capture can arrive with a fresh delivery
  // sequence after Continue. Its provider timing still ends at or before this
  // generation's first sample, so require causal overlap with current PCM.
  return Number.isSafeInteger(timing.start) && Number.isSafeInteger(timing.duration) &&
    timing.duration > 0 && timing.start < fenceEndSamples &&
    timing.end > fence.providerStartSamples && timing.end <= fenceEndSamples;
}

export async function runNativeWarmProviderCanary(pinia: Pinia) {
  const started = performance.now();
  const now = () => performance.now() - started;
  let lastSettledAtMs: number | null = null;
  const sessionCycles = new Map<number, number>();
  const terminalSessions = new Set<number>();
  const subscriptions: Array<() => void> = [];
  const report = { mode: 'warm-provider-canary', passed: false, trialId: '', targetDocument: '',
    expectedInsertion: '', finalTextBeforeProof: '', actualPasteVerified: false,
    finalCallbackFence: null as { captureGeneration: number; eventStart: number } | null,
    finalTranscriptFence: null as { eventStart: number; providerSamples: number;
      providerStartSamples: number; deliverySeqFloor: number } | null,
    finalStartedAtMs: null as number | null,
    cycles: [] as Array<Record<string, unknown>>,
    finalOwnership: null as { logicalRunId: number; captureRunId: number; captureFenceGeneration: number } | null,
    events: [] as ProviderEvent[], terminals: [] as ProviderTerminal[], duplicateDeliveries: [] as string[],
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
    const callbackFenceGenerations = new Set(expectedWarmProviderCallbackGenerations(trial));
    report.trialId = trial.id;
    const seenDeliveries = new Set<string>();
    const providerStartSamples = (snapshot: NativeState, logicalRunId: number) => {
      const generations = new Set(report.cycles.filter(row =>
        Number(row.logicalRunId) === logicalRunId).map(row => Number(row.captureGeneration)));
      return snapshot.fixture.providerPcmLedgers.reduce((sum, row) =>
        sum + (generations.has(row.captureGeneration) ? row.samples : 0), 0);
    };
    const store = await nativeLivePreflight(pinia, initial.qualificationEndpoint, subscriptions,
      (name, payload) => {
        if (report.events.length >= 2048) { report.errors.push('Provider event evidence overflow'); return; }
        const sessionId = Number(payload.session_id);
        if (terminalSessions.has(sessionId)) report.errors.push(`Provider event after terminal: ${sessionId}:${name}`);
        if (name === 'transcription:terminal') terminalSessions.add(sessionId);
        const eventCycle = sessionCycles.get(sessionId);
        if (!Number.isSafeInteger(eventCycle)) report.errors.push(`Provider event has unknown run ownership: ${sessionId}:${name}`);
        const deliverySeq = typeof payload.delivery_seq === 'number' ? payload.delivery_seq : null;
        if (name === 'transcription:final' &&
            (!Number.isSafeInteger(deliverySeq) || Number(deliverySeq) <= 0)) {
          report.errors.push('Provider final is missing a valid delivery identity');
        }
        const bounded = boundedSyntheticText(payload.text);
        if (!bounded.syntheticTextValid) report.errors.push('Provider text evidence overflow');
        const deliveryKey = deliverySeq === null ? null : `${sessionId}:${deliverySeq}:${name}`;
        if (deliveryKey && seenDeliveries.has(deliveryKey)) report.duplicateDeliveries.push(deliveryKey);
        if (deliveryKey) seenDeliveries.add(deliveryKey);
        const markerIds = [...new Set(syntheticPhraseOccurrences(bounded.rawSyntheticText ?? '')
          .map(marker => marker.markerId))];
        report.events.push({ event: name, atMs: now(), cycleIndex: eventCycle ?? null, sessionId,
          deliverySeq, text: bounded.rawSyntheticText, markerIds,
          timingKnown: payload.timing_known === true, sourceStartSeconds: Number(payload.start),
          sourceDurationSeconds: Number(payload.duration) });
        if (name === 'transcription:terminal') {
          const complete = payload.delivery_complete === true && !payload.error;
          if (!complete) report.errors.push('Incomplete provider terminal');
          report.terminals.push({ sessionId, cycleIndex: eventCycle ?? null, complete });
        }
        if (name === 'transcription:error') report.errors.push(JSON.stringify(payload));
      }, { keepAlive: true, autoPasteText: false, autoCopyToClipboard: true });
    report.targetDocument = await invoke<string>('native_e2e_prepare_live_target');
    await wait(500);
    requireWarmProviderCanaryReader(await state());

    for (const cycle of trial.cycles) {
      const eventStart = report.events.length;
      const before = await state();
      if (before.pausedContinuation?.logicalRunId) {
        sessionCycles.set(before.pausedContinuation.logicalRunId, cycle.index);
      }
      const startedAtMs = now();
      const previousSettleToStartMs = lastSettledAtMs === null ? null : startedAtMs - lastSettledAtMs;
      let triggerEventStart = report.events.length;
      let callbackFenceGeneration: number | null = null;
      let triggerProviderSamples: number | null = null;
      let triggerProviderStartSamples: number | null = null;
      let triggerDeliverySeqFloor: number | null = null;
      let triggerLogicalRunId: number | null = null;
      await toggle();
      const active = await poll(value => value.fixture.captureStarts === before.fixture.captureStarts + 1 &&
        value.fixture.activeCaptures === 1 && value.fixture.sourceEpisodes.length === cycle.index + 1,
      `cycle ${cycle.index} capture start`, 15_000);
      const generation = cycle.index + 1;
      if (active.logicalProviderRunId > 0) sessionCycles.set(active.logicalProviderRunId, cycle.index);
      const source = () => state().then(value => value.fixture.sourceEpisodes[cycle.index]);
      let trigger: Record<string, unknown> = {};
      if (cycle.stopPhase === 'before-ready') {
        check(active.status !== 'Recording' || active.providerTransport?.serverReady !== true,
          `cycle ${cycle.index} reached provider Ready before early stop`);
      } else {
        const ready = await poll(value =>
          value.providerTransport?.serverReady === true && value.providerTransport.connectionRetained === true &&
          value.logicalProviderRunId > 0 && value.captureEpisode !== null &&
          value.fixture.sourceEpisodes[cycle.index]?.emittedFrames === 0,
        `cycle ${cycle.index} actual provider Ready`, 30_000);
        check(ready.fixture.sourceEpisodes[cycle.index].sourceGateRequired === true,
          `cycle ${cycle.index} source was not held behind provider Ready`);
        triggerLogicalRunId = ready.logicalProviderRunId;
        if (cycle.stopPhase === 'during-partial' || cycle.stopPhase === 'after-final') {
          // Snapshot the delivery boundary before releasing the first PCM. A
          // fresh provider may emit its only Stable while the retained-session
          // callback ACK or the PCM ledger is still being observed.
          triggerEventStart = report.events.length;
          triggerDeliverySeqFloor = report.events.slice(0, triggerEventStart)
            .filter(event => event.sessionId === triggerLogicalRunId && Number.isSafeInteger(event.deliverySeq))
            .reduce((maximum, event) => Math.max(maximum, Number(event.deliverySeq)), 0);
        }
        const requiresCallbackFence = callbackFenceGenerations.has(generation);
        await releaseWarmCanarySourceBeforeAck(
          () => invoke('native_e2e_configure', { config: { sourceGateReady: true } }),
          requiresCallbackFence ? () => poll(value =>
            value.fixture.providerCallbackGenerations[value.fixture.providerCallbackGenerations.length - 1] === generation,
          `cycle ${cycle.index} provider callback ACK fence`, 30_000) : null,
        );
        if (requiresCallbackFence) {
          callbackFenceGeneration = generation;
        }
      }
      if (cycle.stopPhase === 'after-first-pcm') {
        const observed = await poll(value =>
          (value.fixture.providerPcmLedgers.find(row => row.captureGeneration === generation)?.samples ?? 0) > 0,
        `cycle ${cycle.index} first provider PCM`, 30_000);
        triggerProviderSamples = observed.fixture.providerPcmLedgers.find(row =>
          row.captureGeneration === generation)?.samples ?? null;
        triggerProviderStartSamples = providerStartSamples(observed, observed.logicalProviderRunId);
        trigger = { emittedFrames: observed.fixture.sourceEpisodes[cycle.index].emittedFrames,
          providerSamples: triggerProviderSamples };
      } else if (cycle.stopPhase !== 'before-ready') {
        const wanted = cycle.stopPhase === 'during-partial' ? 'transcription:partial' : 'transcription:final';
        const observed = await poll(value =>
          (value.fixture.providerPcmLedgers.find(row => row.captureGeneration === generation)?.samples ?? 0) > 0,
        `cycle ${cycle.index} current-generation provider PCM fence`, 30_000);
        triggerProviderSamples = observed.fixture.providerPcmLedgers.find(row =>
          row.captureGeneration === generation)?.samples ?? null;
        triggerLogicalRunId = observed.logicalProviderRunId;
        triggerProviderStartSamples = providerStartSamples(observed, observed.logicalProviderRunId);
        check(triggerDeliverySeqFloor !== null,
          `cycle ${cycle.index} transcript boundary was not captured before PCM release`);
        const deadline = performance.now() + 70_000;
        let triggerEvent: ProviderEvent | undefined;
        while (performance.now() < deadline && !triggerEvent) {
          const current = await state();
          triggerProviderSamples = current.fixture.providerPcmLedgers.find(row =>
            row.captureGeneration === generation)?.samples ?? 0;
          triggerProviderStartSamples = providerStartSamples(current, triggerLogicalRunId);
          const fence = { sessionId: triggerLogicalRunId, cycleIndex: cycle.index,
            providerStartSamples: triggerProviderStartSamples,
            providerSamples: triggerProviderSamples, deliverySeqFloor: triggerDeliverySeqFloor };
          triggerEvent = report.events.slice(triggerEventStart).find(event =>
            warmCanaryEventMatchesCapture(event, cycle.episode, wanted, fence));
          if (!triggerEvent) await wait(20);
        }
        check(triggerEvent,
          `cycle ${cycle.index} never observed ${wanted}`);
        trigger = { event: wanted, episode: cycle.episode, deliverySeq: triggerEvent.deliverySeq };
      }
      const beforeStop = await state();
      if (beforeStop.logicalProviderRunId > 0) sessionCycles.set(beforeStop.logicalProviderRunId, cycle.index);
      const association = beforeStop.fixture.captureRunAssociations.find(row => row.captureGeneration === generation) ?? null;
      if (cycle.stopPhase !== 'before-ready') {
        check(beforeStop.captureEpisode !== null &&
          association?.captureRunId === beforeStop.captureEpisode.runId &&
          association.captureFenceGeneration === beforeStop.captureEpisode.generation,
          `cycle ${cycle.index} lost capture generation to logical run ownership`);
      }
      const triggerAtMs = now();
      if (cycle.stopPhase === 'before-ready') {
        trigger = warmCanaryBeforeReadyStopEvidence(await stopWithTransportBoundary());
        check(trigger.readyBeforeStop === false,
          `cycle ${cycle.index} reached provider Ready at the native stop boundary`);
      } else {
        await toggle();
      }
      // Classify the phase only after native gesture acceptance. A final that
      // lands while Stop is being dispatched belongs before the boundary and
      // must invalidate a during-partial/first-PCM claim.
      const stopEventIndex = report.events.length;
      const eventsBeforeStop = report.events.slice(eventStart, stopEventIndex)
        .filter(event => event.sessionId === beforeStop.logicalProviderRunId && event.cycleIndex === cycle.index);
      if (cycle.stopPhase === 'after-first-pcm') {
        check(!eventsBeforeStop.some(event =>
          event.event === 'transcription:partial' || event.event === 'transcription:final'),
        `cycle ${cycle.index} reached transcript evidence before the first-PCM stop`);
      } else if (cycle.stopPhase === 'during-partial') {
        check(!eventsBeforeStop.some(event => event.event === 'transcription:final'),
          `cycle ${cycle.index} reached final before the partial stop`);
      }
      const stopped = await poll(value => value.fixture.activeCaptures === 0 &&
        value.fixture.captureStops === before.fixture.captureStops + 1, `cycle ${cycle.index} capture stop`, 5_000);
      const captureStoppedAtMs = now();
      await poll(value => cycle.stopPhase === 'before-ready'
        ? value.status === 'Idle'
        : value.pausedContinuation?.logicalRunId === beforeStop.logicalProviderRunId &&
          value.providerTransport?.connectionRetained === true,
      `cycle ${cycle.index} stop settlement`, 45_000);
      const settledAtMs = now();
      const sourceAtStop = await source();
      check(sourceAtStop?.name === cycle.episode && sourceAtStop.captureGeneration === generation,
        `cycle ${cycle.index} source identity mismatch`);
      const cycleReport: Record<string, unknown> = { ...cycle, startedAtMs, triggerAtMs,
        previousSettleToStartMs,
        captureStoppedAtMs, settledAtMs, captureGeneration: generation,
        captureRunId: beforeStop.captureEpisode?.runId ?? null,
        captureFenceGeneration: beforeStop.captureEpisode?.generation ?? null,
        logicalRunId: beforeStop.logicalProviderRunId, association, trigger, source: sourceAtStop,
        eventStart, triggerEventStart, stopEventIndex, callbackFenceGeneration, triggerProviderSamples,
        providerStartSamples: triggerProviderStartSamples, triggerDeliverySeqFloor,
        eventEnd: report.events.length, activeCapturesAfterStop: stopped.fixture.activeCaptures };
      const previousCycle = report.cycles[report.cycles.length - 1];
      if (previousCycle) {
        const mustReuse = cycle.resetProviderBefore !== true &&
          trial.cycles[cycle.index - 1]?.stopPhase !== 'before-ready';
        check(mustReuse
          ? Number(previousCycle.logicalRunId) === beforeStop.logicalProviderRunId
          : Number(previousCycle.logicalRunId) !== beforeStop.logicalProviderRunId,
        `cycle ${cycle.index} provider ownership transition contradicted the fixed plan`);
      }
      report.cycles.push(cycleReport);
      const nextCycle = trial.cycles[cycle.index + 1];
      if (nextCycle?.resetProviderBefore === true) {
        // Repeated spoken fixtures are safe only across distinct provider
        // sessions. Retire the paused session before every after-final cycle;
        // the next cycle cannot consume a delayed Stable from its predecessor.
        await invoke('update_stt_config', { provider: 'backend', language: 'en',
          backendStreamingProvider: 'elevenlabs' });
        await poll(value => value.status === 'Idle' &&
          value.providerTransport?.connectionRetained === false,
        `cycle ${cycle.index} provider reset`, 15_000);
        const terminalDeadline = performance.now() + 5_000;
        while (performance.now() < terminalDeadline && !report.terminals.some(terminal =>
          terminal.sessionId === beforeStop.logicalProviderRunId)) await wait(20);
        check(report.terminals.some(terminal => terminal.sessionId === beforeStop.logicalProviderRunId),
          `cycle ${cycle.index} provider reset lacked terminal ownership`);
        cycleReport.providerResetSettledAtMs = now();
        cycleReport.eventEnd = report.events.length;
      }
      lastSettledAtMs = Number(cycleReport.providerResetSettledAtMs ?? settledAtMs);
      await invoke('native_e2e_progress', { report: { scenario: 'warm-provider-churn',
        completedCycles: cycle.index + 1, stopPhase: cycle.stopPhase } });
      await wait(cycle.jitterMs);
    }

    const beforeFinal = await state();
    if (beforeFinal.pausedContinuation?.logicalRunId) {
      sessionCycles.set(beforeFinal.pausedContinuation.logicalRunId, trial.finalEpisodeIndex);
    }
    report.finalStartedAtMs = now();
    await toggle();
    const ready = await poll(value =>
      value.providerTransport?.serverReady === true && value.providerTransport.connectionRetained === true &&
      value.logicalProviderRunId > 0 && value.captureEpisode !== null &&
      value.fixture.sourceEpisodes.length === trial.finalEpisodeIndex + 1,
    'final full proof provider Ready', 30_000);
    check(ready.logicalProviderRunId === Number(report.cycles[report.cycles.length - 1]?.logicalRunId),
      'Final proof opened a new provider instead of continuing the retained session');
    sessionCycles.set(ready.logicalProviderRunId, trial.finalEpisodeIndex);
    check(ready.fixture.sourceEpisodes[trial.finalEpisodeIndex].emittedFrames === 0,
      'Final source escaped the real provider Ready gate');
    // Establish the transcript boundary before releasing this generation's
    // source. A healthy Stable final can arrive while the later PCM readbacks
    // are still being polled, so those polls must not advance the floor past it.
    const finalTranscriptEventStart = report.events.length;
    const finalDeliverySeqFloor = report.events.slice(0, finalTranscriptEventStart)
      .filter(event => event.sessionId === ready.logicalProviderRunId &&
        Number.isSafeInteger(event.deliverySeq))
      .reduce((maximum, event) => Math.max(maximum, Number(event.deliverySeq)), 0);
    report.finalTextBeforeProof = store.finalText;
    await invoke('native_e2e_configure', { config: { sourceGateReady: true } });
    const finalGeneration = trial.finalEpisodeIndex + 1;
    await poll(value => (value.fixture.sourceEpisodes[trial.finalEpisodeIndex]?.emittedFrames ?? 0) > 0 &&
      value.fixture.providerCallbackGenerations[value.fixture.providerCallbackGenerations.length - 1] === finalGeneration,
    'final provider callback ACK fence', 30_000);
    const finalEventStart = report.events.length;
    report.finalCallbackFence = { captureGeneration: finalGeneration, eventStart: finalEventStart };
    const complete = await poll(value => {
      const source = value.fixture.sourceEpisodes[trial.finalEpisodeIndex];
      return source?.emittedFrames === source?.sourceFrames && typeof source.nativeSourceEndMs === 'number';
    }, 'final full PCM proof', 90_000);
    const finalAssociation = complete.fixture.captureRunAssociations.find(row =>
      row.captureGeneration === trial.finalEpisodeIndex + 1);
    check(complete.captureEpisode !== null &&
      finalAssociation?.captureRunId === complete.captureEpisode.runId &&
      finalAssociation.captureFenceGeneration === complete.captureEpisode.generation,
      'Final proof generation does not belong to the live provider run');
    report.finalOwnership = { logicalRunId: complete.logicalProviderRunId,
      captureRunId: complete.captureEpisode.runId,
      captureFenceGeneration: complete.captureEpisode.generation };
    const delivered = await poll(value => value.fixture.providerPcmLedgers.some(row =>
      row.captureGeneration === finalGeneration &&
      row.samples === value.fixture.sourceEpisodes[trial.finalEpisodeIndex]?.sourceFrames),
    'final provider full PCM drain', 30_000);
    const finalProviderLedger = delivered.fixture.providerPcmLedgers.find(row =>
      row.captureGeneration === finalGeneration)!;
    const finalProviderStartSamples = providerStartSamples(delivered, complete.logicalProviderRunId);
    report.finalTranscriptFence = { eventStart: finalTranscriptEventStart,
      providerSamples: finalProviderLedger.samples, providerStartSamples: finalProviderStartSamples,
      deliverySeqFloor: finalDeliverySeqFloor };
    const belongsToFinalCallbackGeneration = (event: ProviderEvent) => {
      const timing = providerTimingSampleRange(event);
      return event.event === 'transcription:final' && event.sessionId === complete.logicalProviderRunId &&
        event.cycleIndex === trial.finalEpisodeIndex && Number.isSafeInteger(event.deliverySeq) &&
        Number(event.deliverySeq) > finalDeliverySeqFloor &&
        (event.timingKnown !== true ||
          (Number.isSafeInteger(timing.start) && Number.isSafeInteger(timing.duration) && timing.duration > 0 &&
            timing.start < finalProviderStartSamples + finalProviderLedger.samples &&
            timing.end > finalProviderStartSamples &&
            timing.end <= finalProviderStartSamples + finalProviderLedger.samples)) &&
        syntheticPhraseOccurrences(event.text ?? '').length === 2 &&
        event.markerIds.length === 2 && event.markerIds.includes(0) && event.markerIds.includes(1);
    };
    await toggle();
    await poll(value => value.fixture.captureStops === beforeFinal.fixture.captureStops + 1 &&
      value.fixture.activeCaptures === 0 &&
      value.pausedContinuation?.logicalRunId === complete.logicalProviderRunId &&
      value.providerTransport?.connectionRetained === true, 'final full proof pause', 45_000);
    const normalizeTranscript = (text: string | null) => (text ?? '')
      .toLocaleLowerCase('ru').replace(/ё/g, 'е').replace(/[.,!?]/g, '').replace(/\s+/g, ' ').trim();
    await poll(() => {
      const delivery = report.events.slice(finalTranscriptEventStart)
        .find(belongsToFinalCallbackGeneration);
      return store.finalText !== report.finalTextBeforeProof && delivery != null &&
        normalizeTranscript(store.finalText) === normalizeTranscript(delivery.text);
    },
    'final stable transcript proof', 30_000);
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
    await wait(250);
    report.final = await state(true);
    check(report.final.fixture.captureStarts === 21 && report.final.fixture.captureStops === 21 &&
      report.final.fixture.activeCaptures === 0 && report.final.fixture.maxActiveCaptures === 1 &&
      report.final.fixture.sourceEpisodes.length === 21 && report.final.preparedCaptureTokenCount === 0,
    'Warm provider canary capture lifecycle is unbalanced');
    check(report.final.fixture.observationOverflow === false && report.final.fixture.markerViolations.length === 0 &&
      report.duplicateDeliveries.length === 0 && report.errors.length === 0,
    'Warm provider canary retained invalid or duplicate evidence');
    const terminalEvents = report.events.filter(event => event.event === 'transcription:terminal');
    const finalTerminals = report.terminals.filter(terminal =>
      terminal.sessionId === report.finalOwnership?.logicalRunId);
    const finalTerminalEvents = terminalEvents.filter(event =>
      event.sessionId === report.finalOwnership?.logicalRunId);
    const expectedTerminalCycles = new Map(report.cycles.map(cycle =>
      [Number(cycle.logicalRunId), Number(cycle.index)]));
    if (report.finalOwnership) {
      expectedTerminalCycles.set(report.finalOwnership.logicalRunId, trial.finalEpisodeIndex);
    }
    check(expectedTerminalCycles.size === expectedWarmProviderLogicalRuns(trial) &&
      report.finalOwnership?.logicalRunId === Number(report.cycles[report.cycles.length - 1]?.logicalRunId) &&
      report.terminals.length === expectedTerminalCycles.size &&
      terminalEvents.length === expectedTerminalCycles.size &&
      report.terminals.every(terminal => terminal.complete &&
        expectedTerminalCycles.get(terminal.sessionId) === terminal.cycleIndex &&
        terminalEvents.filter(event => event.sessionId === terminal.sessionId &&
          event.cycleIndex === terminal.cycleIndex).length === 1) &&
      [...expectedTerminalCycles].every(([sessionId, cycleIndex]) =>
        report.terminals.filter(terminal => terminal.sessionId === sessionId &&
          terminal.cycleIndex === cycleIndex).length === 1) &&
      finalTerminals.length === 1 && finalTerminals[0].cycleIndex === trial.finalEpisodeIndex &&
      finalTerminalEvents.length === 1 && finalTerminalEvents[0].cycleIndex === trial.finalEpisodeIndex,
    'Warm provider canary terminal ownership is incomplete or duplicated');
    const finalEvents = report.events.filter(event => event.event === 'transcription:final');
    check(finalEvents.every(event => {
      const cycleIndex = event.cycleIndex;
      if (typeof cycleIndex !== 'number' || !Number.isSafeInteger(cycleIndex) ||
          cycleIndex < 0 || cycleIndex > trial.finalEpisodeIndex) return false;
      if (cycleIndex === trial.finalEpisodeIndex) {
        return event.sessionId === report.finalOwnership?.logicalRunId && event.markerIds.length >= 2 &&
          typeof event.text === 'string' && event.text.trim().length > 0;
      }
      const cycle = trial.cycles[cycleIndex];
      const row = report.cycles[cycleIndex];
      const provider = report.final?.fixture.providerPcmLedgers.find(ledger =>
        ledger.captureGeneration === Number(row?.captureGeneration));
      return cycle != null && provider != null &&
        warmCanaryEventMatchesCapture(event, cycle.episode, 'transcription:final', {
          sessionId: Number(row?.logicalRunId), cycleIndex,
          providerStartSamples: Number(row?.providerStartSamples), providerSamples: provider.samples,
          deliverySeqFloor: Number(row?.triggerDeliverySeqFloor),
        });
    }) && finalEvents.every(event => finalEvents.filter(candidate =>
      candidate.sessionId === event.sessionId && candidate.cycleIndex === event.cycleIndex).length === 1) &&
      finalEvents.filter(event => event.sessionId === report.finalOwnership?.logicalRunId &&
        event.cycleIndex === trial.finalEpisodeIndex).length === 1,
    'Warm provider canary retained a late, duplicate, stale, or misowned final');
    const generations = report.final.fixture.capturePcmLedgers.map(row => row.captureGeneration);
    check(generations.length === 21 && new Set(generations).size === 21 &&
      generations.every((generation, index) => generation === index + 1),
    'Warm provider canary PCM generations are incomplete');
    const providers = new Map(report.final.fixture.providerPcmLedgers.map(row =>
      [row.captureGeneration, row]));
    for (const [index, capture] of report.final.fixture.capturePcmLedgers.entries()) {
      const emittedFrames = report.final.fixture.sourceEpisodes[index]?.emittedFrames;
      const provider = providers.get(capture.captureGeneration);
      check(capture.samples === emittedFrames && capture.chunks === Math.ceil(emittedFrames / 320),
        `Capture PCM ledger ${capture.captureGeneration} disagrees with its source`);
      if (capture.samples === 0) {
        check(capture.hash === 'cbf29ce484222325' && provider == null,
          `Empty PCM generation ${capture.captureGeneration} reached the provider`);
      } else {
        check(provider && provider.chunks > 0 && provider.samples === capture.samples &&
          provider.hash === capture.hash,
        `Provider PCM ledger ${capture.captureGeneration} is incomplete`);
      }
    }
    check(providers.size === report.final.fixture.capturePcmLedgers.filter(row => row.samples > 0).length,
      'Warm provider canary retained an unexpected PCM generation');
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error));
  } finally {
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
