import { describe, expect, it } from 'vitest';
import { expectedWarmProviderCallbackGenerations,
  validateWarmProviderCanaryPlan } from './nativeWarmProviderCanaryPlan';
import { releaseWarmCanarySourceBeforeAck, requireWarmProviderCanaryReader,
  warmCanaryBeforeReadyStopEvidence, warmCanaryEventMatchesCapture,
  warmCanaryAggregateMatchesEpisode, warmCanaryAttributedFinalEvidence,
  warmCanaryEventMatchesEpisode } from './nativeWarmProviderCanary';

const phases = ['before-ready', 'after-first-pcm', 'during-partial', 'after-final'] as const;
const jitters = [0, 25, 100, 250, 500] as const;

function plan() {
  const cycles = Array.from({ length: 20 }, (_, index) => {
    const stopPhase = phases[Math.floor(index / jitters.length)];
    return { index, stopPhase, jitterMs: jitters[index % jitters.length],
      episode: index % 2 === 0 ? 'episode-a.pcm' : 'episode-b.pcm',
      ...(stopPhase === 'after-final' ? { resetProviderBefore: true } : {}) };
  });
  return { id: 'warm-provider-churn-20', kind: 'warm-provider-canary' as const,
    cycles, episodes: [...cycles.map(cycle => cycle.episode), 'long-auto-commit.pcm'],
    readyGateFromIndex: 5, readyGateTimeoutMs: 1800, finalEpisodeIndex: 20,
    resetProviderBeforeFinal: true as const };
}

