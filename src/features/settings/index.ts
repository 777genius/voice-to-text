/**
 * Settings Feature - Public API
 * Экспортирует только то, что нужно использовать снаружи модуля
 */

// Domain types
export type {
  AppTheme,
  LanguageOption,
  ProviderOption,
  AudioDeviceOption,
  WhisperModelOption,
  SettingsState,
  SaveStatus,
} from './domain/types';

// Store
export { useSettingsStore } from './store/settingsStore';

// Composables
export { useSettings } from './presentation/composables/useSettings';
export { useSettingsTheme } from './presentation/composables/useSettingsTheme';
export { useMicrophoneTest } from './presentation/composables/useMicrophoneTest';

// Components
export { default as SettingsPanel } from './presentation/components/SettingsPanel.vue';
export { default as SettingsWindow } from './presentation/components/SettingsWindow.vue';

// Секции (для использования отдельно, если нужно)
export { default as ProviderSection } from './presentation/components/sections/ProviderSection.vue';
export { default as LanguageSection } from './presentation/components/sections/LanguageSection.vue';
export { default as ApiKeysSection } from './presentation/components/sections/ApiKeysSection.vue';
export { default as WhisperSection } from './presentation/components/sections/WhisperSection.vue';
export { default as ThemeSection } from './presentation/components/sections/ThemeSection.vue';
export { default as HotkeySection } from './presentation/components/sections/HotkeySection.vue';
export { default as SensitivitySection } from './presentation/components/sections/SensitivitySection.vue';
export { default as AutoActionsSection } from './presentation/components/sections/AutoActionsSection.vue';
export { default as AudioDeviceSection } from './presentation/components/sections/AudioDeviceSection.vue';
export { default as MicTestSection } from './presentation/components/sections/MicTestSection.vue';
export { default as UpdatesSection } from './presentation/components/sections/UpdatesSection.vue';
