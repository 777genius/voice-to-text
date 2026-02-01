import type { ITokenRepository, StoredSession } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import { createSession } from '../../domain/entities/Session';
import { createUser } from '../../domain/entities/User';

const STORAGE_KEY = 'auth_session';
const DEVICE_ID_KEY = 'device_id';
const STORE_PATH = 'auth.json';

// Флаг для определения Tauri окружения
const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

async function getStore() {
  if (!isTauri) return null;

  // Важно: НЕ кешируем Store instance.
  // В Tauri multi-window другой webview может поменять файл стора, а кешированный instance этого не увидит.
  try {
    const { Store } = await import('@tauri-apps/plugin-store');
    return await Store.load(STORE_PATH);
  } catch (e) {
    console.warn('Tauri Store not available, using localStorage fallback:', e);
    return null;
  }
}

/**
 * Реализация репозитория токенов
 * Использует tauri-plugin-store в Tauri окружении (с шифрованием на диске)
 * Fallback на localStorage для браузера/тестов
 */
export class TokenRepository implements ITokenRepository {
  async save(session: Session): Promise<void> {
    // device_id критичен для refresh: на сервере refresh токен привязан к client_id.
    // В desktop multi-window localStorage может быть изолирован между webview, поэтому
    // сохраняем deviceId и в сессию, и отдельным ключом в store/localStorage.
    let deviceId = session.deviceId;

    const data: StoredSession = {
      accessToken: session.accessToken,
      refreshToken: session.refreshToken,
      accessExpiresAt: session.accessExpiresAt.toISOString(),
      refreshExpiresAt: session.refreshExpiresAt?.toISOString(),
      deviceId: deviceId,
      user: session.user
        ? {
            id: session.user.id,
            email: session.user.email,
            emailVerified: session.user.emailVerified,
          }
        : undefined,
    };

    const store = await getStore();
    if (store) {
      // Если deviceId не пришёл из доменной сессии, пытаемся подтянуть его из стора.
      if (!deviceId) {
        const storedDeviceId = (await store.get(DEVICE_ID_KEY)) as string | null;
        if (storedDeviceId) {
          deviceId = storedDeviceId;
          data.deviceId = storedDeviceId;
          localStorage.setItem(DEVICE_ID_KEY, storedDeviceId);
        }
      }

      // Если всё равно нет — генерируем и сохраняем (один раз на устройство).
      if (!deviceId) {
        deviceId = `desktop-${crypto.randomUUID()}`;
        data.deviceId = deviceId;
        localStorage.setItem(DEVICE_ID_KEY, deviceId);
        await store.set(DEVICE_ID_KEY, deviceId);
      } else {
        // Для надёжности держим device_id в store тоже (чтобы новые окна могли его прочитать).
        const existing = (await store.get(DEVICE_ID_KEY)) as string | null;
        if (!existing) {
          await store.set(DEVICE_ID_KEY, deviceId);
        }
      }

      await store.set(STORAGE_KEY, data);
      await store.save();
    } else {
      // Browser/test fallback
      if (!deviceId) {
        deviceId = localStorage.getItem(DEVICE_ID_KEY) || `desktop-${crypto.randomUUID()}`;
        localStorage.setItem(DEVICE_ID_KEY, deviceId);
        data.deviceId = deviceId;
      }
      localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
    }
  }

  async get(): Promise<Session | null> {
    let stored: StoredSession | null = null;
    let deviceIdFromStore: string | null = null;

    const store = await getStore();
    if (store) {
      stored = (await store.get(STORAGE_KEY)) as StoredSession | null;
      deviceIdFromStore = (await store.get(DEVICE_ID_KEY)) as string | null;
    } else {
      const data = localStorage.getItem(STORAGE_KEY);
      if (data) {
        try {
          stored = JSON.parse(data);
        } catch {
          return null;
        }
      }
      deviceIdFromStore = localStorage.getItem(DEVICE_ID_KEY);
    }

    if (!stored) return null;

    try {
      const deviceId = stored.deviceId || deviceIdFromStore || undefined;
      return createSession({
        accessToken: stored.accessToken,
        refreshToken: stored.refreshToken,
        accessExpiresAt: new Date(stored.accessExpiresAt),
        refreshExpiresAt: stored.refreshExpiresAt
          ? new Date(stored.refreshExpiresAt)
          : undefined,
        deviceId,
        user: stored.user
          ? createUser({
              id: stored.user.id,
              email: stored.user.email,
              emailVerified: stored.user.emailVerified,
            })
          : undefined,
      });
    } catch {
      return null;
    }
  }

  async clear(): Promise<void> {
    const store = await getStore();
    if (store) {
      await store.delete(STORAGE_KEY);
      await store.save();
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
  }

  getDeviceId(): string {
    // В идеале device_id должен быть одинаковым во всех окнах приложения.
    // Для Tauri храним в plugin-store, иначе — в localStorage.
    //
    // Это не секретные данные, но они участвуют в привязке refresh токена на сервере.
    // Если device_id "прыгает", refresh начнёт стабильно падать.
    if (isTauri) {
      // В этой ветке избегаем async API на горячем пути: будем использовать localStorage как fallback,
      // но попытаемся синхронизировать в store, когда можем.
      let deviceId = localStorage.getItem(DEVICE_ID_KEY);
      if (!deviceId) {
        deviceId = `desktop-${crypto.randomUUID()}`;
        localStorage.setItem(DEVICE_ID_KEY, deviceId);
        void (async () => {
          const store = await getStore();
          if (!store) return;
          const existing = (await store.get(DEVICE_ID_KEY)) as string | null;
          if (existing) {
            localStorage.setItem(DEVICE_ID_KEY, existing);
            return;
          }
          await store.set(DEVICE_ID_KEY, deviceId);
          await store.save();
        })();
      } else {
        void (async () => {
          const store = await getStore();
          if (!store) return;
          const existing = (await store.get(DEVICE_ID_KEY)) as string | null;
          if (existing) {
            if (existing !== deviceId) {
              localStorage.setItem(DEVICE_ID_KEY, existing);
            }
            return;
          }
          await store.set(DEVICE_ID_KEY, deviceId);
          await store.save();
        })();
      }
      return deviceId;
    }

    let deviceId = localStorage.getItem(DEVICE_ID_KEY);
    if (!deviceId) {
      deviceId = `desktop-${crypto.randomUUID()}`;
      localStorage.setItem(DEVICE_ID_KEY, deviceId);
    }
    return deviceId;
  }
}

// Singleton для использования в apiClient и DI контейнере
let tokenRepositoryInstance: TokenRepository | null = null;

export function getTokenRepository(): TokenRepository {
  if (!tokenRepositoryInstance) {
    tokenRepositoryInstance = new TokenRepository();
  }
  return tokenRepositoryInstance;
}

// Для тестов
export function resetTokenRepository(): void {
  tokenRepositoryInstance = null;
}
