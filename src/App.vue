<script setup lang="ts">
import { onMounted, onUnmounted, computed, watch, ref, nextTick } from 'vue';
import { useTheme } from 'vuetify';
import { useAuth, useAuthState } from './features/auth';
import AuthScreen from './features/auth/presentation/components/AuthScreen.vue';
import RecordingPopover from './presentation/components/RecordingPopover.vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useUpdater } from './composables/useUpdater';
import { getWindowMode, type AppWindowLabel } from './windowing/windowMode';
import { SettingsWindow } from './features/settings';
import { i18n } from './i18n';

const auth = useAuth();
const authState = useAuthState();
const theme = useTheme();
const { setupUpdateListener, cleanupUpdateListener } = useUpdater();

let unlistenConfigChanged: UnlistenFn | null = null;
let unlistenUiThemeChange: UnlistenFn | null = null;
let unlistenUiLocaleChange: UnlistenFn | null = null;

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
    try {
      // Важно: если окно было скрыто в промежуточном состоянии (например, auth=true → auth=false),
      // оно должно уметь снова показаться, иначе получается "приложение запущено, но окна нет".
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

function applyUiSettingsFromStorage(): void {
  const storedTheme = localStorage.getItem('uiTheme') || 'dark';
  applyThemeValue(storedTheme);

  const storedLocale = localStorage.getItem('uiLocale');
  if (storedLocale) {
    applyLocaleValue(storedLocale);
  }
}

function applyThemeValue(value: string): void {
  theme.global.name.value = value;

  if (value === 'light') {
    document.documentElement.classList.add('theme-light');
  } else {
    document.documentElement.classList.remove('theme-light');
  }
}

function applyLocaleValue(value: string): void {
  i18n.global.locale.value = value;
  localStorage.setItem('uiLocale', value);
}

// Синхронизация темы с localStorage
const storedTheme = localStorage.getItem('uiTheme') || 'dark';
theme.global.name.value = storedTheme;

watch(() => theme.global.name.value, (newTheme) => {
  localStorage.setItem('uiTheme', String(newTheme));
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
    // Важно: сначала прячем текущее окно (иногда hide из backend может не отработать вовремя).
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
      // Если юзер разлогинился в настройках — закрываем настройки и показываем auth.
      try {
        await getCurrentWindow().hide();
      } catch {}
      await invoke('show_auth_window');
    }
  } catch (e) {
    console.warn('Failed to switch windows:', e);
  }
});

onMounted(async () => {
  // Настраиваем глобальный listener для обновлений
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

    // Синхронизируем флаг авторизации и токен с backend
    const isAuth = authState.isAuthenticated.value;
    const token = isAuth ? authState.accessToken.value : null;
    await invoke('set_authenticated', { authenticated: isAuth, token });

    // После инициализации показываем нужное окно
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

  // Синхронизация между окнами: изменения конфига и авторизации
  const currentWindow = getCurrentWindow();
  unlistenConfigChanged = await listen<{
    revision: number;
    ts: number;
    source_window?: string | null;
    scope?: string | null;
  }>('config:changed', async (event) => {
    const source = event.payload?.source_window ?? null;
    if (source && source === currentWindow.label) return;

    const scope = event.payload?.scope ?? null;
    if (scope === 'auth') {
      await runExternalAuthSync(() => auth.initialize({ silent: true }));
      return;
    }

    applyUiSettingsFromStorage();
  });

  unlistenUiThemeChange = await listen<{ theme: string; sourceWindow?: string }>(
    'ui:theme-changed',
    async (event) => {
      applyThemeValue(event.payload.theme);
    }
  );

  unlistenUiLocaleChange = await listen<{ locale: string; sourceWindow?: string }>(
    'ui:locale-changed',
    async (event) => {
      applyLocaleValue(event.payload.locale);
    }
  );
});

onUnmounted(() => {
  if (unlistenConfigChanged) {
    unlistenConfigChanged();
  }
  if (unlistenUiThemeChange) {
    unlistenUiThemeChange();
  }
  if (unlistenUiLocaleChange) {
    unlistenUiLocaleChange();
  }
  cleanupUpdateListener();
});

// HMR в dev иногда не размонтирует компонент "чисто".
// На всякий случай отписываемся от tauri listeners при замене модуля,
// чтобы не словить дублирование и повторные sync/переключения окон.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    try {
      if (unlistenConfigChanged) unlistenConfigChanged();
      if (unlistenUiThemeChange) unlistenUiThemeChange();
      if (unlistenUiLocaleChange) unlistenUiLocaleChange();
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
