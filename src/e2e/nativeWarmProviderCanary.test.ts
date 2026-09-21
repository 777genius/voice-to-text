import { describe, expect, it } from 'vitest';
import { validateWarmProviderCanaryPlan } from './nativeWarmProviderCanaryPlan';
import { releaseWarmCanarySourceBeforeAck, requireWarmProviderCanaryReader,
  warmCanaryEventMatchesCapture, warmCanaryEventMatchesEpisode } from './nativeWarmProviderCanary';

const phases = ['before-ready', 'after-first-pcm', 'during-partial', 'after-final'] as const;
const jitters = [0, 25, 100, 250, 500] as const;

function plan() {
  const cycles = Array.from({ length: 20 }, (_, index) => {
    const stopPhase = phases[Math.floor(index / jitters.length)];
    return { index, stopPhase, jitterMs: jitters[index % jitters.length],
      episode: index % 2 === 0 ? 'episode-a.pcm' : 'episode-b.pcm' };
  });
  return { id: 'warm-provider-churn-20', kind: 'warm-provider-canary' as const,
    cycles, episodes: [...cycles.map(cycle => cycle.episode), 'long-auto-commit.pcm'],
    readyGateFromIndex: 5, finalEpisodeIndex: 20 };
}

describe('warm provider paid canary plan', () => {
  it('accepts only the fixed 20-cycle phase, jitter and source matrix', () => {
    expect(() => validateWarmProviderCanaryPlan(plan())).not.toThrow();
    const mutations = [
      (value: ReturnType<typeof plan>) => value.cycles.pop(),
      (value: ReturnType<typeof plan>) => { value.cycles[4].jitterMs = 25; },
      (value: ReturnType<typeof plan>) => { value.cycles[7].stopPhase = 'before-ready'; },
      (value: ReturnType<typeof plan>) => { value.cycles[9].episode = 'episode-a.pcm'; },
      (value: ReturnType<typeof plan>) => { value.readyGateFromIndex = 4; },
      (value: ReturnType<typeof plan>) => { value.finalEpisodeIndex = 19; },
      (value: ReturnType<typeof plan>) => { value.episodes[20] = 'episode-a.pcm'; },
    ];
    for (const mutate of mutations) {
      const invalid = structuredClone(plan());
      mutate(invalid);
      expect(() => validateWarmProviderCanaryPlan(invalid)).toThrow();
    }
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
      providerSamples: 8_000 };
    expect(warmCanaryEventMatchesCapture(current, 'episode-b.pcm',
      'transcription:final', fence)).toBe(true);
    expect(warmCanaryEventMatchesCapture({ ...current, sourceStartSeconds: 1.5 },
      'episode-b.pcm', 'transcription:final', fence)).toBe(false);
    expect(warmCanaryEventMatchesCapture({ ...current, timingKnown: false },
      'episode-b.pcm', 'transcription:final', fence)).toBe(false);
  });

  it('quantizes fractional provider timing before testing the capture boundary', () => {
    const stale = { event: 'transcription:final', text: 'За окном растет береза', markerIds: [1],
      sessionId: 9, cycleIndex: 16, deliverySeq: 89, atMs: 1001, timingKnown: true,
      sourceStartSeconds: 0.1, sourceDurationSeconds: 0.14 };
    const fence = { sessionId: 9, cycleIndex: 16, providerStartSamples: 3_840,
      providerSamples: 320 };
    expect(warmCanaryEventMatchesCapture(stale, 'episode-b.pcm',
      'transcription:final', fence)).toBe(false);
  });
});
