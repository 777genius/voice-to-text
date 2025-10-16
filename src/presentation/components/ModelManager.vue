<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { WhisperModelInfo, WhisperModelDownloadProgress } from '../../types';
import {
  EVENT_WHISPER_DOWNLOAD_STARTED,
  EVENT_WHISPER_DOWNLOAD_PROGRESS,
  EVENT_WHISPER_DOWNLOAD_COMPLETED,
} from '../../types';

// Состояние моделей
const models = ref<WhisperModelInfo[]>([]);
const isLoading = ref(true);
const errorMessage = ref('');

// Состояние загрузки конкретной модели
const downloadingModel = ref<string | null>(null);
const downloadProgress = ref(0);

let unlistenProgress: UnlistenFn | null = null;
let unlistenStarted: UnlistenFn | null = null;
let unlistenCompleted: UnlistenFn | null = null;

// Загрузка списка моделей
const loadModels = async () => {
  try {
    isLoading.value = true;
    errorMessage.value = '';
    models.value = await invoke<WhisperModelInfo[]>('get_available_whisper_models');
  } catch (err) {
    console.error('Failed to load models:', err);
    errorMessage.value = String(err);
  } finally {
    isLoading.value = false;
  }
};

// Загрузка модели
const downloadModel = async (modelName: string) => {
  try {
    errorMessage.value = '';
    downloadingModel.value = modelName;
    downloadProgress.value = 0;

    await invoke('download_whisper_model', { modelName });
  } catch (err) {
    console.error('Failed to download model:', err);
    errorMessage.value = String(err);
    downloadingModel.value = null;
  }
};

// Удаление модели
const deleteModel = async (modelName: string) => {
  // Подтверждение перед удалением
  const confirmed = confirm(`Вы уверены что хотите удалить модель ${modelName}?`);
  if (!confirmed) return;

  try {
    errorMessage.value = '';
    await invoke('delete_whisper_model', { modelName });

    // Перезагружаем список после удаления
    await loadModels();
  } catch (err) {
    console.error('Failed to delete model:', err);
    errorMessage.value = String(err);
  }
};

// Проверяем скачана ли модель (из описания, которое обогащается на backend)
const isModelDownloaded = (model: WhisperModelInfo): boolean => {
  return model.description.includes('Скачана');
};

// Форматируем скорость (относительно base)
const formatSpeed = (speedFactor: number): string => {
  if (speedFactor >= 1.0) {
    return `${speedFactor.toFixed(1)}x быстрее`;
  } else {
    return `${(1 / speedFactor).toFixed(1)}x медленнее`;
  }
};

// Форматируем качество
const formatQuality = (qualityFactor: number): string => {
  const percent = Math.round(qualityFactor * 100);
  return `${percent}%`;
};

onMounted(async () => {
  await loadModels();

  // Слушаем события загрузки
  unlistenStarted = await listen<string>(EVENT_WHISPER_DOWNLOAD_STARTED, (event) => {
    console.log('Download started:', event.payload);
    downloadingModel.value = event.payload;
    downloadProgress.value = 0;
  });

  unlistenProgress = await listen<WhisperModelDownloadProgress>(
    EVENT_WHISPER_DOWNLOAD_PROGRESS,
    (event) => {
      if (downloadingModel.value === event.payload.model_name) {
        downloadProgress.value = event.payload.progress;
      }
    }
  );

  unlistenCompleted = await listen<string>(EVENT_WHISPER_DOWNLOAD_COMPLETED, async (event) => {
    console.log('Download completed:', event.payload);
    downloadingModel.value = null;
    downloadProgress.value = 0;

    // Перезагружаем список моделей чтобы обновить статус
    await loadModels();
  });
});

onUnmounted(() => {
  if (unlistenProgress) unlistenProgress();
  if (unlistenStarted) unlistenStarted();
  if (unlistenCompleted) unlistenCompleted();
});
</script>

