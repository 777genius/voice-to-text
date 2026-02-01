<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { useAuth } from '../../features/auth/presentation/composables/useAuth';
import { useAuthStore } from '../../features/auth/store/authStore';
import { api } from '../../features/auth/infrastructure/api/apiClient';

interface LicenseInfo {
  license_id: string;
  role: string;
  plan: string;
  status: string;
  seconds_limit: number;
  seconds_used: number;
  period_start: string;
  period_end: string;
  claimed_at: string;
}

interface LinkedProvider {
  provider: string;
  provider_email: string | null;
  provider_name: string | null;
  linked_at: string;
}

const emit = defineEmits<{
  close: []
}>();

const { t } = useI18n();
const auth = useAuth();
const authStore = useAuthStore();

const isLoggingOut = ref(false);
const license = ref<LicenseInfo | null>(null);
const licenseLoading = ref(false);
const linkedProviders = ref<LinkedProvider[]>([]);

// Пробуем получить email из разных источников
const userEmail = computed(() => {
  if (authStore.userEmail) {
    return authStore.userEmail;
  }
  if (authStore.session?.user?.email) {
    return authStore.session.user.email;
  }
  return '—';
});

// Название плана для отображения
const planLabel = computed(() => {
  if (!license.value) return null;
  const key = `profile.plans.${license.value.plan}`;
  const translated = t(key);
  // Если ключ не найден, vue-i18n вернёт сам ключ
  return translated === key ? license.value.plan : translated;
});

// Статус лицензии
const statusColor = computed(() => {
  if (!license.value) return 'grey';
  return license.value.status === 'active' ? 'success' : 'warning';
});

const statusLabel = computed(() => {
  if (!license.value) return '';
  const key = `profile.statuses.${license.value.status}`;
  const translated = t(key);
  return translated === key ? license.value.status : translated;
});

// Остаток минут
const usageInfo = computed(() => {
  if (!license.value) return null;
  const totalMin = Math.round(license.value.seconds_limit / 60);
  const usedMin = Math.round(license.value.seconds_used / 60);
  const remainMin = Math.max(0, totalMin - usedMin);
  return { used: usedMin, total: totalMin, remaining: remainMin };
});

// Иконка провайдера
function providerIcon(provider: string): string {
  const icons: Record<string, string> = {
    google: 'mdi-google',
  };
  return icons[provider] ?? 'mdi-link-variant';
}

// Название провайдера через i18n
function providerLabel(provider: string): string {
  const key = `profile.providers.${provider}`;
  const translated = t(key);
  return translated === key ? provider : translated;
}

async function fetchLicense() {
  licenseLoading.value = true;
  try {
    const data = await api.get<{ licenses: LicenseInfo[] }>('/api/v1/account/licenses');
    // Берём первую активную или просто первую
    license.value = data.licenses.find(l => l.status === 'active') ?? data.licenses[0] ?? null;
  } catch (err) {
    console.error('Не удалось загрузить лицензию:', err);
  } finally {
    licenseLoading.value = false;
  }
}

async function fetchLinkedProviders() {
  try {
    const data = await api.get<{ linked_providers: LinkedProvider[] }>('/api/v1/auth/me');
    linkedProviders.value = data.linked_providers ?? [];
  } catch (err) {
    console.error('Не удалось загрузить привязанные аккаунты:', err);
  }
}

async function handleLogout() {
  isLoggingOut.value = true;
  try {
    await auth.logout();
    emit('close');
  } finally {
    isLoggingOut.value = false;
  }
}

onMounted(() => {
  fetchLicense();
  fetchLinkedProviders();
});
</script>

<template>
  <v-dialog
    :model-value="true"
    max-width="360"
    @update:model-value="emit('close')"
  >
    <v-card>
      <v-card-title class="d-flex align-center">
        <v-icon class="mr-2">mdi-account-circle</v-icon>
        {{ t('profile.title') }}
        <v-spacer />
        <v-btn
          icon="mdi-close"
          variant="text"
          size="small"
          @click="emit('close')"
        />
      </v-card-title>

      <v-card-text>
        <v-list>
          <v-list-item>
            <template #prepend>
              <v-icon>mdi-email-outline</v-icon>
            </template>
            <v-list-item-title class="text-body-2 text-medium-emphasis">
              {{ t('profile.email') }}
            </v-list-item-title>
            <v-list-item-subtitle class="text-body-1">
              {{ userEmail }}
            </v-list-item-subtitle>
          </v-list-item>

          <v-list-item v-if="licenseLoading">
            <template #prepend>
              <v-progress-circular size="20" width="2" indeterminate />
            </template>
            <v-list-item-title class="text-body-2 text-medium-emphasis">
              {{ t('profile.plan') }}
            </v-list-item-title>
          </v-list-item>

          <v-list-item v-else-if="license">
            <template #prepend>
              <v-icon>mdi-card-account-details-outline</v-icon>
            </template>
            <v-list-item-title class="text-body-2 text-medium-emphasis">
              {{ t('profile.plan') }}
            </v-list-item-title>
            <v-list-item-subtitle class="text-body-1 d-flex align-center ga-2">
              {{ planLabel }}
              <v-chip :color="statusColor" size="x-small" variant="tonal">
                {{ statusLabel }}
              </v-chip>
            </v-list-item-subtitle>
          </v-list-item>

          <v-list-item v-if="license && usageInfo">
            <template #prepend>
              <v-icon>mdi-clock-outline</v-icon>
            </template>
            <v-list-item-title class="text-body-2 text-medium-emphasis">
              {{ t('profile.usage') }}
            </v-list-item-title>
            <v-list-item-subtitle class="text-body-1">
              {{ t('profile.usageDetail', { used: usageInfo.used, total: usageInfo.total }) }}
            </v-list-item-subtitle>
          </v-list-item>

          <template v-if="linkedProviders.length > 0">
            <v-divider class="my-1" />
            <v-list-subheader>{{ t('profile.linkedAccounts') }}</v-list-subheader>
            <v-list-item
              v-for="lp in linkedProviders"
              :key="lp.provider"
            >
              <template #prepend>
                <v-icon>{{ providerIcon(lp.provider) }}</v-icon>
              </template>
              <v-list-item-title class="text-body-2 d-flex align-center ga-2">
                {{ providerLabel(lp.provider) }}
                <v-chip color="success" size="x-small" variant="tonal">
                  {{ t('profile.linked') }}
                </v-chip>
              </v-list-item-title>
              <v-list-item-subtitle v-if="lp.provider_email || lp.provider_name" class="text-body-2">
                {{ lp.provider_email ?? lp.provider_name }}
              </v-list-item-subtitle>
            </v-list-item>
          </template>
        </v-list>
      </v-card-text>

      <v-card-actions>
        <v-btn
          color="error"
          variant="tonal"
          block
          :loading="isLoggingOut"
          @click="handleLogout"
        >
          <v-icon start>mdi-logout</v-icon>
          {{ isLoggingOut ? t('profile.loggingOut') : t('profile.logout') }}
        </v-btn>
      </v-card-actions>
    </v-card>
  </v-dialog>
</template>
