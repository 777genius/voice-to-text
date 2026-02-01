<script setup lang="ts">
import { ref, computed, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { usePasswordReset } from '../composables/usePasswordReset';

const emit = defineEmits<{
  back: [];
}>();

const { t } = useI18n();
const passwordReset = usePasswordReset();

const email = ref('');
const code = ref('');
const newPassword = ref('');
const confirmPassword = ref('');
const showPassword = ref(false);
const formValid = ref(false);

const emailRules = computed(() => [
  (v: string) => !!v || t('auth.rules.emailRequired'),
  (v: string) => /.+@.+\..+/.test(v) || t('auth.rules.emailInvalid'),
]);

const codeRules = computed(() => [
  (v: string) => !!v || t('auth.rules.codeRequired'),
  (v: string) => /^\d{6}$/.test(v) || t('auth.rules.codeFormat'),
]);

const passwordRules = computed(() => [
  (v: string) => !!v || t('auth.rules.passwordRequired'),
  (v: string) => v.length >= 12 || t('auth.rules.passwordMinLength'),
]);

const confirmPasswordRules = computed(() => [
  (v: string) => !!v || t('auth.rules.confirmPasswordRequired'),
  (v: string) => v === newPassword.value || t('auth.rules.passwordsMismatch'),
]);

async function submitEmail() {
  await passwordReset.startReset(email.value);
}

function submitCode() {
  passwordReset.submitCode();
}

async function submitPassword() {
  await passwordReset.confirmReset(code.value, newPassword.value);
}

function goBack() {
  if (passwordReset.step.value === 'email') {
    passwordReset.reset();
    emit('back');
  } else {
    passwordReset.goBack();
  }
}

onUnmounted(() => {
  passwordReset.reset();
});
</script>

<template>
  <div>
    <v-alert
      v-if="passwordReset.error.value"
      type="error"
      variant="tonal"
      closable
      class="mb-4"
      @click:close="passwordReset.clearError"
    >
      {{ passwordReset.error.value }}
    </v-alert>

    <v-form v-if="passwordReset.step.value === 'email'" v-model="formValid" @submit.prevent="submitEmail">
      <p class="text-body-2 text-medium-emphasis mb-4">
        {{ t('auth.resetPasswordDesc') }}
      </p>

      <v-text-field
        v-model="email"
        :label="t('auth.email')"
        type="email"
        prepend-inner-icon="mdi-email-outline"
        :rules="emailRules"
        :disabled="passwordReset.isLoading.value"
        autofocus
        class="mb-4"
      />

      <v-btn
        type="submit"
        color="primary"
        size="large"
        block
        :loading="passwordReset.isLoading.value"
        :disabled="!formValid"
        class="mb-4"
      >
        {{ t('auth.sendCode') }}
      </v-btn>
    </v-form>

    <v-form v-else-if="passwordReset.step.value === 'code'" v-model="formValid" @submit.prevent="submitCode">
      <v-alert v-if="passwordReset.resetEmail.value" type="info" variant="tonal" class="mb-4">
        {{ t('auth.codeSentTo') }} <strong>{{ passwordReset.resetEmail.value }}</strong>
      </v-alert>
      <v-alert v-else type="warning" variant="tonal" class="mb-4">
        {{ t('auth.errors.emailNotSet') }}
      </v-alert>

      <v-text-field
        v-model="code"
        :label="t('auth.verificationCode')"
        placeholder="000000"
        maxlength="6"
        :rules="codeRules"
        :disabled="passwordReset.isLoading.value"
        autofocus
        class="mb-4"
        autocomplete="one-time-code"
      />

      <v-btn
        type="submit"
        color="primary"
        size="large"
        block
        :disabled="!formValid || passwordReset.isLoading.value"
        :loading="passwordReset.isLoading.value"
        class="mb-4"
      >
        {{ t('auth.verifyCode') }}
      </v-btn>
    </v-form>

    <v-form v-else v-model="formValid" @submit.prevent="submitPassword">
      <v-text-field
        v-model="newPassword"
        :label="t('auth.newPassword')"
        :type="showPassword ? 'text' : 'password'"
        prepend-inner-icon="mdi-lock-outline"
        :append-inner-icon="showPassword ? 'mdi-eye-off' : 'mdi-eye'"
        :rules="passwordRules"
        :disabled="passwordReset.isLoading.value"
        autofocus
        class="mb-2"
        @click:append-inner="showPassword = !showPassword"
      />

      <v-text-field
        v-model="confirmPassword"
        :label="t('auth.confirmPassword')"
        :type="showPassword ? 'text' : 'password'"
        prepend-inner-icon="mdi-lock-check-outline"
        :rules="confirmPasswordRules"
        :disabled="passwordReset.isLoading.value"
        class="mb-4"
      />

      <v-btn
        type="submit"
        color="primary"
        size="large"
        block
        :loading="passwordReset.isLoading.value"
        :disabled="!formValid"
        class="mb-4"
      >
        {{ t('auth.savePassword') }}
      </v-btn>
    </v-form>

    <v-btn
      variant="text"
      @click="goBack"
    >
      <v-icon start>mdi-arrow-left</v-icon>
      {{ t('auth.back') }}
    </v-btn>
  </div>
</template>
