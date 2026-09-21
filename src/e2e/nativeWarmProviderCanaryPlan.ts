export type WarmProviderStopPhase = 'before-ready' | 'after-first-pcm' | 'during-partial' | 'after-final';
export type WarmProviderCyclePlan = { index: number; jitterMs: number;
  stopPhase: WarmProviderStopPhase; episode: string };
export type WarmProviderCanaryTrial = { id: string; kind: 'warm-provider-canary';
  cycles: WarmProviderCyclePlan[]; episodes: string[]; readyGateFromIndex?: number;
  finalEpisodeIndex: number };

export function validateWarmProviderCanaryPlan(trial: WarmProviderCanaryTrial) {
  const phases: WarmProviderStopPhase[] = ['before-ready', 'after-first-pcm', 'during-partial', 'after-final'];
  const jitters = [0, 25, 100, 250, 500];
  const phrases = ['episode-a.pcm', 'episode-b.pcm'];
  if (trial.id !== 'warm-provider-churn-20' || trial.kind !== 'warm-provider-canary') {
    throw new Error('Wrong warm canary identity');
  }
  if (trial.cycles.length !== 20 || trial.episodes.length !== 21 ||
      trial.readyGateFromIndex !== 5 || trial.finalEpisodeIndex !== 20) {
    throw new Error('Warm canary must contain 20 churn cycles and one final proof');
  }
  for (const [index, cycle] of trial.cycles.entries()) {
    if (cycle.index !== index || cycle.stopPhase !== phases[Math.floor(index / jitters.length)] ||
        cycle.jitterMs !== jitters[index % jitters.length] || cycle.episode !== trial.episodes[index] ||
        cycle.episode !== phrases[index % phrases.length]) {
      throw new Error(`Warm canary cycle ${index} differs from the fixed paid plan`);
    }
  }
  if (new Set(trial.cycles.map(cycle => cycle.episode)).size < 2) {
    throw new Error('Warm canary needs multiple distinct spoken fixtures');
  }
  if (trial.episodes[trial.finalEpisodeIndex] !== 'long-auto-commit.pcm' ||
      trial.cycles.some(cycle => cycle.episode === trial.episodes[trial.finalEpisodeIndex])) {
    throw new Error('Warm canary final proof source must be exclusive to its generation');
  }
}
