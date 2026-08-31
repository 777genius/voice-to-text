import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { startNativeSampler } from './nativeSampler';

// Drain the sampler, stop() and observer promise continuations without advancing
// any browser timer (including a poll timer that WebKit may indefinitely defer).
const flushMicrotasks = async () => {
  for (let turn = 0; turn < 5; turn += 1) await Promise.resolve();
};

describe('native E2E sampler teardown', () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] }));
  afterEach(() => vi.useRealTimers());

  it('joins when the hidden WebView poll timer never fires', async () => {
    const sample = vi.fn(async () => {});
    const sampler = startNativeSampler(sample);
    await flushMicrotasks();
    expect(vi.getTimerCount()).toBe(1);
    let joined = false;
    const stopped = sampler.stop().then(() => { joined = true; });
    try {
      await flushMicrotasks(); // Deliberately never advance the fake poll timer.
      expect(joined).toBe(true);
      expect(vi.getTimerCount()).toBe(0);
      expect(sample).toHaveBeenCalledTimes(1);
    } finally {
      await vi.runOnlyPendingTimersAsync();
      await stopped;
    }
  });

  it('joins an in-flight native sample and schedules no delay after stop', async () => {
    let finishSample!: () => void;
    const sample = vi.fn(() => new Promise<void>((resolve) => { finishSample = resolve; }));
    const sampler = startNativeSampler(sample);
    let joined = false;
    const stopped = sampler.stop().then(() => { joined = true; });
    await flushMicrotasks();
    expect(joined).toBe(false); // A real IPC stall must still block teardown.
    finishSample();
    try {
      await flushMicrotasks();
      expect(joined).toBe(true);
      expect(vi.getTimerCount()).toBe(0);
      expect(sample).toHaveBeenCalledTimes(1);
    } finally {
      await vi.runOnlyPendingTimersAsync();
      await stopped;
    }
  });

  it('retains a visibility violation from the last in-flight sample', async () => {
    let finishSample!: (visible: boolean) => void;
    let replacementHidden = false;
    let samples = 0;
    const sampler = startNativeSampler(async () => {
      const visible = ++samples === 1 ? true : await new Promise<boolean>((resolve) => { finishSample = resolve; });
      if (!visible) replacementHidden = true;
    });
    await flushMicrotasks();
    await vi.advanceTimersByTimeAsync(10);
    const stopped = sampler.stop();
    finishSample(false);
    await flushMicrotasks();
    await vi.runOnlyPendingTimersAsync();
    await stopped;
    expect(samples).toBe(2);
    expect(replacementHidden).toBe(true);
  });

  it('propagates a native IPC rejection to the owner joining the sampler', async () => {
    const failure = new Error('native state unavailable');
    const sampler = startNativeSampler(async () => { throw failure; });
    await flushMicrotasks();
    await expect(sampler.stop()).rejects.toBe(failure);
  });
});
