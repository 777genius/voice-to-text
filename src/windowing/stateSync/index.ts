export {
  TOPIC_APP_CONFIG,
  TOPIC_STT_CONFIG,
  TOPIC_AUTH_STATE,
  TOPIC_UI_PREFERENCES,
} from './topics';

export type { RevisionSyncHandle } from '@statesync/core';
export { createTauriRevisionSync } from '@statesync/tauri';
export { createPiniaSnapshotApplier } from '@statesync/pinia';

export {
  STATE_SYNC_INVALIDATION_EVENT,
  CMD_GET_APP_CONFIG_SNAPSHOT,
  CMD_GET_STT_CONFIG_SNAPSHOT,
  CMD_GET_AUTH_STATE_SNAPSHOT,
  CMD_GET_UI_PREFERENCES_SNAPSHOT,
  CMD_UPDATE_UI_PREFERENCES,
  CMD_UPDATE_APP_CONFIG,
  CMD_UPDATE_STT_CONFIG,
  type StateSyncInvalidationEventPayload,
} from './tauri';

export {
  UI_PREFS_THEME_KEY,
  UI_PREFS_LOCALE_KEY,
  UI_PREFS_USE_SYSTEM_THEME_KEY,
  UI_PREFS_REVISION_KEY,
  UI_PREFS_MIGRATED_TO_RUST_KEY,
  getUiPrefsRevision,
  bumpUiPrefsRevision,
  readUiPreferencesFromStorage,
  writeUiPreferencesCacheToStorage,
  writeUiPreferencesToStorage,
  type UiPreferences,
} from './uiPreferences';

export type {
  AppConfigSnapshotData,
  SttConfigSnapshotData,
  AuthStateSnapshotData,
  UiPreferencesSnapshotData,
  TauriSnapshotEnvelope,
} from './contracts';

export { createAuthStateSync } from './authStateSync';
export { createUiPreferencesSync } from './uiPreferencesSync';

export { createStoreTauriTopicSync } from './storeSync';
