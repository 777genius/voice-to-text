import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { IDeepLinkListener, DeepLinkCallback, UnsubscribeFn } from '../../application/ports/IDeepLinkListener';

/**
 * Адаптер для прослушивания deep link событий в Tauri
 */
export class TauriDeepLinkListener implements IDeepLinkListener {
  async subscribe(callback: DeepLinkCallback): Promise<UnsubscribeFn> {
    const unlisten: UnlistenFn = await listen<string>('deep-link', (event) => {
      callback(event.payload);
    });

    return () => {
      unlisten();
    };
  }
}
