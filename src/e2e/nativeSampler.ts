/** E2E polling: stop must join native IPC, but need not wait for a throttled timer. */
export function startNativeSampler(sample: () => Promise<void>, intervalMs = 10) {
  let running = true;
  let cancelWait: (() => void) | undefined;
  const completed = (async () => {
    while (running) {
      await sample();
      if (!running) break;
      await new Promise<void>((resolve) => {
        const finish = () => {
          clearTimeout(timer);
          cancelWait = undefined;
          resolve();
        };
        const timer = setTimeout(finish, intervalMs);
        cancelWait = finish;
      });
    }
  })();
  // Keep an early IPC rejection handled until the owner joins the sampler.
  void completed.catch(() => {});
  return {
    async stop(): Promise<void> {
      running = false;
      cancelWait?.();
      await completed;
    },
  };
}