<template>
  <div class="model-manager">
    <div class="manager-header">
      <h3>Управление моделями Whisper</h3>
      <p class="manager-hint">
        Скачайте модель для использования оффлайн транскрибации.
        Рекомендуется модель <strong>small</strong> для баланса скорости и качества.
      </p>
    </div>

    <!-- Ошибки -->
    <div v-if="errorMessage" class="error-message">{{ errorMessage }}</div>

    <!-- Индикатор загрузки списка -->
    <div v-if="isLoading" class="loading-message">Загрузка списка моделей...</div>

    <!-- Список моделей -->
    <div v-else class="models-list">
      <div
        v-for="model in models"
        :key="model.name"
        class="model-card"
        :class="{ downloaded: isModelDownloaded(model) }"
      >
        <div class="model-info">
          <div class="model-header">
            <h4 class="model-name">{{ model.name }}</h4>
            <span v-if="isModelDownloaded(model)" class="badge-downloaded">✓ Скачана</span>
          </div>

          <p class="model-description">{{ model.description }}</p>

          <div class="model-specs">
            <div class="spec-item">
              <span class="spec-label">Размер:</span>
              <span class="spec-value">{{ model.size_human }}</span>
            </div>
            <div class="spec-item">
              <span class="spec-label">Скорость:</span>
              <span class="spec-value">{{ formatSpeed(model.speed_factor) }}</span>
            </div>
            <div class="spec-item">
              <span class="spec-label">Качество:</span>
              <span class="spec-value">{{ formatQuality(model.quality_factor) }}</span>
            </div>
          </div>
        </div>

        <!-- Прогресс загрузки -->
        <div
          v-if="downloadingModel === model.name"
          class="download-progress"
        >
          <div class="progress-bar">
            <div
              class="progress-fill"
              :style="{ width: `${downloadProgress}%` }"
            ></div>
          </div>
          <div class="progress-text">{{ downloadProgress }}%</div>
        </div>

        <!-- Кнопки действий -->
        <div v-else class="model-actions">
          <button
            v-if="!isModelDownloaded(model)"
            class="button-download"
            @click="downloadModel(model.name)"
            :disabled="downloadingModel !== null"
          >
            Скачать
          </button>
          <button
            v-else
            class="button-delete"
            @click="deleteModel(model.name)"
            :disabled="downloadingModel !== null"
          >
            Удалить
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.model-manager {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
}

.manager-header h3 {
  margin: 0 0 var(--spacing-xs) 0;
  font-size: 16px;
  font-weight: 600;
  color: var(--color-text);
}

.manager-hint {
  margin: 0;
  font-size: 12px;
  color: var(--color-text-secondary);
  line-height: 1.4;
}

.loading-message {
  text-align: center;
  padding: var(--spacing-md);
  color: var(--color-text-secondary);
  font-size: 13px;
}

.error-message {
  padding: var(--spacing-sm);
  background: rgba(244, 67, 54, 0.2);
  border: 1px solid rgba(244, 67, 54, 0.3);
  border-radius: var(--radius-md);
  color: #f44336;
  font-size: 13px;
}

.models-list {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
}

.model-card {
  padding: var(--spacing-md);
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: var(--radius-md);
  transition: all 0.2s ease;
}

.model-card:hover {
  background: rgba(255, 255, 255, 0.05);
  border-color: rgba(255, 255, 255, 0.15);
}

.model-card.downloaded {
  border-color: rgba(76, 175, 80, 0.3);
  background: rgba(76, 175, 80, 0.05);
}

.model-info {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-xs);
}

.model-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-sm);
}

.model-name {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text);
  text-transform: capitalize;
}

.badge-downloaded {
  padding: 2px 8px;
  background: rgba(76, 175, 80, 0.2);
  border: 1px solid rgba(76, 175, 80, 0.3);
  border-radius: var(--radius-sm);
  color: #4caf50;
  font-size: 11px;
  font-weight: 500;
}

.model-description {
  margin: 0;
  font-size: 12px;
  color: var(--color-text-secondary);
  line-height: 1.4;
}

.model-specs {
  display: flex;
  gap: var(--spacing-md);
  margin-top: var(--spacing-xs);
}

.spec-item {
  display: flex;
  gap: 4px;
  font-size: 11px;
}

.spec-label {
  color: var(--color-text-secondary);
}

.spec-value {
  color: var(--color-text);
  font-weight: 500;
}

.download-progress {
  margin-top: var(--spacing-sm);
  display: flex;
  flex-direction: column;
  gap: var(--spacing-xs);
}

.progress-bar {
  width: 100%;
  height: 8px;
  background: rgba(255, 255, 255, 0.1);
  border-radius: var(--radius-sm);
  overflow: hidden;
}

.progress-fill {
  height: 100%;
  background: linear-gradient(90deg, var(--color-accent), var(--color-accent-hover));
  border-radius: var(--radius-sm);
  transition: width 0.3s ease;
}

.progress-text {
  text-align: center;
  font-size: 12px;
  color: var(--color-text-secondary);
  font-weight: 500;
}

.model-actions {
  margin-top: var(--spacing-sm);
  display: flex;
  gap: var(--spacing-sm);
}

.button-download,
.button-delete {
  padding: 6px var(--spacing-md);
  border: none;
  border-radius: var(--radius-md);
  font-size: 13px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s ease;
  flex: 1;
}

.button-download {
  background: var(--color-accent);
  color: var(--color-text);
}

.button-download:hover:not(:disabled) {
  background: var(--color-accent-hover);
  transform: translateY(-1px);
}

.button-download:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.button-delete {
  background: rgba(244, 67, 54, 0.2);
  border: 1px solid rgba(244, 67, 54, 0.3);
  color: #f44336;
}

.button-delete:hover:not(:disabled) {
  background: rgba(244, 67, 54, 0.3);
  transform: translateY(-1px);
}

.button-delete:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
