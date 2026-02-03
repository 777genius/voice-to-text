/**
 * Утилиты для воспроизведения звуковых эффектов
 */

import showSoundUrl from '../assets/sounds/show.mp3';
import doneSoundUrl from '../assets/sounds/done.mp3';

type SoundName = 'show' | 'done';

type SoundConfig = {
  url: string;
  volume: number;
};

const SOUND_CONFIG: Record<SoundName, SoundConfig> = {
  show: { url: showSoundUrl, volume: 0.5 },
  done: { url: doneSoundUrl, volume: 0.5 },
};

let audioContext: AudioContext | null = null;
const decodedBuffers = new Map<string, Promise<AudioBuffer>>();

function getAudioContext(): AudioContext | null {
  try {
    const Ctor = window.AudioContext || (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctor) return null;
    if (!audioContext) {
      // Важно: используем Web Audio, а не <audio>, чтобы короткие UI-звуки
      // не становились "Now Playing" в macOS и не перехватывали системные media keys.
      audioContext = new Ctor({ latencyHint: 'interactive' });
    }
    return audioContext;
  } catch (err) {
    console.warn('[Sound] Failed to create AudioContext:', err);
    return null;
  }
}

async function decodeBuffer(url: string): Promise<AudioBuffer> {
  const cached = decodedBuffers.get(url);
  if (cached) return cached;

  const promise = (async () => {
    const ctx = getAudioContext();
    if (!ctx) {
      throw new Error('AudioContext is not available');
    }

    const res = await fetch(url);
    if (!res.ok) {
      throw new Error(`Failed to fetch sound: ${res.status} ${res.statusText}`);
    }
    const arr = await res.arrayBuffer();
    return await ctx.decodeAudioData(arr);
  })();

  decodedBuffers.set(url, promise);
  return promise;
}

function fallbackPlayWithHtmlAudio(url: string, volume: number): void {
  try {
    const audio = new Audio(url);
    audio.volume = volume;
    audio.addEventListener(
      'ended',
      () => {
        try {
          audio.pause();
          // Важно "отвязать" src, чтобы даже в fallback режиме не оставаться последним медиаплеером.
          audio.removeAttribute('src');
          audio.load();
        } catch {
          // ignore
        }
      },
      { once: true },
    );
    void audio.play();
  } catch (err) {
    console.warn('[Sound] Failed to play sound in fallback mode:', err);
  }
}

async function playUiSound(name: SoundName): Promise<void> {
  const { url, volume } = SOUND_CONFIG[name];

  const ctx = getAudioContext();
  if (!ctx) {
    fallbackPlayWithHtmlAudio(url, volume);
    return;
  }

  try {
    if (ctx.state === 'suspended') {
      await ctx.resume();
    }

    const buffer = await decodeBuffer(url);

    const source = ctx.createBufferSource();
    source.buffer = buffer;

    const gain = ctx.createGain();
    gain.gain.value = volume;

    source.connect(gain);
    gain.connect(ctx.destination);

    source.start(0);
  } catch (err) {
    console.warn(`[Sound] Failed to play "${name}" sound:`, err);
  }
}

/**
 * Проигрывает звук при открытии окна
 */
export function playShowSound(): void {
  void playUiSound('show');
}

/**
 * Проигрывает звук при успешном завершении записи и копировании в буфер
 */
export function playDoneSound(): void {
  void playUiSound('done');
}
