import type { SettingsState } from './types';

export function areSettingsStatesEqual(a: SettingsState | null, b: SettingsState | null): boolean {
  if (!a || !b) return a === b;

  return (
    a.provider === b.provider &&
    a.backendStreamingProvider === b.backendStreamingProvider &&
    a.language === b.language &&
    a.deepgramApiKey === b.deepgramApiKey &&
    a.assemblyaiApiKey === b.assemblyaiApiKey &&
    a.whisperModel === b.whisperModel &&
    a.theme === b.theme &&
    a.useSystemTheme === b.useSystemTheme &&
    a.recordingHotkey === b.recordingHotkey &&
    a.microphoneSensitivity === b.microphoneSensitivity &&
    a.selectedAudioDevice === b.selectedAudioDevice &&
    a.autoCopyToClipboard === b.autoCopyToClipboard &&
    a.autoPasteText === b.autoPasteText &&
    a.playCompletionSound === b.playCompletionSound &&
    a.hideRecordingWindowOnHotkey === b.hideRecordingWindowOnHotkey &&
    a.showMiniRecordingWindow === b.showMiniRecordingWindow &&
    a.keepRecordingUntilManualStop === b.keepRecordingUntilManualStop &&
    a.deepgramKeyterms === b.deepgramKeyterms
  );
}
