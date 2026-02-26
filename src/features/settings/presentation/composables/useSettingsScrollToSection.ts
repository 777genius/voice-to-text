/**
 * Переиспользуемый механизм скролла к секции настроек с подсветкой.
 * Используется при открытии настроек с указанием целевой секции (например, выбор устройства).
 */

import { ref, nextTick, onMounted, onUnmounted } from 'vue';
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

export function useSettingsScrollToSection(scrollContainerRef: { value: HTMLElement | null }) {
  const scrollToSection = (sectionId: string | null): boolean => {
    if (!sectionId) return false;
    const container = scrollContainerRef.value;
    if (!container) return false;

    const el = container.querySelector<HTMLElement>(
      `[data-settings-section="${sectionId}"]`
    );
    if (!el) return false;

    el.scrollIntoView({ behavior: 'smooth', block: 'center' });
    el.classList.add('settings-section-flash');
    setTimeout(() => {
      el.classList.remove('settings-section-flash');
    }, FLASH_DURATION_MS);
    return true;
  };

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

  onMounted(async () => {
    if (!isTauriAvailable()) return;

    unlisten = await listen<SettingsWindowOpenedPayload>(
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
  });

  onUnmounted(() => {
    if (unlisten) {
      unlisten();
      unlisten = null;
    }
  });

  return { scrollToSection };
}
