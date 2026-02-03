import { describe, expect, it } from 'vitest';
import { extractChangelogSection, normalizeChangelogSectionMarkdown } from './changelog';

describe('changelog utils', () => {
  it('extracts section for a version written as [x.y.z] header', () => {
    const input = `
# Changelog

## [0.7.0] — 2026-02-02

### Добавлено
- Первая фича
- Вторая фича

### Изменено
- Что-то поменяли

---

## [0.6.0] — 2026-02-01

### Добавлено
- Старое
`.trim();

    const section = extractChangelogSection(input, '0.7.0');
    expect(section).toContain('### Добавлено');
    expect(section).toContain('- Первая фича');
    expect(section).not.toContain('## [0.6.0]');
  });

  it('normalizes section markdown (removes --- and nested ## headers)', () => {
    const section = `
### Добавлено
- A

## [0.0.0] — should be removed

---

### Изменено
- B
`.trim();

    const md = normalizeChangelogSectionMarkdown(section);
    expect(md).toContain('### Добавлено');
    expect(md).toContain('- A');
    expect(md).toContain('### Изменено');
    expect(md).toContain('- B');
    expect(md).not.toContain('---');
    expect(md).not.toContain('## [0.0.0]');
  });
});

