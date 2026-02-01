/**
 * Простой single-flight для refresh токенов.
 *
 * Идея: refresh token нельзя использовать параллельно. Если сразу несколько мест
 * захотели обновить сессию, второй и последующие просто ждут первый запрос.
 */
let inFlightRefresh: Promise<void> | null = null;

export async function runRefreshSingleFlight(task: () => Promise<void>): Promise<void> {
  if (inFlightRefresh) {
    return inFlightRefresh;
  }

  inFlightRefresh = (async () => {
    await task();
  })().finally(() => {
    inFlightRefresh = null;
  });

  return inFlightRefresh;
}

// Удобно для изоляции тестов (если понадобится)
export function __resetRefreshSingleFlightForTests(): void {
  inFlightRefresh = null;
}

