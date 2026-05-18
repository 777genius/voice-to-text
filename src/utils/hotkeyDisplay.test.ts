import { describe, expect, it } from 'vitest';
import { formatHotkeyForDisplay } from './hotkeyDisplay';

describe('hotkey display formatting', () => {
  it('renders CmdOrCtrl as Cmd on macOS', () => {
    expect(formatHotkeyForDisplay('CmdOrCtrl+Shift+X', 'MacIntel')).toBe('Cmd+Shift+X');
  });

  it('renders CmdOrCtrl as Ctrl outside macOS', () => {
    expect(formatHotkeyForDisplay('CmdOrCtrl+Shift+X', 'Win32')).toBe('Ctrl+Shift+X');
  });

  it('maps tauri key tokens to readable characters', () => {
    expect(formatHotkeyForDisplay('CmdOrCtrl+Backquote', 'MacIntel')).toBe('Cmd+`');
    expect(formatHotkeyForDisplay('CmdOrCtrl+IntlBackslash', 'MacIntel')).toBe('Cmd+\\');
    expect(formatHotkeyForDisplay('Alt+Minus', 'Win32')).toBe('Alt+-');
    expect(formatHotkeyForDisplay('Shift+Slash', 'Linux x86_64')).toBe('Shift+/');
  });

  it('returns an empty string for empty values', () => {
    expect(formatHotkeyForDisplay('', 'MacIntel')).toBe('');
    expect(formatHotkeyForDisplay(null, 'MacIntel')).toBe('');
    expect(formatHotkeyForDisplay(undefined, 'MacIntel')).toBe('');
  });
});
