import { describe, expect, it } from 'vitest';
import { normalizeUiLocale, normalizeUiTheme } from './i18n.locales';

describe('i18n.locales', () => {
  it('normalizeUiLocale returns a safe default for invalid values', () => {
    expect(normalizeUiLocale(null)).toBe('ru');
    expect(normalizeUiLocale(undefined)).toBe('ru');
    expect(normalizeUiLocale('')).toBe('ru');
    expect(normalizeUiLocale('zz')).toBe('ru');
    expect(normalizeUiLocale('en')).toBe('en');
    expect(normalizeUiLocale('uk')).toBe('uk');
  });

  it('normalizeUiTheme returns a safe default for invalid values', () => {
    expect(normalizeUiTheme(null)).toBe('dark');
    expect(normalizeUiTheme(undefined)).toBe('dark');
    expect(normalizeUiTheme('')).toBe('dark');
    expect(normalizeUiTheme('neon')).toBe('dark');
    expect(normalizeUiTheme('dark')).toBe('dark');
    expect(normalizeUiTheme('light')).toBe('light');
  });
});

