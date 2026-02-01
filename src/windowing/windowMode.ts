export type AppWindowLabel = 'main' | 'auth' | 'settings' | 'unknown';

export type AppRenderMode = 'loading' | 'main' | 'auth' | 'settings' | 'none';

export type DesiredWindow = 'main' | 'auth' | null;

export interface WindowModeInput {
  windowLabel: AppWindowLabel;
  isInitialized: boolean;
  isAuthenticated: boolean;
}

export interface WindowModeOutput {
  render: AppRenderMode;
  desiredWindow: DesiredWindow;
}

export function getWindowMode(input: WindowModeInput): WindowModeOutput {
  if (!input.isInitialized) {
    return { render: 'loading', desiredWindow: null };
  }

  // Правило: UI и поведение зависят от окна, а не от auth-состояния.
  // - main окно никогда не показывает auth-экраны
  // - auth окно никогда не показывает основной UI
  switch (input.windowLabel) {
    case 'main':
      return input.isAuthenticated
        ? { render: 'main', desiredWindow: null }
        : { render: 'none', desiredWindow: 'auth' };

    case 'auth':
      return input.isAuthenticated
        ? { render: 'none', desiredWindow: 'main' }
        : { render: 'auth', desiredWindow: null };

    case 'settings':
      // Окно настроек — только для авторизованных пользователей.
      // Если пользователь не залогинен, окно должно быть скрыто, а UI — переехать в auth.
      return input.isAuthenticated
        ? { render: 'settings', desiredWindow: null }
        : { render: 'none', desiredWindow: 'auth' };

    default:
      // В браузере/тестах: оставляем старое поведение как fallback.
      return input.isAuthenticated
        ? { render: 'main', desiredWindow: null }
        : { render: 'auth', desiredWindow: null };
  }
}

