import { describe, expect, it } from 'vitest';
import { renderMarkdownToSafeHtml } from './markdown';

describe('renderMarkdownToSafeHtml', () => {
  it('escapes HTML and renders basic markdown', () => {
    const md = `
### Добавлено
- **Жирный** и \`код\`
- [Link](https://example.com)

<script>alert(1)</script>
`.trim();

    const html = renderMarkdownToSafeHtml(md);
    expect(html).toContain('md-h3');
    expect(html).toContain('<strong>Жирный</strong>');
    expect(html).toContain('<code>код</code>');
    expect(html).toContain('href="https://example.com"');
    expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
    expect(html).not.toContain('<script>');
  });

  it('does not allow javascript: links', () => {
    const md = `- [X](javascript:alert(1))`;
    const html = renderMarkdownToSafeHtml(md);
    expect(html).not.toContain('href="javascript:');
  });
});

