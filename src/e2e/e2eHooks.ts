import type { Pinia } from 'pinia';

import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

import { useAppConfigStore } from '@/stores/appConfig';
import { useSttConfigStore } from '@/stores/sttConfig';
import { useAuthStore } from '@/features/auth/store/authStore';
import { readUiPreferencesFromStorage } from '@/windowing/stateSync';
import { createSession } from '@/features/auth/domain/entities/Session';

type E2eApi = {
  getWindowLabel: () => string;
  invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;

  getAppConfig: () => {
    revision: string;
    recordingHotkey: string;
    autoCopyToClipboard: boolean;
    autoPasteText: boolean;
    microphoneSensitivity: number;
    selectedAudioDevice: string;
  };

  getSttConfig: () => {
    revision: string;
    provider: string;
    language: string;
    keepConnectionAlive: boolean;
  };

  getUiPrefs: () => {
    theme: string;
    locale: string;
    isLight: boolean;
  };
};

declare global {
  interface Window {
    __E2E__?: E2eApi;
  }
}

/**
 * Ставит минимальные e2e-хуки на window, чтобы WebDriver тесты могли:
 * - читать состояние Pinia store'ов без DOM-хака
 * - вызывать invoke() напрямую (для сценариев, где UI слишком нестабилен)
 *
 * Важно: включается только когда VITE_E2E=1.
 */
export function installE2eHooks(pinia: Pinia): void {
  // Vite всегда кладёт env в строки.
  const enabled = import.meta.env.VITE_E2E === '1';
  if (!enabled) return;

  // E2E auth bypass:
  // Для тестов синхронизации нам не нужна проверка реальной авторизации.
  // Поэтому в e2e режиме сразу ставим authenticated, чтобы окна не прятались.
  try {
    const authStore = useAuthStore(pinia);
    const now = Date.now();
    authStore.setAuthenticated(
      createSession({
        accessToken: 'e2e-access-token',
        refreshToken: 'e2e-refresh-token',
        accessExpiresAt: new Date(now + 60 * 60 * 1000),
        refreshExpiresAt: new Date(now + 24 * 60 * 60 * 1000),
        deviceId: 'desktop-e2e',
        user: undefined,
      }),
      'e2e@local',
    );
  } catch {}

  const appConfig = useAppConfigStore(pinia);
  const sttConfig = useSttConfigStore(pinia);

  // В e2e режиме нам важно, чтобы store sync стартовал независимо от того,
  // какой именно компонент успел смонтироваться.
  // Иначе тест может “успеть” сделать update_* до подписки и получить флейк.
  try {
    void appConfig.startSync();
  } catch {}
  try {
    void sttConfig.startSync();
  } catch {}

  window.__E2E__ = {
    getWindowLabel: () => String(getCurrentWindow().label),
    invoke: (command, args) => invoke(command, args as any),
    getAppConfig: () => ({
      revision: appConfig.revision,
      recordingHotkey: appConfig.recordingHotkey,
      autoCopyToClipboard: appConfig.autoCopyToClipboard,
      autoPasteText: appConfig.autoPasteText,
      microphoneSensitivity: appConfig.microphoneSensitivity,
      selectedAudioDevice: appConfig.selectedAudioDevice,
    }),
    getSttConfig: () => ({
      revision: sttConfig.revision,
      provider: String(sttConfig.provider),
      language: sttConfig.language,
      keepConnectionAlive: sttConfig.keepConnectionAlive,
    }),
    getUiPrefs: () => {
      const prefs = readUiPreferencesFromStorage();
      return {
        theme: prefs.theme,
        locale: prefs.locale,
        isLight: document.documentElement.classList.contains('theme-light'),
      };
    },
  };
}

