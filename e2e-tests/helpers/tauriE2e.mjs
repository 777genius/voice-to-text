export async function ensureE2E() {
  await browser.execute(() => {
    if (!window.__E2E__) throw new Error('__E2E__ is not installed');
  });
}

export async function invoke(command, args) {
  const res = await browser.executeAsync((cmd, a, done) => {
    window.__E2E__
      .invoke(cmd, a)
      .then((res) => done(res))
      .catch((err) => done({ __error: String(err) }));
  }, command, args);
  if (res && res.__error) {
    throw new Error(res.__error);
  }
  return res;
}

export async function emitEvent(event, payload) {
  const res = await browser.executeAsync((eventName, eventPayload, done) => {
    window.__E2E__
      .emitEvent(eventName, eventPayload)
      .then(() => done(null))
      .catch((err) => done({ __error: String(err) }));
  }, event, payload);
  if (res && res.__error) throw new Error(res.__error);
}

export async function waitFor(fn, { timeoutMs = 15_000, intervalMs = 200 } = {}) {
  const start = Date.now();
  let lastError = null;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    try {
      lastError = null;
      const ok = await fn();
      if (ok) return;
    } catch (err) {
      lastError = err;
    }
    if (Date.now() - start > timeoutMs) {
      const detail = lastError ? `; last error: ${lastError.message || String(lastError)}` : '';
      throw new Error(`timeout after ${timeoutMs}ms${detail}`);
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

export async function clickTauriElement(
  selector,
  { timeoutMs = 15_000, intervalMs = 200 } = {},
) {
  await waitFor(
    async () => {
      const result = await browser.execute((targetSelector) => {
        const element = document.querySelector(targetSelector);
        if (!(element instanceof HTMLElement)) {
          return { clicked: false, reason: 'element is missing' };
        }
        if (!element.isConnected) {
          return { clicked: false, reason: 'element is detached' };
        }
        if (element.matches(':disabled') || element.getAttribute('aria-disabled') === 'true') {
          return { clicked: false, reason: 'element is disabled' };
        }

        let current = element;
        while (current instanceof HTMLElement) {
          const style = getComputedStyle(current);
          if (current.hidden || current.inert) {
            return { clicked: false, reason: 'element or ancestor is hidden' };
          }
          if (style.display === 'none') {
            return { clicked: false, reason: 'element or ancestor has display:none' };
          }
          if (style.visibility === 'hidden' || style.visibility === 'collapse') {
            return { clicked: false, reason: 'element or ancestor is not visible' };
          }
          if (style.pointerEvents === 'none') {
            return { clicked: false, reason: 'element or ancestor ignores pointer events' };
          }
          current = current.parentElement;
        }

        element.click();
        return { clicked: true, reason: null };
      }, selector);

      if (!result?.clicked) {
        throw new Error(`cannot click ${selector}: ${result?.reason ?? 'unknown browser response'}`);
      }
      return true;
    },
    { timeoutMs, intervalMs },
  );
}

export async function getWindowLabelSafe() {
  return await browser.execute(() => {
    if (!window.__E2E__) return null;
    return window.__E2E__.getWindowLabel();
  });
}

export async function ensureFullRecordingLayout() {
  await waitFor(async () => {
    return await browser.execute(() => {
      if (!window.__E2E__) return false;
      window.__E2E__.useFullRecordingLayout();
      return window.__E2E__.getAppConfig().showMiniRecordingWindow === false;
    });
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
  await ensureFullRecordingLayout();
  await clickTauriElement('[data-testid="open-settings"]');
}
