import assert from 'node:assert/strict';
import test from 'node:test';

import { JSDOM } from 'jsdom';

import { clickTauriElement, ensureFullRecordingLayout } from './tauriE2e.mjs';

async function withBrowserDom(markup, run) {
  const dom = new JSDOM(markup);
  const previous = {
    browser: globalThis.browser,
    document: globalThis.document,
    getComputedStyle: globalThis.getComputedStyle,
    HTMLElement: globalThis.HTMLElement,
    window: globalThis.window,
  };

  globalThis.window = dom.window;
  globalThis.document = dom.window.document;
  globalThis.getComputedStyle = dom.window.getComputedStyle.bind(dom.window);
  globalThis.HTMLElement = dom.window.HTMLElement;
  globalThis.browser = {
    execute: async (fn, ...args) => fn(...args),
  };

  try {
    await run(dom.window.document);
  } finally {
    globalThis.browser = previous.browser;
    globalThis.document = previous.document;
    globalThis.getComputedStyle = previous.getComputedStyle;
    globalThis.HTMLElement = previous.HTMLElement;
    globalThis.window = previous.window;
    dom.window.close();
  }
}

test('clickTauriElement activates an enabled UI control', async () => {
  await withBrowserDom('<button data-testid="target">Start</button>', async (document) => {
    let clicks = 0;
    document.querySelector('[data-testid="target"]').addEventListener('click', () => {
      clicks += 1;
    });

    await clickTauriElement('[data-testid="target"]');

    assert.equal(clicks, 1);
  });
});

test('clickTauriElement waits until a temporarily disabled control is enabled', async () => {
  await withBrowserDom('<button data-testid="target" disabled>Stop</button>', async (document) => {
    const button = document.querySelector('[data-testid="target"]');
    let clicks = 0;
    button.addEventListener('click', () => {
      clicks += 1;
    });
    setTimeout(() => {
      button.disabled = false;
    }, 5);

    await clickTauriElement('[data-testid="target"]', { timeoutMs: 100, intervalMs: 2 });

    assert.equal(clicks, 1);
  });
});

test('clickTauriElement rejects a control inside a non-interactive ancestor', async () => {
  await withBrowserDom(
    '<div style="pointer-events: none"><button data-testid="target">Start</button></div>',
    async () => {
      await assert.rejects(
        clickTauriElement('[data-testid="target"]', { timeoutMs: 10, intervalMs: 2 }),
        /ancestor ignores pointer events/,
      );
    },
  );
});

test('ensureFullRecordingLayout waits for the deterministic E2E layout', async () => {
  await withBrowserDom('', async () => {
    let mini = true;
    window.__E2E__ = {
      getAppConfig: () => ({ showMiniRecordingWindow: mini }),
    };
    setTimeout(() => {
      mini = false;
    }, 5);

    await ensureFullRecordingLayout();
  });
});
