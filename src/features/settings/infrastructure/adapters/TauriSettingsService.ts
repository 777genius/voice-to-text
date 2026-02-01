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
  CMD_UPDATE_APP_CONFIG,
  CMD_UPDATE_STT_CONFIG,
} from '@/windowing/stateSync';
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

  async updateSttConfig(config: SttConfigData): Promise<void> {
    await invoke(CMD_UPDATE_STT_CONFIG, {
      provider: config.provider,
      language: config.language,
      deepgram_api_key: config.deepgramApiKey,
      assemblyai_api_key: config.assemblyaiApiKey,
      model: config.model,
    });
  }

  // App конфигурация

  async getAppConfig(): Promise<AppConfigSnapshotData> {
    const snap = await invoke<TauriSnapshotEnvelope<AppConfigSnapshotData>>(
      CMD_GET_APP_CONFIG_SNAPSHOT
    );
    return snap.data;
  }

  async updateAppConfig(config: Partial<AppConfigData>): Promise<void> {
    await invoke(CMD_UPDATE_APP_CONFIG, {
      microphone_sensitivity: config.microphone_sensitivity,
      recording_hotkey: config.recording_hotkey,
      auto_copy_to_clipboard: config.auto_copy_to_clipboard,
      auto_paste_text: config.auto_paste_text,
      selected_audio_device: config.selected_audio_device,
    });
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
      device_name: deviceName,
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
    return invoke<boolean>('check_whisper_model', { model_name: modelName });
  }
}

// Singleton экземпляр
export const tauriSettingsService = new TauriSettingsService();
