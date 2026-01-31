import { defineStore } from 'pinia';
import { ref } from 'vue';

// Хранилище состояния обновлений приложения
export const useUpdateStore = defineStore('update', () => {
  // Текущая версия приложения (null если не удалось получить)
  const currentVersion = ref<string | null>(null);

  // Доступная версия обновления (null если нет обновлений)
  const availableVersion = ref<string | null>(null);

  // Release notes для обновления
  const releaseNotes = ref<string | null>(null);

  // Флаг "вы используете последнюю версию"
  const isLatest = ref(false);

  // Флаг процесса проверки обновлений
  const isChecking = ref(false);

  // Флаг процесса установки обновления
  const isInstalling = ref(false);

  // Прогресс скачивания обновления (0-100), null если неизвестно/не идёт
  const downloadProgress = ref<number | null>(null);

  // Техническая информация о скачивании (не обязана быть всегда)
  const downloadedBytes = ref<number | null>(null);
  const totalBytes = ref<number | null>(null);

  // Сообщение об ошибке (если есть)
  const error = ref<string | null>(null);

  function setCurrentVersion(version: string | null) {
    currentVersion.value = version;
  }

  // Установить информацию о доступном обновлении
  function setAvailableUpdate(version: string, notes?: string) {
    availableVersion.value = version;
    releaseNotes.value = notes ?? null;
    isLatest.value = false;
    error.value = null;
  }

  function setDownloadProgress(payload: {
    progress: number | null;
    downloaded?: number | null;
    total?: number | null;
  }) {
    downloadProgress.value = payload.progress;
    downloadedBytes.value = payload.downloaded ?? downloadedBytes.value;
    totalBytes.value = payload.total ?? totalBytes.value;
  }

  function resetDownloadProgress() {
    downloadProgress.value = null;
    downloadedBytes.value = null;
    totalBytes.value = null;
  }

  function setLatest(value: boolean) {
    isLatest.value = value;
    if (value) {
      availableVersion.value = null;
      releaseNotes.value = null;
      resetDownloadProgress();
      error.value = null;
    }
  }

  // Сбросить состояние (при отказе от обновления)
  function dismiss() {
    availableVersion.value = null;
    releaseNotes.value = null;
    isLatest.value = false;
    resetDownloadProgress();
    error.value = null;
  }

  // Очистить ошибку
  function clearError() {
    error.value = null;
  }

  // Сбросить всё состояние
  function reset() {
    currentVersion.value = null;
    availableVersion.value = null;
    releaseNotes.value = null;
    isLatest.value = false;
    isChecking.value = false;
    isInstalling.value = false;
    resetDownloadProgress();
    error.value = null;
  }

  return {
    // State
    currentVersion,
    availableVersion,
    releaseNotes,
    isLatest,
    isChecking,
    isInstalling,
    downloadProgress,
    downloadedBytes,
    totalBytes,
    error,

    // Actions
    setCurrentVersion,
    setAvailableUpdate,
    setDownloadProgress,
    resetDownloadProgress,
    setLatest,
    dismiss,
    clearError,
    reset,
  };
});
