import { createApp } from 'vue';
import { afterEach, describe, expect, it, vi } from 'vitest';
import FeedbackSection from './FeedbackSection.vue';

vi.mock('vue-i18n', () => ({
  useI18n: () => ({
    t: (key: string) => (key === 'settings.feedback.label' ? 'Feedback' : key),
  }),
}));

describe('FeedbackSection', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('shows the feedback email as a mail link', () => {
    const root = document.createElement('div');
    const app = createApp(FeedbackSection);
    app.component('v-icon', { template: '<span><slot /></span>' });
    document.body.appendChild(root);
    app.mount(root);

    const link = root.querySelector<HTMLAnchorElement>('.feedback-contact__email');
    expect(root.textContent).toContain('Feedback:');
    expect(link?.textContent?.trim()).toBe('quantjumppro@gmail.com');
    expect(link?.getAttribute('href')).toBe('mailto:quantjumppro@gmail.com');

    app.unmount();
  });
});
