<script setup lang="ts">
import { ref, computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useAuthState } from '../composables/useAuthState';
import LoginForm from './LoginForm.vue';
import VerifyEmailForm from './VerifyEmailForm.vue';
import PasswordResetForm from './PasswordResetForm.vue';
import FlagIcon from '@/presentation/components/FlagIcon.vue';
import { UI_LOCALES, type UiLocale } from '@/i18n.locales';
import { getCurrentWindow } from '@tauri-apps/api/window';

const { t, locale } = useI18n();
const authState = useAuthState();

const currentLocale = computed(() =>
  UI_LOCALES.includes(locale.value as UiLocale) ? (locale.value as UiLocale) : 'en'
);

function changeLanguage(code: string) {
  locale.value = code;
  localStorage.setItem('uiLocale', code);
}

async function closeWindow() {
  const window = getCurrentWindow();
  await window.hide();
}

type AuthView = 'login' | 'register' | 'verify' | 'reset';
const currentView = ref<AuthView>('login');

const showVerifyEmail = computed(() => authState.needsVerification.value);

const subtitle = computed(() => {
  if (showVerifyEmail.value) {
    return t('auth.verifyEmail');
  }
  switch (currentView.value) {
    case 'login':
      return t('auth.loginTitle');
    case 'register':
      return t('auth.registerTitle');
    case 'reset':
      return t('auth.resetTitle');
    default:
      return '';
  }
});

function switchToRegister() {
  currentView.value = 'register';
}

function switchToLogin() {
  currentView.value = 'login';
}

function switchToReset() {
  currentView.value = 'reset';
}
</script>

<template>
  <div class="auth-screen" data-tauri-drag-region>
    <!-- Панель управления -->
    <div class="top-controls">
      <!-- Выбор языка -->
      <v-menu location="bottom">
        <template #activator="{ props }">
          <v-btn
            v-bind="props"
            variant="text"
            size="small"
            class="lang-btn"
          >
            <FlagIcon :locale="currentLocale" :size="20" />
          </v-btn>
        </template>
        <v-list density="compact" class="lang-list">
          <v-list-item
            v-for="code in UI_LOCALES"
            :key="code"
            :active="locale === code"
            @click="changeLanguage(code)"
          >
            <template #prepend>
              <FlagIcon :locale="code" :size="18" class="mr-2" />
            </template>
            <v-list-item-title>{{ t(`languages.${code}`) }}</v-list-item-title>
          </v-list-item>
        </v-list>
      </v-menu>

      <!-- Кнопка закрытия -->
      <v-btn
        icon
        variant="text"
        size="small"
        class="close-btn"
        @click="closeWindow"
      >
        <v-icon size="18">mdi-close</v-icon>
      </v-btn>
    </div>

    <div class="auth-card">
      <div class="auth-header">
        <v-icon size="48" color="primary" class="mb-2">mdi-microphone</v-icon>
        <div class="text-h5 font-weight-bold">{{ t('app.title') }}</div>
        <div class="text-body-2 text-medium-emphasis">
          {{ subtitle }}
        </div>
      </div>

      <div class="auth-content">
        <VerifyEmailForm
          v-if="showVerifyEmail"
          @back="switchToLogin"
        />

        <LoginForm
          v-else-if="currentView === 'login' || currentView === 'register'"
          :mode="currentView"
          @switch-to-register="switchToRegister"
          @switch-to-login="switchToLogin"
          @forgot-password="switchToReset"
        />

        <PasswordResetForm
          v-else-if="currentView === 'reset'"
          @back="switchToLogin"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
.auth-screen {
  width: 100%;
  height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  position: relative;
}

.top-controls {
  position: absolute;
  top: 12px;
  right: 12px;
  z-index: 10;
  display: flex;
  align-items: center;
  gap: 4px;
}

.lang-btn {
  opacity: 0.7;
  transition: opacity 0.2s;
  min-width: 36px !important;
  padding: 0 8px !important;
}

.lang-btn:hover {
  opacity: 1;
}

.lang-list {
  min-width: 140px;
}

.close-btn {
  opacity: 0.6;
  transition: opacity 0.2s;
}

.close-btn:hover {
  opacity: 1;
}

.auth-card {
  width: 100%;
  max-height: 100%;
  background: #1e1e1e;
  border-radius: 20px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  padding: 32px 24px;
  overflow-y: auto;
  scrollbar-width: none;
  display: flex;
  flex-direction: column;
}

.auth-card::-webkit-scrollbar {
  display: none;
}

.auth-header {
  text-align: center;
  margin-top: auto;
  margin-bottom: 24px;
}

.auth-content {
  width: 100%;
  max-width: 380px;
  margin: 0 auto auto;
}

/* Светлая тема */
.v-theme--light .auth-card {
  background: #fafafa;
  border: 1px solid rgba(0, 0, 0, 0.06);
}
</style>
