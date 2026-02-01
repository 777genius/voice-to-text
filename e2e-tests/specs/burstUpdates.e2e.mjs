import { expect } from 'expect-webdriverio';
import {
  ensureE2E,
  findWindowHandleByLabel,
  invoke,
  openSettingsWindow,
  waitFor,
} from '../helpers/tauriE2e.mjs';

describe('multi-window sync (burst updates)', () => {
  it('multiple rapid updates converge to the latest state in both windows', async () => {
    await ensureE2E();
    const mainHandle = await findWindowHandleByLabel('main');
    await browser.switchToWindow(mainHandle);
    await openSettingsWindow();

    const settingsHandle = await findWindowHandleByLabel('settings');
    await browser.switchToWindow(settingsHandle);
    await ensureE2E();

    // Burst updates: шлём несколько апдейтов подряд.
    const res1 = await invoke('update_app_config', { auto_copy_to_clipboard: false });
    if (res1 && res1.__error) throw new Error(res1.__error);
    const res2 = await invoke('update_app_config', { auto_copy_to_clipboard: true });
    if (res2 && res2.__error) throw new Error(res2.__error);

    const res3 = await invoke('update_ui_preferences', { theme: 'dark', locale: 'ru' });
    if (res3 && res3.__error) throw new Error(res3.__error);
    const res4 = await invoke('update_ui_preferences', { theme: 'light', locale: 'en' });
    if (res4 && res4.__error) throw new Error(res4.__error);

    const res5 = await invoke('update_stt_config', {
      provider: 'backend',
      language: 'ru',
      deepgram_api_key: null,
      assemblyai_api_key: null,
      model: null,
    });
    if (res5 && res5.__error) throw new Error(res5.__error);
    const res6 = await invoke('update_stt_config', {
      provider: 'backend',
      language: 'en',
      deepgram_api_key: null,
      assemblyai_api_key: null,
      model: null,
    });
    if (res6 && res6.__error) throw new Error(res6.__error);

    // Проверяем, что main окно сошлось к последним значениям.
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

