import { describe, expect, it } from 'vitest';
import { normalizeAppUpdateNotes } from './updateNotes';

describe('normalizeAppUpdateNotes', () => {
  it('removes GitHub release installation instructions from in-app notes', () => {
    const md = `
See CHANGELOG for details.

## Installation

**macOS:**
- Download the \`.dmg\` file
- Drag to Applications

## Fixed
- Mini update dialog opens in the app
`.trim();

    expect(normalizeAppUpdateNotes(md)).toBe('### Fixed\n- Mini update dialog opens in the app');
  });

  it('keeps changelog-style notes and normalizes embedded level-two headings', () => {
    const md = `
## Fixed
- **Hotkey** restart after finalize drain

### Changed
- Better update indicator
`.trim();

    expect(normalizeAppUpdateNotes(md)).toBe(
      [
        '### Fixed',
        '- **Hotkey** restart after finalize drain',
        '',
        '### Changed',
        '- Better update indicator',
      ].join('\n')
    );
  });
});
