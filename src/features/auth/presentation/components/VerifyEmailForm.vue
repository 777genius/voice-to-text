<script setup lang="ts">
import { ref, computed, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { useEmailVerification } from '../composables/useEmailVerification';

const emit = defineEmits<{
  back: [];
}>();

const { t } = useI18n();
const verification = useEmailVerification();

const code = ref('');
const formValid = ref(false);
const resendCooldown = ref(0);
const resendLoading = ref(false);
let resendTimer: ReturnType<typeof setInterval> | null = null;

const codeRules = computed(() => [
  (v: string) => !!v || t('auth.rules.codeRequired'),
  (v: string) => /^\d{6}$/.test(v) || t('auth.rules.codeFormat'),
]);

function onPaste(e: ClipboardEvent) {
  e.preventDefault();
  const raw = e.clipboardData?.getData('text') || '';
  const digits = raw.replace(/\D/g, '').slice(0, 6);
  code.value = digits;
}

async function submit() {
  if (!formValid.value) return;
  await verification.verify(code.value);
}

function startCooldown(seconds: number) {
  resendCooldown.value = seconds;
  resendTimer = setInterval(() => {
    resendCooldown.value--;
    if (resendCooldown.value <= 0 && resendTimer) {
      clearInterval(resendTimer);
      resendTimer = null;
    }
  }, 1000);
}

async function resend() {
  resendLoading.value = true;
  try {
    await verification.resend();
    startCooldown(60);
  } catch {
    // Ошибка отображается через verification.error
  } finally {
    resendLoading.value = false;
  }
}

function goBack() {
  if (resendTimer) clearInterval(resendTimer);
  verification.goBack();
  emit('back');
}

onUnmounted(() => {
  if (resendTimer) clearInterval(resendTimer);
});
</script>

<template>
  <v-form v-model="formValid" @submit.prevent="submit">
    <v-alert
      v-if="verification.pendingEmail.value"
      type="info"
      variant="tonal"
      class="mb-4"
    >
      {{ t('auth.codeSentTo') }}
      <strong>{{ verification.pendingEmail.value }}</strong>
    </v-alert>
    <v-alert
      v-else
      type="warning"
      variant="tonal"
      class="mb-4"
    >
      {{ t('auth.errors.emailNotSet') }}
    </v-alert>

    <v-alert
      v-if="verification.error.value"
      type="error"
      variant="tonal"
      closable
      class="mb-4"
      @click:close="verification.clearError"
    >
      {{ verification.error.value }}
    </v-alert>

    <v-text-field
      v-model="code"
      :label="t('auth.verificationCode')"
      placeholder="000000"
      maxlength="6"
      :rules="codeRules"
      :disabled="verification.isLoading.value"
      autofocus
      class="mb-4"
      autocomplete="one-time-code"
      @paste="onPaste"
    />

    <v-btn
      type="submit"
      color="primary"
      size="large"
      block
      :loading="verification.isLoading.value"
      :disabled="!formValid"
      class="mb-4"
    >
      {{ t('auth.verify') }}
    </v-btn>

    <div class="d-flex justify-space-between">
      <v-btn
        variant="text"
        @click="goBack"
      >
        <v-icon start>mdi-arrow-left</v-icon>
        {{ t('auth.back') }}
      </v-btn>

      <v-btn
        variant="text"
        color="primary"
        :disabled="resendCooldown > 0 || resendLoading"
        :loading="resendLoading"
        @click="resend"
      >
        {{ resendCooldown > 0 ? t('auth.resendIn', { seconds: resendCooldown }) : t('auth.resend') }}
      </v-btn>
    </div>
  </v-form>
</template>
