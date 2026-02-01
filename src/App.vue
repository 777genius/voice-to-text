<script setup lang="ts">
import { onMounted, onUnmounted, computed, watch, ref, nextTick } from 'vue';
import { useTheme } from 'vuetify';
import { useAuth, useAuthState } from './features/auth';
import AuthScreen from './features/auth/presentation/components/AuthScreen.vue';
import RecordingPopover from './presentation/components/RecordingPopover.vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useUpdater } from './composables/useUpdater';
import { getWindowMode, type AppWindowLabel } from './windowing/windowMode';
import { SettingsWindow } from './features/settings';
import {
  bumpUiPrefsRevision,
  UI_PREFS_LOCALE_KEY,
  UI_PREFS_THEME_KEY,
  readUiPreferencesFromStorage,
  writeUiPreferencesCacheToStorage,
} from './windowing/stateSync';
import type { RevisionSyncHandle } from './windowing/stateSync';
import { isTauriAvailable } from './utils/tauri';
import { i18n } from './i18n';
import { normalizeUiLocale, normalizeUiTheme } from './i18n.locales';
import { createAuthStateSync } from './windowing/stateSync/authStateSync';
import { createUiPreferencesSync } from './windowing/stateSync/uiPreferencesSync';

const auth = useAuth();
const authState = useAuthState();
const theme = useTheme();
const { setupUpdateListener, cleanupUpdateListener } = useUpdater();

// Флаг завершения инициализации (чтобы не мелькал AuthScreen)
const isInitialized = ref(false);

// Защита от "пинг-понга" между окнами:
// при синхронизации auth из другого окна мы обновляем store у себя, но не шлём set_authenticated обратно.
let externalAuthSyncDepth = 0;

function isExternalAuthSync(): boolean {
  return externalAuthSyncDepth > 0;
}

async function runExternalAuthSync(task: () => Promise<void>): Promise<void> {
  externalAuthSyncDepth += 1;
  try {
    await task();
    // Важно: даём Vue отработать реактивные обновления,
    // чтобы watcher не успел отправить set_authenticated обратно.
    await nextTick();
  } finally {
    externalAuthSyncDepth = Math.max(0, externalAuthSyncDepth - 1);
  }
}

const windowLabel = ref<AppWindowLabel>('unknown');

const mode = computed(() =>
  getWindowMode({
    windowLabel: windowLabel.value,
    isInitialized: isInitialized.value,
    isAuthenticated: authState.isAuthenticated.value,
  })
);

const showLoading = computed(() => mode.value.render === 'loading');
const showAuth = computed(() => mode.value.render === 'auth');
const showApp = computed(() => mode.value.render === 'main');
const showSettings = computed(() => mode.value.render === 'settings');

// Если окно по правилам не должно показывать UI — прячем его, чтобы не оставалось "невидимого стекла".
watch(
  () => mode.value.render,
  async (render) => {
    if (!isInitialized.value) return;
    // При HMR окно уже видно — не трогаем фокус/видимость
    if (isHmrReload) return;
    try {
      if (render === 'none') {
      await getCurrentWindow().hide();
      } else {
        // Settings окно контролируется командами backend (show_settings_window/show_recording_window),
        // поэтому НЕ показываем его автоматически на старте.
        if (windowLabel.value !== 'settings') {
          await getCurrentWindow().show();
        }
      }
    } catch {}
  }
);

function applyThemeValue(value: string): void {
  const next = normalizeUiTheme(value);
  theme.global.name.value = next;

  // Держим localStorage в консистентном состоянии (как кэш).
  writeUiPreferencesCacheToStorage({ ...readUiPreferencesFromStorage(), theme: next });
  document.documentElement.dataset.uiTheme = next;

  if (next === 'light') {
    document.documentElement.classList.add('theme-light');
  } else {
    document.documentElement.classList.remove('theme-light');
  }
}

function applyLocaleValue(value: string): void {
  const next = normalizeUiLocale(value);
  i18n.global.locale.value = next;
  document.documentElement.dataset.uiLocale = next;
  const prev = localStorage.getItem(UI_PREFS_LOCALE_KEY);
  if (prev !== next) {
    writeUiPreferencesCacheToStorage({ ...readUiPreferencesFromStorage(), locale: next });
    if (!isTauriAvailable()) {
      bumpUiPrefsRevision();
    }
  }
}

// Синхронизация темы с localStorage
const storedTheme = normalizeUiTheme(localStorage.getItem(UI_PREFS_THEME_KEY));
theme.global.name.value = storedTheme;

watch(() => theme.global.name.value, (newTheme) => {
  const next = normalizeUiTheme(String(newTheme));
  const prev = localStorage.getItem(UI_PREFS_THEME_KEY);
  if (prev !== next) {
    writeUiPreferencesCacheToStorage({ ...readUiPreferencesFromStorage(), theme: next });
    if (!isTauriAvailable()) {
      bumpUiPrefsRevision();
    }
  }
});

