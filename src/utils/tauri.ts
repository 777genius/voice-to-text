export function isTauriAvailable(): boolean {
  if (typeof window === 'undefined') return false;

  const w = window as any;
  return Boolean(w.__TAURI__ || w.__TAURI_INTERNALS__);
}
