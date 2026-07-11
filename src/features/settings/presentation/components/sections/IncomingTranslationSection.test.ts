import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, defineComponent, nextTick } from 'vue';
import { createI18n } from 'vue-i18n';
import { createPinia, setActivePinia } from 'pinia';

import IncomingTranslationSection from './IncomingTranslationSection.vue';
import { useSettingsStore } from '../../../store/settingsStore';

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock('@/utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

const ToggleStub = defineComponent({
  props: ['modelValue'],
  emits: ['update:modelValue'],
  template: '<div class="toggle-stub"><slot /></div>',
});

const ButtonStub = defineComponent({
  props: ['disabled', 'value'],
  template:
    '<button class="button-stub" :disabled="disabled" :data-value="value"><slot /></button>',
});

const AlertStub = defineComponent({
  template: '<div class="alert-stub"><slot /></div>',
});

const SliderStub = defineComponent({
  props: ['modelValue'],
  template: '<div class="slider-stub" :data-value="modelValue" />',
});

const TextFieldStub = defineComponent({
  props: ['modelValue'],
  template: '<input class="text-field-stub" :value="modelValue" />',
});

function flushMicrotasks() {
  return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve());
}

function mountSection() {
  const pinia = createPinia();
  setActivePinia(pinia);
  const root = document.createElement('div');
  document.body.appendChild(root);
  const app = createApp(IncomingTranslationSection);
  app.use(pinia);
  app.use(
    createI18n({
      legacy: false,
      locale: 'en',
      messages: {
        en: {
          settings: {
            incomingTranslation: {
              label: 'Incoming translation',
              captionsOnly: 'Text only',
              textAndAudio: 'Text and audio',
              volume: 'Translation volume',
              capabilityChecking: 'Checking',
              capabilities: {
                ready: 'Ready',
                unsupported_platform: 'Unsupported',
                permission_required: 'Permission required',
                unsafe_self_capture: 'Unsafe route',
                no_output_device: 'No output',
                unsupported_target_language: 'Unsupported language',
              },
            },
            openaiApiKey: {
              label: 'OpenAI API key',
              placeholder: 'Key',
            },
          },
        },
      },
    }),
  );
  app.component('v-btn-toggle', ToggleStub);
  app.component('v-btn', ButtonStub);
  app.component('v-alert', AlertStub);
  app.component('v-expand-transition', defineComponent({ template: '<div><slot /></div>' }));
  app.component('v-slider', SliderStub);
  app.component('v-text-field', TextFieldStub);
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

describe('IncomingTranslationSection capability gating', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    document.body.innerHTML = '';
  });

  it('disables text and audio when backend capability is not ready', async () => {
    invokeMock.mockResolvedValue({
      supported: false,
      capability: 'permission_required',
    });
    const wrapper = mountSection();
    await flushMicrotasks();
    await nextTick();

    const spoken = wrapper.root.querySelector<HTMLButtonElement>(
      '[data-value="text_and_audio"]',
    );
    expect(invokeMock).toHaveBeenCalledWith('get_incoming_spoken_translation_capability', {
      targetLanguage: 'ru',
    });
    expect(spoken?.disabled).toBe(true);
    expect(wrapper.root.querySelector('.alert-stub')?.textContent).toContain(
      'Permission required',
    );
    wrapper.unmount();
  });

  it('enables spoken controls and renders persisted volume after a ready preflight', async () => {
    invokeMock.mockResolvedValue({ supported: true, capability: 'ready' });
    const wrapper = mountSection();
    wrapper.store.setIncomingTranslationDelivery('text_and_audio');
    wrapper.store.setIncomingTranslationVolume(42);
    await flushMicrotasks();
    await nextTick();

    const spoken = wrapper.root.querySelector<HTMLButtonElement>(
      '[data-value="text_and_audio"]',
    );
    expect(spoken?.disabled).toBe(false);
    expect(wrapper.root.querySelector('.volume-label')?.textContent).toContain('42%');
    expect(wrapper.root.querySelector('.slider-stub')?.getAttribute('data-value')).toBe('42');
    wrapper.unmount();
  });
});
