import net from 'node:net';

export function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function probeTcpPort({ host, port, timeoutMs = 500 }) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host, port });
    let settled = false;

    const finish = (ok) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(ok);
    };

    socket.setTimeout(timeoutMs);
    socket.once('connect', () => finish(true));
    socket.once('timeout', () => finish(false));
    socket.once('error', () => finish(false));
  });
}

export async function waitForTcpPort({
  host = '127.0.0.1',
  port,
  timeoutMs = 10_000,
  intervalMs = 100,
  probe = probeTcpPort,
  sleep = delay,
  now = () => Date.now(),
} = {}) {
  if (!Number.isInteger(port) || port <= 0) {
    throw new Error(`invalid TCP port: ${port}`);
  }

  const deadline = now() + timeoutMs;
  while (true) {
    if (await probe({ host, port })) {
      return;
    }
    const remainingMs = deadline - now();
    if (remainingMs <= 0) {
      break;
    }
    await sleep(Math.min(intervalMs, remainingMs));
  }

  throw new Error(`timeout waiting for TCP ${host}:${port} after ${timeoutMs}ms`);
}

export async function assertTcpPortFree({
  host = '127.0.0.1',
  port,
  probe = probeTcpPort,
} = {}) {
  if (!Number.isInteger(port) || port <= 0) {
    throw new Error(`invalid TCP port: ${port}`);
  }

  if (await probe({ host, port })) {
    throw new Error(`TCP ${host}:${port} is already in use`);
  }
}

export function createExpectedProcessExitTracker() {
  let expected = false;

  return {
    markStarted() {
      expected = false;
    },
    markStopping() {
      expected = true;
    },
    isExpected() {
      return expected;
    },
  };
}
