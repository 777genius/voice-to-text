import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, reactive, ref } from 'vue';
import AudioVisualizer from './AudioVisualizer.vue';
import type { AudioMeterOwner } from '../../composables/useAudioVisualizer';

const fixture = vi.hoisted(() => ({ owner: null as null | (() => AudioMeterOwner | null), store: {} as any, handler: null as any }));
vi.mock('@tauri-apps/api/event', () => ({ listen: async (_name: string, handler: unknown) => {
  fixture.handler = handler;
  return () => {};
} }));
vi.mock('../../utils/tauri', () => ({ isTauriAvailable: () => true }));
vi.mock('../../stores/transcription', () => ({ useTranscriptionStore: () => fixture.store }));
vi.mock('../../composables/useAudioVisualizer', () => ({
  useAudioVisualizer: (_active: unknown, options: any) => {
    fixture.owner = options.getOwner;
    return { bars: ref([]) };
  },
}));

afterEach(() => vi.restoreAllMocks());

describe('AudioVisualizer outgoing session ownership', () => {
  it.each([false, true])('routes outgoing translation without a capture generation (readiness protocol %s)', async (protocol) => {
    fixture.store = reactive({
      activeRecordingMode: 'live_translation', sessionId: 41,
      isStarting: false, isRecording: true, hasCaptureReadinessProtocol: protocol,
      isCaptureReady: false, captureRunId: null,
      incomingTranslationSessionId: 99, incomingTranslationStatus: 'Recording',
    });
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    vi.spyOn(window, 'requestAnimationFrame').mockReturnValue(1);
    const root = document.createElement('div');
    const app = createApp(AudioVisualizer, { active: true });
    app.mount(root);
    try {
      expect(fixture.owner!()).toEqual({ runId: 41, kind: 'translation' });
      const { TauriAudioSpectrumSource } = await vi.importActual<typeof import('../../composables/useAudioVisualizer')>('../../composables/useAudioVisualizer');
      const source = new TauriAudioSpectrumSource(fixture.owner!);
      const received = vi.fn();
      await source.start(received);
      fixture.handler({ payload: { run_id: 99, capture_generation: null, bars: [0.1] } });
      fixture.handler({ payload: { run_id: 41, capture_generation: 1, bars: [0.2] } });
      fixture.handler({ payload: { run_id: 41, capture_generation: null, bars: [0.8] } });
      expect(received).toHaveBeenCalledExactlyOnceWith([0.8]);
      source.stop();
      fixture.store.activeRecordingMode = 'dictation';
      fixture.store.incomingTranslationSessionId = null;
      fixture.store.isCaptureReady = true;
      fixture.store.captureRunId = 42;
      fixture.store.captureGeneration = 3;
      expect(fixture.owner!()).toEqual({ runId: 42, kind: 'capture', captureGeneration: 3 });
    } finally {
      app.unmount();
    }
  });
});
