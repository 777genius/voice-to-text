import { open } from '@tauri-apps/plugin-shell';
import type { IUrlOpener } from '../../application/ports/IUrlOpener';

/**
 * Адаптер для открытия URL через Tauri shell plugin
 */
export class TauriUrlOpener implements IUrlOpener {
  async open(url: string): Promise<void> {
    await open(url);
  }
}
