import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { useUpdateStore } from '../stores/update';
import { isTauriAvailable } from '@/utils/tauri';
import {
  EVENT_UPDATE_AVAILABLE,
  EVENT_UPDATE_DOWNLOAD_PROGRESS,
  EVENT_UPDATE_DOWNLOAD_STARTED,
  EVENT_UPDATE_INSTALLING,
  type AppUpdateDownloadProgress,
  type AppUpdateInfo,
} from '@/types';

// Singleton для listener - должен быть один на всё приложение
let unlistenUpdateAvailable: UnlistenFn | null = null;
let unlistenUpdateDownloadStarted: UnlistenFn | null = null;
let unlistenUpdateDownloadProgress: UnlistenFn | null = null;
let unlistenUpdateInstalling: UnlistenFn | null = null;

// Composable для работы с обновлениями приложения
// Единый источник логики обновлений для всех компонентов (DRY)
export function useUpdater() {
  const store = useUpdateStore();

  async function loadCurrentVersion(): Promise<string | null> {
    if (!isTauriAvailable()) {
      store.setCurrentVersion(null);
      return null;
    }

    try {
      const version = await getVersion();
      store.setCurrentVersion(version);
      return version;
    } catch (err) {
      console.error('Failed to get current app version:', err);
      store.setCurrentVersion(null);
      return null;
    }
  }

  // Проверка обновлений вручную
  async function checkForUpdates(): Promise<string | null> {
    store.isChecking = true;
    store.error = null;
    store.setLatest(false);

    try {
      if (!isTauriAvailable()) {
        return null;
      }

      const update = await invoke<AppUpdateInfo | null>('check_for_updates');

      if (update) {
        store.setAvailableUpdate(update.version, update.body);
        return update.version;
      } else {
        store.setLatest(true);
        return null;
      }
    } catch (err) {
      console.error('Failed to check for updates:', err);
      store.error = String(err);
      return null;
    } finally {
      store.isChecking = false;
    }
  }

  // Установка обновления
  async function installUpdate(): Promise<void> {
    store.isInstalling = true;
    store.error = null;
    store.resetDownloadProgress();

    try {
      await invoke('install_update');
      // После успешной установки приложение перезапустится,
      // поэтому сбрасывать состояние не нужно
    } catch (err) {
      console.error('Failed to install update:', err);
      store.error = String(err);
      store.resetDownloadProgress();
      store.isInstalling = false;
    }
  }

  // Отказ от обновления (закрыть диалог)
  function dismissUpdate(): void {
    store.dismiss();
  }

  // Настроить глобальный listener для события 'update:available'
  // Вызывается один раз в App.vue
  async function setupUpdateListener(): Promise<void> {
    // Предотвращаем дублирование listeners
    if (unlistenUpdateAvailable) {
      return;
    }

    try {
      unlistenUpdateAvailable = await listen<AppUpdateInfo>(EVENT_UPDATE_AVAILABLE, (event) => {
        console.log('Update available event received:', event.payload);
        store.setAvailableUpdate(event.payload.version, event.payload.body);
      });

      unlistenUpdateDownloadStarted = await listen<{ version: string }>(
        EVENT_UPDATE_DOWNLOAD_STARTED,
        (event) => {
          // На старте скачивания прогресс может быть неизвестен, но нам важно показать UI,
          // что процесс пошёл (даже если пока без процентов).
          store.setDownloadProgress({ progress: null, downloaded: null, total: null });
          // На всякий случай обновляем версию, если прилетела.
          if (event.payload?.version) {
            store.setAvailableUpdate(event.payload.version, store.releaseNotes ?? undefined);
          }
        }
      );

      unlistenUpdateDownloadProgress = await listen<AppUpdateDownloadProgress>(
        EVENT_UPDATE_DOWNLOAD_PROGRESS,
        (event) => {
          store.setDownloadProgress({
            progress: event.payload.progress,
            downloaded: event.payload.downloaded,
            total: event.payload.total,
          });
        }
      );

      unlistenUpdateInstalling = await listen<{ version: string }>(EVENT_UPDATE_INSTALLING, () => {
        // Скачивание закончено — дальше будет установка.
        // Оставляем последний процент, но если его не было — сбрасываем в indeterminate.
        if (store.downloadProgress === null) {
          store.setDownloadProgress({ progress: null });
        }
      });
    } catch (err) {
      console.error('Failed to setup update listener:', err);
    }
  }

  // Очистка listeners
  function cleanupUpdateListener(): void {
    if (unlistenUpdateAvailable) {
      unlistenUpdateAvailable();
      unlistenUpdateAvailable = null;
    }
    if (unlistenUpdateDownloadStarted) {
      unlistenUpdateDownloadStarted();
      unlistenUpdateDownloadStarted = null;
    }
    if (unlistenUpdateDownloadProgress) {
      unlistenUpdateDownloadProgress();
      unlistenUpdateDownloadProgress = null;
    }
    if (unlistenUpdateInstalling) {
      unlistenUpdateInstalling();
      unlistenUpdateInstalling = null;
    }
  }

  return {
    // Store state (реактивные ссылки)
    store,

    // Actions
    loadCurrentVersion,
    checkForUpdates,
    installUpdate,
    dismissUpdate,
    setupUpdateListener,
    cleanupUpdateListener,
  };
}
