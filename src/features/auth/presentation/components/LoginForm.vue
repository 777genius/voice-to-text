<script setup lang="ts">
import { ref, computed, watch, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { useAuth } from '../composables/useAuth';
import { useOAuth } from '../composables/useOAuth';
import { AuthErrorCode } from '../../domain/errors';

const props = defineProps<{
  mode: 'login' | 'register';
}>();

const emit = defineEmits<{
  'switch-to-register': [];
  'switch-to-login': [];
  'forgot-password': [email?: string];
  'start-password-setup': [email: string];
}>();

const { t } = useI18n();
const auth = useAuth();
const oauth = useOAuth();

// В dev можно подставлять из env, но не хардкодим в репе (хуки правильно на это ругаются)
const email = ref(import.meta.env.DEV ? (import.meta.env.VITE_DEV_EMAIL ?? '') : '');
const password = ref(import.meta.env.DEV ? (import.meta.env.VITE_DEV_PASSWORD ?? '') : '');
const confirmPassword = ref('');
const showPassword = ref(false);
const formValid = ref(false);
const shouldFocusPasswordOnLogin = ref(false);
const passwordField = ref<{
  focus?: () => void;
  $el?: HTMLElement;
} | null>(null);

// Раздельные флаги загрузки для email и Google кнопок
const isEmailLoading = ref(false);
const isGoogleLoading = ref(false);

const emailRules = computed(() => [
  (v: string) => !!v || t('auth.rules.emailRequired'),
  (v: string) => /.+@.+\..+/.test(v) || t('auth.rules.emailInvalid'),
]);

const passwordRules = computed(() => [
  (v: string) => !!v || t('auth.rules.passwordRequired'),
  (v: string) => v.length >= 12 || t('auth.rules.passwordMinLength'),
]);

const confirmPasswordRules = computed(() => [
  (v: string) => !!v || t('auth.rules.confirmPasswordRequired'),
  (v: string) => v === password.value || t('auth.rules.passwordsMismatch'),
]);

const isRegister = computed(() => props.mode === 'register');
const showExistingAccountActions = computed(
  () => isRegister.value && auth.errorCode.value === AuthErrorCode.AccountAlreadyExists
);

function focusPasswordInput() {
  nextTick(() => {
    const field = passwordField.value;
    if (field?.focus) {
      field.focus();
      return;
    }

    const input = field?.$el?.querySelector('input');
    if (input instanceof HTMLInputElement) {
      input.focus();
      input.select();
    }
  });
}

function switchToLoginAndFocusPassword() {
  shouldFocusPasswordOnLogin.value = true;
  emit('switch-to-login');
}

watch(
  () => props.mode,
  (mode, previousMode) => {
    if (previousMode === 'register' && mode === 'login' && shouldFocusPasswordOnLogin.value) {
      shouldFocusPasswordOnLogin.value = false;
      focusPasswordInput();
    }
  }
);

async function submit() {
  if (!formValid.value) return;

  isEmailLoading.value = true;
  try {
    if (isRegister.value) {
      const result = await auth.register(email.value, password.value);
      if (result?.nextStep === 'password_setup') {
        emit('start-password-setup', email.value);
      }
    } else {
      await auth.login(email.value, password.value);
    }
  } finally {
    isEmailLoading.value = false;
  }
}

async function loginWithGoogle() {
  isGoogleLoading.value = true;
  try {
    await oauth.startGoogleOAuth();
  } finally {
    isGoogleLoading.value = false;
  }
}
</script>

<template>
  <v-form v-model="formValid" @submit.prevent="submit">
    <v-alert
      v-if="auth.error.value"
      type="error"
      variant="tonal"
      closable
      class="mb-4"
      @click:close="auth.clearError"
    >
      {{ auth.error.value }}
    </v-alert>

    <div v-if="showExistingAccountActions" class="d-flex flex-column ga-2 mb-4">
      <v-btn variant="outlined" color="primary" @click="switchToLoginAndFocusPassword">
        {{ t('auth.login') }}
      </v-btn>
      <v-btn variant="text" color="primary" @click="emit('forgot-password', email)">
        {{ t('auth.forgotPassword') }}
      </v-btn>
    </div>

    <v-text-field
      v-model="email"
      :label="t('auth.email')"
      type="email"
      prepend-inner-icon="mdi-email-outline"
      :rules="emailRules"
      :disabled="isEmailLoading || isGoogleLoading"
      autocomplete="email"
      autofocus
      class="mb-2"
    />

    <v-text-field
      ref="passwordField"
      v-model="password"
      :label="t('auth.password')"
      :type="showPassword ? 'text' : 'password'"
      prepend-inner-icon="mdi-lock-outline"
      :append-inner-icon="showPassword ? 'mdi-eye-off' : 'mdi-eye'"
      :rules="passwordRules"
      :disabled="isEmailLoading || isGoogleLoading"
      autocomplete="current-password"
      class="mb-2"
      @click:append-inner="showPassword = !showPassword"
    />

    <v-text-field
      v-if="isRegister"
      v-model="confirmPassword"
      :label="t('auth.confirmPassword')"
      :type="showPassword ? 'text' : 'password'"
      prepend-inner-icon="mdi-lock-check-outline"
      :rules="confirmPasswordRules"
      :disabled="isEmailLoading || isGoogleLoading"
      autocomplete="new-password"
      class="mb-2"
    />

    <div v-if="!isRegister" class="text-right mb-4">
      <v-btn
        variant="text"
        size="small"
        color="primary"
        @click="emit('forgot-password', email)"
      >
        {{ t('auth.forgotPassword') }}
      </v-btn>
    </div>

    <v-btn
      type="submit"
      color="primary"
      size="large"
      block
      :loading="isEmailLoading"
      :disabled="!formValid || isGoogleLoading"
      class="mb-4"
    >
      {{ isRegister ? t('auth.register') : t('auth.login') }}
    </v-btn>

    <div class="d-flex align-center my-4">
      <v-divider />
      <span class="mx-4 text-medium-emphasis text-body-2">{{ t('auth.or') }}</span>
      <v-divider />
    </div>

    <v-btn
      variant="outlined"
      size="large"
      block
      :loading="isGoogleLoading"
      :disabled="isEmailLoading"
      class="mb-4"
      @click="loginWithGoogle"
    >
      <v-icon start>mdi-google</v-icon>
      {{ t('auth.loginWithGoogle') }}
    </v-btn>

    <div class="text-center">
      <span class="text-medium-emphasis">
        {{ isRegister ? t('auth.hasAccount') : t('auth.noAccount') }}
      </span>
      <v-btn
        variant="text"
        color="primary"
        @click="isRegister ? emit('switch-to-login') : emit('switch-to-register')"
      >
        {{ isRegister ? t('auth.login') : t('auth.create') }}
      </v-btn>
    </div>
  </v-form>
</template>
