import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { useUpdateStore } from '../stores/update';
import { isTauriAvailable } from '@/utils/tauri';
import { loadUpdateNotesFromChangelog } from '@/utils/changelog';
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
let updateListenerGeneration = 0;
let setupUpdateListenerPromise: Promise<void> | null = null;

async function registerUpdateListener<T>(
  generation: number,
  eventName: string,
  handler: Parameters<typeof listen<T>>[1]
): Promise<UnlistenFn | null> {
  const unlisten = await listen<T>(eventName, handler);
  if (generation !== updateListenerGeneration) {
    unlisten();
    return null;
  }
  return unlisten;
}

// Composable для работы с обновлениями приложения
// Единый источник логики обновлений для всех компонентов (DRY)
export function useUpdater() {
  const store = useUpdateStore();

  async function resolveUpdateNotes(version: string, fallbackBody?: string): Promise<string | undefined> {
    try {
      const notes = await loadUpdateNotesFromChangelog(version);
      if (notes) return notes;
    } catch (err) {
      console.warn('Failed to load update notes from changelog:', err);
    }
    return fallbackBody?.trim() ? fallbackBody : undefined;
  }

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
        const notes = await resolveUpdateNotes(update.version, update.body);
        store.setAvailableUpdate(update.version, notes);
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

  async function loadCachedAvailableUpdate(): Promise<string | null> {
    if (!isTauriAvailable()) {
      return null;
    }

    try {
      const update = await invoke<AppUpdateInfo | null>('get_cached_available_update');
      if (!update) return null;

      const notes = await resolveUpdateNotes(update.version, update.body);
      store.setAvailableUpdate(update.version, notes);
      return update.version;
    } catch (err) {
      console.error('Failed to load cached update:', err);
      return null;
    }
  }

  async function openUpdateWindow(): Promise<boolean> {
    if (!isTauriAvailable()) {
      return false;
    }

    try {
      await invoke('show_update_window');
      return true;
    } catch (err) {
      console.error('Failed to open update window:', err);
      return false;
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
    if (!isTauriAvailable()) return;

    // Предотвращаем дублирование listeners
    if (unlistenUpdateAvailable) {
      return;
    }
    if (setupUpdateListenerPromise) {
      return setupUpdateListenerPromise;
    }

    const generation = updateListenerGeneration;
    const setupPromise = setupUpdateListenerInternal(generation).finally(() => {
      if (setupUpdateListenerPromise === setupPromise) {
        setupUpdateListenerPromise = null;
      }
    });
    setupUpdateListenerPromise = setupPromise;
    return setupPromise;
  }

  async function setupUpdateListenerInternal(generation: number): Promise<void> {
    try {
      unlistenUpdateAvailable = await registerUpdateListener<AppUpdateInfo>(generation, EVENT_UPDATE_AVAILABLE, (event) => {
        console.log('Update available event received:', event.payload);
        const version = event.payload.version;
        // Сразу фиксируем факт обновления, а "что нового" подтягиваем отдельно.
        // Так мы не показываем пользователю мусор из GitHub Release, если он откроет диалог мгновенно.
        store.setAvailableUpdate(version);
        void (async () => {
          const notes = await resolveUpdateNotes(version, event.payload.body);
          if (store.availableVersion !== version) {
            return;
          }
          if (notes !== store.releaseNotes) {
            store.setAvailableUpdate(version, notes);
          }
        })();
      });
      if (!unlistenUpdateAvailable) return;

      unlistenUpdateDownloadStarted = await registerUpdateListener<{ version: string }>(
        generation,
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
      if (!unlistenUpdateDownloadStarted) return;

      unlistenUpdateDownloadProgress = await registerUpdateListener<AppUpdateDownloadProgress>(
        generation,
        EVENT_UPDATE_DOWNLOAD_PROGRESS,
        (event) => {
          store.setDownloadProgress({
            progress: event.payload.progress,
            downloaded: event.payload.downloaded,
            total: event.payload.total,
          });
        }
      );
      if (!unlistenUpdateDownloadProgress) return;

      unlistenUpdateInstalling = await registerUpdateListener<{ version: string }>(generation, EVENT_UPDATE_INSTALLING, () => {
        // Скачивание закончено — дальше будет установка.
        // Оставляем последний процент, но если его не было — сбрасываем в indeterminate.
        if (store.downloadProgress === null) {
          store.setDownloadProgress({ progress: null });
        }
      });
      if (!unlistenUpdateInstalling) return;
    } catch (err) {
      console.error('Failed to setup update listener:', err);
      cleanupUpdateListener();
    }
  }

  // Очистка listeners
  function cleanupUpdateListener(): void {
    updateListenerGeneration++;
    setupUpdateListenerPromise = null;

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
    loadCachedAvailableUpdate,
    openUpdateWindow,
    installUpdate,
    dismissUpdate,
    setupUpdateListener,
    cleanupUpdateListener,
  };
}
