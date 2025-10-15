/**
 * Утилиты для воспроизведения звуковых эффектов
 */

import showSoundUrl from '../assets/sounds/show.mp3';
import doneSoundUrl from '../assets/sounds/done.mp3';

// Предзагружаем звуки для быстрого воспроизведения
const showAudio = new Audio(showSoundUrl);
const doneAudio = new Audio(doneSoundUrl);

// Устанавливаем громкость
showAudio.volume = 0.5;
doneAudio.volume = 0.5;

/**
 * Проигрывает звук при открытии окна
 */
export function playShowSound(): void {
  try {
    // Перематываем на начало если звук уже играл
    showAudio.currentTime = 0;
    showAudio.play().catch(err => {
      console.warn('Failed to play show sound:', err);
    });
  } catch (err) {
    console.warn('Failed to play show sound:', err);
  }
}

/**
 * Проигрывает звук при успешном завершении записи и копировании в буфер
 */
export function playDoneSound(): void {
  try {
    // Перематываем на начало если звук уже играл
    doneAudio.currentTime = 0;
    doneAudio.play().catch(err => {
      console.warn('Failed to play done sound:', err);
    });
  } catch (err) {
    console.warn('Failed to play done sound:', err);
  }
}
