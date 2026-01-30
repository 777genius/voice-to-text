import { describe, expect, it } from 'vitest';
import { getWindowMode } from './windowMode';

describe('getWindowMode', () => {
  it('returns loading before initialization', () => {
    expect(
      getWindowMode({
        windowLabel: 'main',
        isInitialized: false,
        isAuthenticated: false,
      })
    ).toEqual({ render: 'loading', desiredWindow: null });
  });

  it('main window: authenticated -> render main', () => {
    expect(
      getWindowMode({
        windowLabel: 'main',
        isInitialized: true,
        isAuthenticated: true,
      })
    ).toEqual({ render: 'main', desiredWindow: null });
  });

  it('main window: unauthenticated -> render none, request auth window', () => {
    expect(
      getWindowMode({
        windowLabel: 'main',
        isInitialized: true,
        isAuthenticated: false,
      })
    ).toEqual({ render: 'none', desiredWindow: 'auth' });
  });

  it('auth window: unauthenticated -> render auth', () => {
    expect(
      getWindowMode({
        windowLabel: 'auth',
        isInitialized: true,
        isAuthenticated: false,
      })
    ).toEqual({ render: 'auth', desiredWindow: null });
  });

  it('auth window: authenticated -> render none, request main window', () => {
    expect(
      getWindowMode({
        windowLabel: 'auth',
        isInitialized: true,
        isAuthenticated: true,
      })
    ).toEqual({ render: 'none', desiredWindow: 'main' });
  });

  it('settings window: unauthenticated -> render none, request auth window', () => {
    expect(
      getWindowMode({
        windowLabel: 'settings',
        isInitialized: true,
        isAuthenticated: false,
      })
    ).toEqual({ render: 'none', desiredWindow: 'auth' });
  });

  it('settings window: authenticated -> render settings', () => {
    expect(
      getWindowMode({
        windowLabel: 'settings',
        isInitialized: true,
        isAuthenticated: true,
      })
    ).toEqual({ render: 'settings', desiredWindow: null });
  });
});

