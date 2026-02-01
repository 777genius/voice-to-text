/**
 * Composable для управления темой в настройках
 * Синхронизирует Vuetify тему с CSS переменными
 */

import { computed } from 'vue';
import { useTheme } from 'vuetify';
import { useSettingsStore } from '../../store/settingsStore';
import type { AppTheme } from '../../domain/types';

// Флаг для предотвращения повторной инициализации
let themeInitialized = false;

export function useSettingsTheme() {
  const store = useSettingsStore();
  const vuetifyTheme = useTheme();

  const currentTheme = computed({
    get: () => store.theme,
    set: (value: AppTheme) => setTheme(value),
  });

  const isDark = computed(() => store.theme === 'dark');

  /**
   * Установить тему
   */
  function setTheme(theme: AppTheme): void {
    store.setTheme(theme);

    // Синхронизируем с Vuetify
    vuetifyTheme.global.name.value = theme;
  }

  /**
   * Переключить тему
   */
  function toggleTheme(): void {
    setTheme(store.theme === 'dark' ? 'light' : 'dark');
  }

  /**
   * Инициализировать тему из localStorage (вызывается один раз)
   */
  function initializeTheme(): void {
    if (themeInitialized) return;
    themeInitialized = true;

    const savedTheme = localStorage.getItem('uiTheme') as AppTheme | null;
    const theme = savedTheme ?? 'dark';

    // Устанавливаем в store (без повторного сохранения в localStorage)
    store.theme = theme;

    // Синхронизируем с Vuetify
    vuetifyTheme.global.name.value = theme;

    // Устанавливаем CSS класс
    if (theme === 'light') {
      document.documentElement.classList.add('theme-light');
    } else {
      document.documentElement.classList.remove('theme-light');
    }
  }

  return {
    currentTheme,
    isDark,
    setTheme,
    toggleTheme,
    initializeTheme,
  };
}
