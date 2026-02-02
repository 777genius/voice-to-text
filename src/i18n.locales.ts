/**
 * Единый контракт поддерживаемых UI-локалей и STT-языков.
 *
 * Почему отдельный файл:
 * - чтобы не дублировать списки локалей по компонентам/композаблам
 * - чтобы нормализация значения была в одном месте
 * - чтобы новые окна (settings/auth/main) всегда использовали одинаковые правила
 */

// ---------------------------------------------------------------------------
// UI-локали (6 поддерживаемых переводов интерфейса)
// ---------------------------------------------------------------------------

export const UI_LOCALES = ['ru', 'en', 'es', 'fr', 'de', 'uk'] as const;
export type UiLocale = (typeof UI_LOCALES)[number];

// ---------------------------------------------------------------------------
// STT-языки (полный список Deepgram Nova-3)
// ---------------------------------------------------------------------------

export const STT_LANGUAGES = [
  'en', 'ru', 'uk', 'es', 'fr', 'de',
  'ja', 'ko', 'pt', 'it', 'nl', 'pl',
  'cs', 'sk', 'hu', 'ro', 'bg', 'hr',
  'sr', 'sl', 'bs', 'mk', 'el', 'tr',
  'da', 'sv', 'no', 'fi', 'et', 'lv',
  'lt', 'be', 'hi', 'bn', 'ta', 'te',
  'kn', 'mr', 'id', 'ms', 'vi', 'tl',
  'ca', 'ar', 'multi',
] as const;

export type SttLanguage = (typeof STT_LANGUAGES)[number];

/**
 * Маппинг STT-языка → ISO 3166-1 alpha-2 код страны для флагов.
 * CDN flagcdn.com: https://flagcdn.com/{code}.svg
 */
export const STT_FLAG_CODES: Record<SttLanguage, string> = {
  en: 'gb',
  ru: 'ru',
  uk: 'ua',
  es: 'es',
  fr: 'fr',
  de: 'de',
  ja: 'jp',
  ko: 'kr',
  pt: 'pt',
  it: 'it',
  nl: 'nl',
  pl: 'pl',
  cs: 'cz',
  sk: 'sk',
  hu: 'hu',
  ro: 'ro',
  bg: 'bg',
  hr: 'hr',
  sr: 'rs',
  sl: 'si',
  bs: 'ba',
  mk: 'mk',
  el: 'gr',
  tr: 'tr',
  da: 'dk',
  sv: 'se',
  no: 'no',
  fi: 'fi',
  et: 'ee',
  lv: 'lv',
  lt: 'lt',
  be: 'by',
  hi: 'in',
  bn: 'bd',
  ta: 'in',
  te: 'in',
  kn: 'in',
  mr: 'in',
  id: 'id',
  ms: 'my',
  vi: 'vn',
  tl: 'ph',
  ca: 'es',
  ar: 'sa',
  multi: 'un',
};

/**
 * Маппинг STT-языка → ближайшая UI-локаль (для fallback интерфейса).
 * Если языка нет — используем 'en'.
 */
export const STT_LANG_TO_UI_LOCALE: Record<SttLanguage, UiLocale> = {
  en: 'en',
  ru: 'ru',
  uk: 'uk',
  es: 'es',
  fr: 'fr',
  de: 'de',
  ja: 'en',
  ko: 'en',
  pt: 'en',
  it: 'en',
  nl: 'en',
  pl: 'en',
  cs: 'en',
  sk: 'en',
  hu: 'en',
  ro: 'en',
  bg: 'en',
  hr: 'en',
  sr: 'en',
  sl: 'en',
  bs: 'en',
  mk: 'en',
  el: 'en',
  tr: 'en',
  da: 'en',
  sv: 'en',
  no: 'en',
  fi: 'en',
  et: 'en',
  lv: 'en',
  lt: 'en',
  be: 'ru',
  hi: 'en',
  bn: 'en',
  ta: 'en',
  te: 'en',
  kn: 'en',
  mr: 'en',
  id: 'en',
  ms: 'en',
  vi: 'en',
  tl: 'en',
  ca: 'es',
  ar: 'en',
  multi: 'en',
};

// ---------------------------------------------------------------------------
// Старый маппинг для обратной совместимости
// ---------------------------------------------------------------------------

export const LOCALE_FLAG_CODES: Record<UiLocale, string> = {
  en: 'gb',
  ru: 'ru',
  uk: 'ua',
  es: 'es',
  fr: 'fr',
  de: 'de',
};

/** URL флага по коду STT-языка (SVG, CDN) */
export function getSttFlagUrl(lang: string): string {
  const code = STT_FLAG_CODES[lang as SttLanguage] ?? 'gb';
  return `https://flagcdn.com/${code}.svg`;
}

/** URL флага по коду UI-локали (SVG, CDN) */
export function getFlagUrl(locale: UiLocale): string {
  const code = LOCALE_FLAG_CODES[locale] ?? 'gb';
  return `https://flagcdn.com/${code}.svg`;
}

export function isSttLanguage(value: string): value is SttLanguage {
  return (STT_LANGUAGES as readonly string[]).includes(value);
}

export function isUiLocale(value: string): value is UiLocale {
  return (UI_LOCALES as readonly string[]).includes(value);
}

/**
 * Нормализация STT-языка: если невалидный — fallback на 'ru'.
 */
export function normalizeSttLanguage(value: string | null | undefined): SttLanguage {
  if (!value) return 'ru';
  return isSttLanguage(value) ? value : 'ru';
}

/**
 * Получить UI-локаль для заданного STT-языка.
 * Например: 'ja' → 'en', 'uk' → 'uk', 'be' → 'ru'
 */
export function sttLangToUiLocale(lang: string): UiLocale {
  if (isSttLanguage(lang)) return STT_LANG_TO_UI_LOCALE[lang];
  return 'en';
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
