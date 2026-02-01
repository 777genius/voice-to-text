/**
 * Единый контракт поддерживаемых UI-локалей.
 *
 * Почему отдельный файл:
 * - чтобы не дублировать списки локалей по компонентам/композаблам
 * - чтобы нормализация значения была в одном месте
 * - чтобы новые окна (settings/auth/main) всегда использовали одинаковые правила
 */

export const UI_LOCALES = ['ru', 'en', 'es', 'fr', 'de', 'uk'] as const;
export type UiLocale = (typeof UI_LOCALES)[number];

export function isUiLocale(value: string): value is UiLocale {
  return (UI_LOCALES as readonly string[]).includes(value);
}

export function normalizeUiLocale(value: string | null | undefined): UiLocale {
  if (!value) return 'ru';
  return isUiLocale(value) ? value : 'ru';
}

export const UI_THEMES = ['dark', 'light'] as const;
export type UiTheme = (typeof UI_THEMES)[number];

export function isUiTheme(value: string): value is UiTheme {
  return (UI_THEMES as readonly string[]).includes(value);
}

export function normalizeUiTheme(value: string | null | undefined): UiTheme {
  if (!value) return 'dark';
  return isUiTheme(value) ? value : 'dark';
}

