import { describe, expect, it } from 'vitest';
import { validateWarmProviderCanaryPlan } from './nativeWarmProviderCanaryPlan';

const phases = ['before-ready', 'after-first-pcm', 'during-partial', 'after-final'] as const;
const jitters = [0, 25, 100, 250, 500] as const;
const episodeByPhase = {
  'before-ready': 'episode-a.pcm',
  'after-first-pcm': 'episode-b.pcm',
  'during-partial': 'stop-inside-word.pcm',
  'after-final': 'episode-a.pcm',
} as const;

function plan() {
  const cycles = Array.from({ length: 20 }, (_, index) => {
    const stopPhase = phases[Math.floor(index / jitters.length)];
    return { index, stopPhase, jitterMs: jitters[index % jitters.length], episode: episodeByPhase[stopPhase] };
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
});
