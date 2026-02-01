import { expect } from 'expect-webdriverio';
import {
  ensureE2E,
  findWindowHandleByLabel,
  invoke,
  openSettingsWindow,
  waitFor,
} from '../helpers/tauriE2e.mjs';

describe('multi-window sync (real tauri webdriver)', () => {
  it('opens settings window and syncs ui-preferences + app-config + stt-config to main', async () => {
    await ensureE2E();
    const mainHandle = await findWindowHandleByLabel('main');

    await browser.switchToWindow(mainHandle);
    await openSettingsWindow();

    const settingsHandle = await findWindowHandleByLabel('settings');
    await browser.switchToWindow(settingsHandle);
    await ensureE2E();

    // 1) ui-preferences: меняем тему/локаль
    const uiRes = await invoke('update_ui_preferences', { theme: 'light', locale: 'en' });
    if (uiRes && uiRes.__error) throw new Error(uiRes.__error);

    // 2) app-config: меняем авто-копирование
    const appRes = await invoke('update_app_config', { auto_copy_to_clipboard: true });
    if (appRes && appRes.__error) throw new Error(appRes.__error);

    // 3) stt-config: меняем язык
    const sttRes = await invoke('update_stt_config', {
      provider: 'backend',
      language: 'en',
      deepgram_api_key: null,
      assemblyai_api_key: null,
      model: null,
    });
    if (sttRes && sttRes.__error) throw new Error(sttRes.__error);

    // Проверяем, что main окно подтянуло изменения
    await browser.switchToWindow(mainHandle);
    await ensureE2E();

    await waitFor(async () => {
      const prefs = await browser.execute(() => window.__E2E__.getUiPrefs());
      return prefs.isLight === true && prefs.locale === 'en';
    });

    await waitFor(async () => {
      const cfg = await browser.execute(() => window.__E2E__.getAppConfig());
      return cfg.autoCopyToClipboard === true;
    });

    await waitFor(async () => {
      const cfg = await browser.execute(() => window.__E2E__.getSttConfig());
      return cfg.language === 'en';
    });
  });
});