// При смене состояния авторизации - синхронизируем с backend и переключаем окна
watch(() => authState.isAuthenticated.value, async (isAuth) => {
  if (!isInitialized.value) return;
  // Во время загрузки auth мы не должны "перекидывать" пользователя между окнами.
  // Это особенно критично при синхронизации между окнами и во время refresh токенов.
  if (authState.isLoading.value) return;

  try {
    if (!isExternalAuthSync()) {
      const token = isAuth ? authState.accessToken.value : null;
      console.log('[Auth] set_authenticated called, isAuth:', isAuth, 'token present:', !!token);
      await invoke('set_authenticated', { authenticated: isAuth, token });
    }

    // Переключение делаем по правилам окна, чтобы main не показывал auth UI и наоборот.
    if (windowLabel.value === 'auth' && isAuth) {
      try {
        await getCurrentWindow().hide();
      } catch {}
      await invoke('show_recording_window');
    } else if (windowLabel.value === 'main' && !isAuth) {
      try {
        await getCurrentWindow().hide();
      } catch {}
      await invoke('show_auth_window');
    } else if (windowLabel.value === 'settings' && !isAuth) {
      try {
        await getCurrentWindow().hide();
      } catch {}
      await invoke('show_auth_window');
    }
  } catch (e) {
    console.warn('Failed to switch windows:', e);
  }
});

// Per-topic sync handles
let authSyncHandle: RevisionSyncHandle | null = null;
let uiPrefsSyncHandle: RevisionSyncHandle | null = null;

onMounted(async () => {
  await setupUpdateListener();

  try {
    try {
      const label = String(getCurrentWindow().label);
      windowLabel.value =
        label === 'main' || label === 'auth' || label === 'settings'
          ? label
          : 'unknown';
    } catch {
      windowLabel.value = 'unknown';
    }

    await auth.initialize();
  } finally {
    isInitialized.value = true;

    const isAuth = authState.isAuthenticated.value;
    const token = isAuth ? authState.accessToken.value : null;
    await invoke('set_authenticated', { authenticated: isAuth, token });

    // При HMR не переключаем окна — состояние уже корректное
    if (!isHmrReload) {
    if (windowLabel.value === 'main' && !isAuth) {
      try {
        await getCurrentWindow().hide();
      } catch {}
      await invoke('show_auth_window');
    } else if (windowLabel.value === 'auth' && isAuth) {
      try {
        await getCurrentWindow().hide();
      } catch {}
      await invoke('show_recording_window');
      }
    }
  }

  // Создаём per-topic sync handles (только в Tauri окружении)
  if (isTauriAvailable()) {
    authSyncHandle = createAuthStateSync({
      listen,
      invoke,
      getLocalIsAuthenticated: () => authState.isAuthenticated.value,
      onExternalAuthState: () => {
        void runExternalAuthSync(() => auth.initialize({ silent: true }));
      },
    });

    uiPrefsSyncHandle = createUiPreferencesSync({
      listen,
      invoke,
      applyTheme: (t) => applyThemeValue(t),
      applyLocale: (l) => applyLocaleValue(l),
    });

    try {
      await Promise.all([
        authSyncHandle.start(),
        uiPrefsSyncHandle.start(),
      ]);
    } catch (err) {
      console.error('[App] state-sync start failed:', err);
      authSyncHandle?.stop(); authSyncHandle = null;
      uiPrefsSyncHandle?.stop(); uiPrefsSyncHandle = null;
    }
  }
});

onUnmounted(() => {
  if (authSyncHandle) {
    authSyncHandle.stop();
    authSyncHandle = null;
  }
  if (uiPrefsSyncHandle) {
    uiPrefsSyncHandle.stop();
    uiPrefsSyncHandle = null;
  }
  cleanupUpdateListener();
});

// HMR: при перезагрузке модуля не дёргаем show/hide/focus — окно уже на месте
let isHmrReload = false;

if (import.meta.hot) {
  // Если модуль уже был загружен ранее — значит это HMR reload
  if (import.meta.hot.data.__hmrPreviouslyMounted) {
    isHmrReload = true;
  }

  import.meta.hot.dispose((data) => {
    data.__hmrPreviouslyMounted = true;
    try {
      if (authSyncHandle) {
        authSyncHandle.stop();
        authSyncHandle = null;
      }
      if (uiPrefsSyncHandle) {
        uiPrefsSyncHandle.stop();
        uiPrefsSyncHandle = null;
      }
    } catch {}
    cleanupUpdateListener();
  });
}
</script>

<template>
  <v-app>
    <!-- Loading при инициализации -->
    <v-container v-if="showLoading" class="fill-height" fluid>
      <v-row align="center" justify="center">
        <v-progress-circular
          indeterminate
          color="primary"
          size="48"
        />
      </v-row>
    </v-container>

    <AuthScreen v-else-if="showAuth" />

    <SettingsWindow v-else-if="showSettings" />

    <div v-else-if="showApp" class="app">
      <RecordingPopover />
    </div>
  </v-app>
</template>

<style scoped>
.app {
  width: 100%;
  height: 100vh;
  display: block;
  margin: 0;
  padding: 0;
  border-radius: var(--radius-xl);
  overflow: hidden;
  background: transparent;
  position: relative;
}
</style>
