import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, defineComponent, nextTick } from 'vue';
import { createI18n } from 'vue-i18n';
import { createPinia, setActivePinia } from 'pinia';

import StreamingProviderSection from './StreamingProviderSection.vue';
import { useSettingsStore } from '../../../store/settingsStore';
import { BackendStreamingProviderType } from '@/types';

vi.mock('@/utils/tauri', () => ({
  isTauriAvailable: () => false,
}));

const SelectStub = defineComponent({
  props: ['items', 'modelValue'],
  template: `
    <div class="select-stub" :data-value="modelValue">
      <span v-for="item in items" :key="item.value" class="select-option">
        {{ item.label }}
      </span>
    </div>
  `,
});

const AlertStub = defineComponent({
  template: '<div class="alert-stub"><slot /></div>',
});

function mountSection() {
  const pinia = createPinia();
  setActivePinia(pinia);
  const root = document.createElement('div');
  document.body.appendChild(root);
  const app = createApp(StreamingProviderSection);
  app.use(pinia);
  app.use(
    createI18n({
      legacy: false,
      locale: 'en',
      messages: {
        en: {
          settings: {
            streamingProvider: {
              label: 'Streaming provider',
              optionDeepgram: 'Deepgram (recommended)',
              optionElevenLabs: 'ElevenLabs',
              elevenLabsDelayTitle: 'ElevenLabs may reconnect before recording',
              elevenLabsDelayBody: 'Wait for the green indicator before speaking. Speech may not be recognized while connecting.',
            },
          },
        },
      },
    }),
  );
  app.component('v-select', SelectStub);
  app.component('v-alert', AlertStub);
  app.mount(root);

  return {
    store: useSettingsStore(),
    root,
    unmount() {
      app.unmount();
      root.remove();
    },
  };
}

describe('StreamingProviderSection', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  it('uses Deepgram as the recommended default without showing a warning', () => {
    const wrapper = mountSection();

    expect(wrapper.store.backendStreamingProvider).toBe(BackendStreamingProviderType.Deepgram);
    expect(wrapper.root.querySelector('.select-stub')?.getAttribute('data-value')).toBe('deepgram');
    expect(wrapper.root.textContent).toContain('Deepgram (recommended)');
    expect(wrapper.root.querySelector('[data-testid="elevenlabs-startup-delay-notice"]')).toBeNull();

    wrapper.unmount();
  });

  it('warns that speech during an ElevenLabs connection may not be recognized', async () => {
    const wrapper = mountSection();

    wrapper.store.setBackendStreamingProvider(BackendStreamingProviderType.ElevenLabs);
    await nextTick();

    const notice = wrapper.root.querySelector('[data-testid="elevenlabs-startup-delay-notice"]');
    expect(notice?.textContent).toContain('ElevenLabs may reconnect before recording');
    expect(notice?.textContent).toContain('Wait for the green indicator');
    expect(notice?.textContent).toContain('Speech may not be recognized');
    expect(notice?.textContent).not.toContain('start speaking immediately');

    wrapper.unmount();
  });
});
