/**
 * Сервис для работы с Tauri API в контексте настроек
 * Инкапсулирует все invoke вызовы к бэкенду
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { AppConfigData, SttConfigData } from '../../domain/types';
import {
  CMD_GET_APP_CONFIG_SNAPSHOT,
  CMD_GET_STT_CONFIG_SNAPSHOT,
  invokeUpdateAppConfig,
  invokeUpdateSttConfig,
} from '@/windowing/stateSync';
import type { UpdateAppConfigInvokeArgs, UpdateSttConfigInvokeArgs } from '@/windowing/stateSync';
import type { AppConfigSnapshotData, SttConfigSnapshotData, TauriSnapshotEnvelope } from '@/windowing/stateSync';

// Payload события уровня громкости
interface MicrophoneLevelPayload {
  level: number;
}

class TauriSettingsService {
  // STT конфигурация

  async getSttConfig(): Promise<SttConfigSnapshotData> {
    const snap = await invoke<TauriSnapshotEnvelope<SttConfigSnapshotData>>(
      CMD_GET_STT_CONFIG_SNAPSHOT
    );
    return snap.data;
  }

  async updateSttConfig(
    config: Partial<SttConfigData> & Pick<SttConfigData, 'provider' | 'language'>
  ): Promise<void> {
    const args: UpdateSttConfigInvokeArgs = {
      provider: config.provider,
      language: config.language,
    };

    if ('backendStreamingProvider' in config) {
      args.backendStreamingProvider = config.backendStreamingProvider;
    }
    if ('deepgramApiKey' in config) args.deepgramApiKey = config.deepgramApiKey;
    if ('assemblyaiApiKey' in config) args.assemblyaiApiKey = config.assemblyaiApiKey;
    if ('model' in config) args.model = config.model;
    if ('streamingKeyterms' in config) args.streamingKeyterms = config.streamingKeyterms;

    await invokeUpdateSttConfig(args);
  }

  // App конфигурация

  async getAppConfig(): Promise<AppConfigSnapshotData> {
    const snap = await invoke<TauriSnapshotEnvelope<AppConfigSnapshotData>>(
      CMD_GET_APP_CONFIG_SNAPSHOT
    );
    return snap.data;
  }

  async updateAppConfig(config: Partial<AppConfigData>): Promise<void> {
    // Важно: не отправляем undefined в invoke — в разных рантаймах это может вести себя по-разному.
    // Шлём только реально заданные поля.
    const args: UpdateAppConfigInvokeArgs = {};

    // В Tauri args для команд ожидаются в camelCase.
    // Rust параметры при этом остаются в snake_case (Tauri сам мапит имена).
    if (typeof config.microphone_sensitivity === 'number') {
      args.microphoneSensitivity = Math.round(config.microphone_sensitivity);
    }
    if (typeof config.recording_hotkey === 'string') {
      args.recordingHotkey = config.recording_hotkey;
    }
    if (typeof config.auto_copy_to_clipboard === 'boolean') {
      args.autoCopyToClipboard = config.auto_copy_to_clipboard;
    }
    if (typeof config.auto_paste_text === 'boolean') {
      args.autoPasteText = config.auto_paste_text;
    }
    if (typeof config.play_completion_sound === 'boolean') {
      args.playCompletionSound = config.play_completion_sound;
    }
    if (typeof config.hide_recording_window_on_hotkey === 'boolean') {
      args.hideRecordingWindowOnHotkey = config.hide_recording_window_on_hotkey;
    }
    if (typeof config.show_mini_recording_window === 'boolean') {
      args.showMiniRecordingWindow = config.show_mini_recording_window;
    }
    if (typeof config.keep_recording_until_manual_stop === 'boolean') {
      args.keepRecordingUntilManualStop = config.keep_recording_until_manual_stop;
    }
    if (typeof config.hold_to_record === 'boolean') {
      args.holdToRecord = config.hold_to_record;
    }
    if (typeof config.double_space_hotkey_enabled === 'boolean') {
      args.doubleSpaceHotkeyEnabled = config.double_space_hotkey_enabled;
    }
    if (typeof config.selected_audio_device === 'string' || config.selected_audio_device === null) {
      args.selectedAudioDevice = config.selected_audio_device ?? '';
    }
    if (config.recording_mode === 'dictation' || config.recording_mode === 'live_translation') {
      args.recordingMode = config.recording_mode;
    }
    if (typeof config.openai_api_key === 'string' || config.openai_api_key === null) {
      args.openaiApiKey = config.openai_api_key ?? '';
    }
    if (
      config.incoming_translation_delivery === 'captions_only' ||
      config.incoming_translation_delivery === 'text_and_audio'
    ) {
      args.incomingTranslationDelivery = config.incoming_translation_delivery;
    }
    if (typeof config.incoming_translation_volume === 'number') {
      args.incomingTranslationVolume = Math.max(
        0,
        Math.min(100, Math.round(config.incoming_translation_volume)),
      );
    }

    await invokeUpdateAppConfig(args);
  }

  // Аудио устройства

  async getAudioDevices(): Promise<string[]> {
    return invoke<string[]>('get_audio_devices');
  }

  // Тест микрофона

  async startMicrophoneTest(
    sensitivity: number,
    deviceName: string | null
  ): Promise<void> {
    await invoke('start_microphone_test', {
      sensitivity,
      deviceName,
    });
  }

  async stopMicrophoneTest(): Promise<number[]> {
    return invoke<number[]>('stop_microphone_test');
  }

  listenMicrophoneLevel(
    callback: (level: number) => void
  ): Promise<UnlistenFn> {
    return listen<MicrophoneLevelPayload>('microphone_test:level', (event) => {
      callback(event.payload.level);
    });
  }

  // Accessibility разрешения (macOS)

  async checkAccessibilityPermission(): Promise<boolean> {
    return invoke<boolean>('check_accessibility_permission');
  }

  async requestAccessibilityPermission(): Promise<void> {
    await invoke('request_accessibility_permission');
  }

  // Whisper модели

  async checkWhisperModel(modelName: string): Promise<boolean> {
    return invoke<boolean>('check_whisper_model', { modelName });
  }
}

// Singleton экземпляр
export const tauriSettingsService = new TauriSettingsService();
