export async function ensureE2E() {
  await browser.execute(() => {
    if (!window.__E2E__) throw new Error('__E2E__ is not installed');
  });
}

export async function invoke(command, args) {
  return await browser.executeAsync((cmd, a, done) => {
    window.__E2E__
      .invoke(cmd, a)
      .then((res) => done(res))
      .catch((err) => done({ __error: String(err) }));
  }, command, args);
}

export async function waitFor(fn, { timeoutMs = 15_000, intervalMs = 200 } = {}) {
  const start = Date.now();
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const ok = await fn();
    if (ok) return;
    if (Date.now() - start > timeoutMs) throw new Error('timeout');
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

export async function getWindowLabelSafe() {
  return await browser.execute(() => {
    if (!window.__E2E__) return null;
    return window.__E2E__.getWindowLabel();
  });
}

export async function findWindowHandleByLabel(label, { timeoutMs = 15_000 } = {}) {
  const start = Date.now();
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const handles = await browser.getWindowHandles();
    for (const h of handles) {
      try {
        await browser.switchToWindow(h);
        const current = await getWindowLabelSafe();
        if (current === label) return h;
      } catch {}
    }
    if (Date.now() - start > timeoutMs) {
      throw new Error(`timeout waiting for window label: ${label}`);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
}

export async function openSettingsWindow() {
  const settingsButton = await $('[data-testid="open-settings"]');
  await settingsButton.waitForExist({ timeout: 15000 });
  await settingsButton.click();
}

