/**
 * Переиспользуемый механизм скролла к секции настроек с подсветкой.
 * Используется при открытии настроек с указанием целевой секции (например, выбор устройства).
 */

import { nextTick, onMounted, onUnmounted } from 'vue';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { isTauriAvailable } from '@/utils/tauri';

export const SETTINGS_SECTION_AUDIO_DEVICE = 'audio-device';
export const SETTINGS_SECTION_HOTKEY = 'hotkey';
export const SETTINGS_SECTION_LANGUAGE = 'language';
export const SETTINGS_SECTION_THEME = 'theme';

export const SETTINGS_SECTION_IDS = [
  SETTINGS_SECTION_AUDIO_DEVICE,
  SETTINGS_SECTION_HOTKEY,
  SETTINGS_SECTION_LANGUAGE,
  SETTINGS_SECTION_THEME,
] as const;

export type SettingsSectionId = (typeof SETTINGS_SECTION_IDS)[number];

const FLASH_DURATION_MS = 2200;
const SETTINGS_SECTION_FLASH_CLASS = 'settings-section-flash';

export function createSettingsSectionFlashController(durationMs = FLASH_DURATION_MS) {
  const timers = new Map<HTMLElement, number>();

  const flash = (el: HTMLElement): void => {
    const existingTimer = timers.get(el);
    if (existingTimer !== undefined) {
      window.clearTimeout(existingTimer);
    }

    el.classList.remove(SETTINGS_SECTION_FLASH_CLASS);
    void el.offsetWidth;
    el.classList.add(SETTINGS_SECTION_FLASH_CLASS);

    const timer = window.setTimeout(() => {
      if (timers.get(el) !== timer) return;
      timers.delete(el);
      el.classList.remove(SETTINGS_SECTION_FLASH_CLASS);
    }, durationMs);
    timers.set(el, timer);
  };

  const cleanup = (): void => {
    for (const [el, timer] of timers) {
      window.clearTimeout(timer);
      el.classList.remove(SETTINGS_SECTION_FLASH_CLASS);
    }
    timers.clear();
  };

  return { flash, cleanup };
}

export function useSettingsScrollToSection(scrollContainerRef: { value: HTMLElement | null }) {
  const sectionFlash = createSettingsSectionFlashController();

  const scrollToSection = (sectionId: string | null): boolean => {
    if (!sectionId) return false;
    const container = scrollContainerRef.value;
    if (!container) return false;

    const el = container.querySelector<HTMLElement>(
      `[data-settings-section="${sectionId}"]`
    );
    if (!el) return false;

    el.scrollIntoView({ behavior: 'smooth', block: 'center' });
    sectionFlash.flash(el);
    return true;
  };

  onUnmounted(() => {
    sectionFlash.cleanup();
  });

  return { scrollToSection };
}

export interface SettingsWindowOpenedPayload {
  scrollToSection?: string | null;
}

export function useSettingsScrollToSectionListener(
  scrollContainerRef: { value: HTMLElement | null }
) {
  const { scrollToSection } = useSettingsScrollToSection(scrollContainerRef);
  let unlisten: UnlistenFn | null = null;
  let isUnmounted = false;

  onMounted(async () => {
    isUnmounted = false;
    if (!isTauriAvailable()) return;

    const nextUnlisten = await listen<SettingsWindowOpenedPayload>(
      'settings-window-opened',
      async (event) => {
        const payload = event.payload;
        const targetSection =
          payload && typeof payload === 'object' && 'scrollToSection' in payload
            ? (payload as SettingsWindowOpenedPayload).scrollToSection
            : null;

        if (!targetSection) return;

        await nextTick();
        scrollToSection(targetSection);
      }
    );
    if (isUnmounted) {
      nextUnlisten();
      return;
    }
    unlisten = nextUnlisten;
  });

  onUnmounted(() => {
    isUnmounted = true;
    if (unlisten) {
      unlisten();
      unlisten = null;
    }
  });

  return { scrollToSection };
}
