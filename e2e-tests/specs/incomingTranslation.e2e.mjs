import { emitEvent, ensureE2E, findWindowHandleByLabel, waitFor } from '../helpers/tauriE2e.mjs';

describe('incoming translation subtitles (real tauri webdriver)', () => {
  it('renders translated system audio events and ignores late events from a closed session', async () => {
    await ensureE2E();
    const mainHandle = await findWindowHandleByLabel('main');
    await browser.switchToWindow(mainHandle);

    await emitEvent('incoming_translation:status', {
      session_id: 901,
      status: 'Recording',
    });
    await emitEvent('incoming_translation:source-final', {
      session_id: 901,
      text: 'hello from zoom',
      timestamp: 1,
    });
    await emitEvent('incoming_translation:delta', {
      session_id: 901,
      text: 'привет из zoom',
      timestamp: 2,
    });

    const panel = await $('[data-testid="incoming-translation-panel"]');
    await panel.waitForExist({ timeout: 15_000 });
    const text = await $('[data-testid="incoming-translation-text"]');

    await waitFor(async () => (await text.getText()).includes('привет из zoom'));
    const firstVisibleText = await text.getText();
    if (!firstVisibleText.includes('привет из zoom')) {
      throw new Error(`incoming translation text was not rendered: ${firstVisibleText}`);
    }

    await emitEvent('incoming_translation:status', {
      session_id: 901,
      status: 'Idle',
    });
    await emitEvent('incoming_translation:status', {
      session_id: 901,
      status: 'Recording',
    });
    await emitEvent('incoming_translation:delta', {
      session_id: 901,
      text: 'поздний перевод',
      timestamp: 3,
    });

    await browser.pause(250);
    const afterLateEventText = await text.getText();
    if (afterLateEventText.includes('поздний перевод')) {
      throw new Error(`late closed-session translation leaked into UI: ${afterLateEventText}`);
    }

    await emitEvent('incoming_translation:status', {
      session_id: 902,
      status: 'Recording',
    });
    await emitEvent('incoming_translation:delta', {
      session_id: 902,
      text: 'новая сессия',
      timestamp: 4,
    });

    await waitFor(async () => (await text.getText()).includes('новая сессия'));
    const secondVisibleText = await text.getText();
    if (!secondVisibleText.includes('новая сессия')) {
      throw new Error(`new incoming translation session was not rendered: ${secondVisibleText}`);
    }

    await emitEvent('incoming_translation:status', {
      session_id: 903,
      status: 'Recording',
    });
    await emitEvent('incoming_translation:delta', {
      session_id: 903,
      text: 'перевод перед ошибкой',
      timestamp: 5,
    });
    await waitFor(async () => (await text.getText()).includes('перевод перед ошибкой'));

    await emitEvent('incoming_translation:status', {
      session_id: 903,
      status: 'Error',
    });
    await waitFor(async () => {
      const terminalText = await text.getText();
      return terminalText.length > 0 && !terminalText.includes('перевод перед ошибкой');
    });

    await emitEvent('incoming_translation:error', {
      session_id: 903,
      error: 'temporary network blip',
      error_type: 'connection',
    });
    await waitFor(async () => (await text.getText()).includes('temporary network blip'));

    await emitEvent('incoming_translation:status', {
      session_id: 904,
      status: 'Recording',
    });
    await emitEvent('incoming_translation:delta', {
      session_id: 904,
      text: 'после ошибки',
      timestamp: 6,
    });
    await waitFor(async () => (await text.getText()).includes('после ошибки'));
    const afterErrorRecoveryText = await text.getText();
    if (afterErrorRecoveryText.includes('temporary network blip')) {
      throw new Error(`incoming translation error leaked into new session: ${afterErrorRecoveryText}`);
    }
  });
});