describe('warm provider paid canary plan', () => {
  it('accepts only the fixed 20-cycle phase, jitter and source matrix', () => {
    expect(() => validateWarmProviderCanaryPlan(plan())).not.toThrow();
    const mutations = [
      (value: ReturnType<typeof plan>) => value.cycles.pop(),
      (value: ReturnType<typeof plan>) => { value.cycles[4].jitterMs = 25; },
      (value: ReturnType<typeof plan>) => { value.cycles[7].stopPhase = 'before-ready'; },
      (value: ReturnType<typeof plan>) => { value.cycles[9].episode = 'episode-a.pcm'; },
      (value: ReturnType<typeof plan>) => { delete value.cycles[15].resetProviderBefore; },
      (value: ReturnType<typeof plan>) => { value.cycles[14].resetProviderBefore = true; },
      (value: ReturnType<typeof plan>) => { value.readyGateFromIndex = 4; },
      (value: ReturnType<typeof plan>) => { value.readyGateTimeoutMs = 2200; },
      (value: ReturnType<typeof plan>) => { value.finalEpisodeIndex = 19; },
      (value: ReturnType<typeof plan>) => { delete (value as Partial<ReturnType<typeof plan>>)
        .resetProviderBeforeFinal; },
      (value: ReturnType<typeof plan>) => { value.episodes[20] = 'episode-a.pcm'; },
    ];
    for (const mutate of mutations) {
      const invalid = structuredClone(plan());
      mutate(invalid);
      expect(() => validateWarmProviderCanaryPlan(invalid)).toThrow();
    }
  });

  it('requires callback fences only for retained provider generations', () => {
    expect(expectedWarmProviderCallbackGenerations(plan())).toEqual([
      7, 8, 9, 10, 11, 12, 13, 14, 15,
    ]);
  });

  it('refuses capture when the owned OS reader is not ready after preparation', () => {
    const ready = { nativeReadback: { armed: true, valid: true, error: null, records: [{ text: '' }] } };
    expect(() => requireWarmProviderCanaryReader(ready)).not.toThrow();
    expect(() => requireWarmProviderCanaryReader({ nativeReadback: { ...ready.nativeReadback,
      armed: false, error: 'reader did not arm' } })).toThrow(/not armed/);
    expect(() => requireWarmProviderCanaryReader({ nativeReadback: { ...ready.nativeReadback,
      valid: false, error: 'reader stopped' } })).toThrow(/reader stopped/);
    expect(() => requireWarmProviderCanaryReader({ nativeReadback: { ...ready.nativeReadback,
      records: [{ text: 'stale transcript' }] } })).toThrow(/initial text not empty/);
  });

  it('releases gated PCM before waiting for its provider ACK', async () => {
    const order: string[] = [];
    await releaseWarmCanarySourceBeforeAck(
      async () => { order.push('release'); },
      async () => { order.push('ack'); },
    );
    expect(order).toEqual(['release', 'ack']);
  });

  it('uses authoritative provider readiness at the before-ready stop boundary', () => {
    expect(warmCanaryBeforeReadyStopEvidence({
      statusBeforeStop: 'Starting', nativeBoundaryMs: 42,
      providerTransportBeforeStop: { serverReady: true, connectionRetained: true },
    })).toEqual({
      readyBeforeStop: true, statusBeforeStop: 'Starting',
      providerTransportBeforeStop: { serverReady: true, connectionRetained: true },
      nativeBoundaryMs: 42,
    });
    expect(warmCanaryBeforeReadyStopEvidence({
      statusBeforeStop: 'Starting', providerTransportBeforeStop: null, nativeBoundaryMs: 43,
    }).readyBeforeStop).toBe(false);
  });

  it('accepts only the current episode phrase as partial/final trigger evidence', () => {
    const partial = { event: 'transcription:partial', text: 'На столе уже', markerIds: [] };
    const final = { event: 'transcription:final', text: 'За окном растет береза', markerIds: [1] };
    expect(warmCanaryEventMatchesEpisode(partial, 'episode-a.pcm', 'transcription:partial')).toBe(true);
    expect(warmCanaryEventMatchesEpisode(partial, 'episode-b.pcm', 'transcription:partial')).toBe(false);
    expect(warmCanaryEventMatchesEpisode(final, 'episode-b.pcm', 'transcription:final')).toBe(true);
    expect(warmCanaryEventMatchesEpisode({ ...final, markerIds: [] },
      'episode-b.pcm', 'transcription:final')).toBe(false);
    expect(warmCanaryEventMatchesEpisode({ ...final,
      text: 'За окном растет береза. На столе лежит книга', markerIds: [0, 1] },
    'episode-b.pcm', 'transcription:final')).toBe(false);
    expect(warmCanaryEventMatchesEpisode({ ...final,
      text: 'За окном растет береза. За окном растет береза' },
    'episode-b.pcm', 'transcription:final')).toBe(false);
  });

  it('rejects a late same-session final even with a fresh delivery sequence and repeated phrase', () => {
    const current = { event: 'transcription:final', text: 'За окном растет береза', markerIds: [1],
      sessionId: 9, cycleIndex: 16, deliverySeq: 88, atMs: 1000, timingKnown: true,
      sourceStartSeconds: 2, sourceDurationSeconds: 0.5 };
    const fence = { sessionId: 9, cycleIndex: 16, providerStartSamples: 32_000,
      providerSamples: 8_000, deliverySeqFloor: 87 };
    expect(warmCanaryEventMatchesCapture(current, 'episode-b.pcm',
      'transcription:final', fence)).toBe(true);
    expect(warmCanaryEventMatchesCapture({ ...current, sourceStartSeconds: 1.5 },
      'episode-b.pcm', 'transcription:final', fence)).toBe(false);
    expect(warmCanaryEventMatchesCapture({ ...current, timingKnown: false,
      sourceStartSeconds: 0, sourceDurationSeconds: 0 },
    'episode-b.pcm', 'transcription:final', fence)).toBe(true);
    expect(warmCanaryEventMatchesCapture({ ...current, timingKnown: false, deliverySeq: 87,
      sourceStartSeconds: 0, sourceDurationSeconds: 0 },
    'episode-b.pcm', 'transcription:final', fence)).toBe(false);
  });

  it('quantizes fractional provider timing before testing the capture boundary', () => {
    const stale = { event: 'transcription:final', text: 'За окном растет береза', markerIds: [1],
      sessionId: 9, cycleIndex: 16, deliverySeq: 89, atMs: 1001, timingKnown: true,
      sourceStartSeconds: 0.1, sourceDurationSeconds: 0.14 };
    const fence = { sessionId: 9, cycleIndex: 16, providerStartSamples: 3_840,
      providerSamples: 320, deliverySeqFloor: 88 };
    expect(warmCanaryEventMatchesCapture(stale, 'episode-b.pcm',
      'transcription:final', fence)).toBe(false);
  });

  it('accepts the production Stable shape only with unique-source attribution', () => {
    const fence = { sessionId: 9, cycleIndex: 16, providerStartSamples: 32_000,
      providerSamples: 8_000, deliverySeqFloor: 87 };
    const stable = { event: 'transcription:final', text: 'За окном растет береза', markerIds: [1],
      sessionId: 9, cycleIndex: 16, deliverySeq: 88, atMs: 1001, timingKnown: false,
      sourceStartSeconds: 0, sourceDurationSeconds: 0 };
    const timed = { ...stable, deliverySeq: null, atMs: 1000, timingKnown: true,
      sourceStartSeconds: 2, sourceDurationSeconds: 0.5 };
    expect(warmCanaryAttributedFinalEvidence([stable], fence, true).stableDeliveries)
      .toEqual([stable]);
    expect(warmCanaryAttributedFinalEvidence([stable], fence, false).stableDeliveries)
      .toEqual([]);
    expect(warmCanaryAttributedFinalEvidence([timed], fence, false).timedDeliveries)
      .toEqual([timed]);
    expect(warmCanaryAttributedFinalEvidence([timed], fence, true).stableDeliveries)
      .toEqual([]);
    expect(warmCanaryAttributedFinalEvidence([
      { ...timed, sourceStartSeconds: 1.9999375 },
    ], fence, false).timedDeliveries).toEqual([]);
    expect(warmCanaryAttributedFinalEvidence([
      { ...stable, deliverySeq: 89 }, { ...stable, deliverySeq: 88 },
    ], fence, true).stableDeliveries).toEqual([]);
  });

  it('accepts segmented episode text but rejects duplicate fragments', () => {
    const event = (text: string, deliverySeq: number) => ({ event: 'transcription:final', text,
      markerIds: [], sessionId: 9, cycleIndex: 16, deliverySeq, atMs: 1000 + deliverySeq,
      timingKnown: false, sourceStartSeconds: 0, sourceDurationSeconds: 0 });
    expect(warmCanaryAggregateMatchesEpisode([
      event('За окном', 1), event('растет береза', 2),
    ], 'episode-b.pcm')).toBe(true);
    expect(warmCanaryAggregateMatchesEpisode([
      event('За окном', 1), event('За окном', 2), event('растет береза', 3),
    ], 'episode-b.pcm')).toBe(false);
  });
});
