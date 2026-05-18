function getNavigatorPlatform(): string {
  return typeof navigator === 'undefined' ? '' : navigator.platform;
}

function isMacPlatform(platform: string | null | undefined): boolean {
  return String(platform ?? '').toUpperCase().includes('MAC');
}

export function formatHotkeyForDisplay(
  raw: string | null | undefined,
  platform = getNavigatorPlatform()
): string {
  const mapped = String(raw ?? '')
    .replace(/Backquote/g, '`')
    .replace(/Minus/g, '-')
    .replace(/Equal/g, '=')
    .replace(/BracketLeft/g, '[')
    .replace(/BracketRight/g, ']')
    .replace(/IntlBackslash/g, '\\')
    .replace(/Backslash/g, '\\')
    .replace(/Semicolon/g, ';')
    .replace(/Quote/g, "'")
    .replace(/Comma/g, ',')
    .replace(/Period/g, '.')
    .replace(/Slash/g, '/');

  if (!mapped) return '';
  return isMacPlatform(platform)
    ? mapped.replace(/CmdOrCtrl/g, 'Cmd')
    : mapped.replace(/CmdOrCtrl/g, 'Ctrl');
}
