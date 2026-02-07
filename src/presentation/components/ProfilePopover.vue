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

interface GiftRedeemResult {
  redeemed: boolean;
  seconds_added: number;
  bonus_seconds_balance: number;
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

const licenseKeyInput = ref('');
const isClaiming = ref(false);
const claimError = ref<string | null>(null);

// Аккордеон: какая секция сейчас раскрыта
const activeSection = ref<'none' | 'license' | 'gift'>('none');

// Стейт для активации подарка
const giftCodeInput = ref('');
const isRedeemingGift = ref(false);
const giftError = ref<string | null>(null);
const giftSuccessMessage = ref<string | null>(null);

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
  const percent = totalMin > 0 ? Math.round((usedMin / totalMin) * 100) : 0;
  return { used: usedMin, total: totalMin, remaining: remainMin, percent };
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

function toggleSection(section: 'license' | 'gift') {
  if (activeSection.value === section) {
    activeSection.value = 'none';
  } else {
    activeSection.value = section;
    // Сбрасываем ошибки при переключении
    claimError.value = null;
    giftError.value = null;
    giftSuccessMessage.value = null;
  }
}

async function claimLicense() {
  const key = licenseKeyInput.value.trim();
  if (!key) {
    claimError.value = t('profile.claim.errors.empty');
    return;
  }

  isClaiming.value = true;
  claimError.value = null;
  try {
    await api.post('/api/v1/account/licenses/claim', { license_key: key });
    licenseKeyInput.value = '';
    activeSection.value = 'none';
    await fetchLicense();
  } catch (err: any) {
    const msg = String(err?.message || '');
    claimError.value = msg.trim() ? msg : t('profile.claim.errors.generic');
  } finally {
    isClaiming.value = false;
  }
}

async function redeemGift() {
  const code = giftCodeInput.value.trim();
  if (!code) {
    giftError.value = t('profile.gift.errors.empty');
    return;
  }

  isRedeemingGift.value = true;
  giftError.value = null;
  giftSuccessMessage.value = null;
  try {
    const data = await api.post<GiftRedeemResult>('/api/v1/gifts/redeem', { code });
    const minutes = Math.round(data.seconds_added / 60);
    giftSuccessMessage.value = t('profile.gift.success', { minutes });
    giftCodeInput.value = '';
    await fetchLicense();
  } catch (err: any) {
    const msg = String(err?.message || '');
    giftError.value = msg.trim() ? msg : t('profile.gift.errors.generic');
  } finally {
    isRedeemingGift.value = false;
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
    :scrim="false"
    @update:model-value="emit('close')"
  >
    <v-card class="profile-card">
      <v-card-title class="d-flex align-center">        
        <v-spacer />
        <v-btn
          icon="mdi-close"
          variant="text"
          size="small"
          @click="emit('close')"
        />
      </v-card-title>

      <v-card-text class="pt-0">
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

          <v-list-item v-else>
            <template #prepend>
              <v-icon>mdi-card-account-details-outline</v-icon>
            </template>
            <v-list-item-title class="text-body-2 text-medium-emphasis">
              {{ t('profile.plan') }}
            </v-list-item-title>
            <v-list-item-subtitle class="text-body-1">
              {{ t('profile.noPlan') }}
            </v-list-item-subtitle>
          </v-list-item>

          <v-divider class="my-1" />

          <div class="d-flex flex-column align-center ga-2 px-4 py-2">
            <v-btn
              variant="tonal"
              size="small"
              :color="activeSection === 'license' ? 'primary' : undefined"
              prepend-icon="mdi-key-variant"
              @click="toggleSection('license')"
            >
              {{ t('profile.claim.title') }}
            </v-btn>
            <v-btn
              variant="tonal"
              size="small"
              :color="activeSection === 'gift' ? 'primary' : undefined"
              prepend-icon="mdi-gift-outline"
              @click="toggleSection('gift')"
            >
              {{ t('profile.gift.title') }}
            </v-btn>
          </div>

          <v-expand-transition>
            <div v-show="activeSection === 'license'" class="px-4 pb-2">
              <div class="text-body-2 text-medium-emphasis mb-2">
                {{ t('profile.claim.hint') }}
              </div>
              <v-text-field
                v-model="licenseKeyInput"
                :label="t('profile.claim.inputLabel')"
                density="comfortable"
                variant="outlined"
                hide-details
                autocomplete="off"
              />
              <div v-if="claimError" class="text-caption text-error mt-2">
                {{ claimError }}
              </div>
              <v-btn
                class="mt-3"
                color="primary"
                block
                :loading="isClaiming"
                :disabled="isClaiming"
                @click="claimLicense"
              >
                {{ t('profile.claim.cta') }}
              </v-btn>
            </div>
          </v-expand-transition>

          <v-expand-transition>
            <div v-show="activeSection === 'gift'" class="px-4 pb-2">
              <div class="text-body-2 text-medium-emphasis mb-2">
                {{ t('profile.gift.hint') }}
              </div>
              <v-text-field
                v-model="giftCodeInput"
                :label="t('profile.gift.inputLabel')"
                density="comfortable"
                variant="outlined"
                hide-details
                autocomplete="off"
              />
              <div v-if="giftError" class="text-caption text-error mt-2">
                {{ giftError }}
              </div>
              <div v-if="giftSuccessMessage" class="text-caption text-success mt-2">
                {{ giftSuccessMessage }}
              </div>
              <v-btn
                class="mt-3"
                color="primary"
                block
                :loading="isRedeemingGift"
                :disabled="isRedeemingGift"
                @click="redeemGift"
              >
                {{ t('profile.gift.cta') }}
              </v-btn>
            </div>
          </v-expand-transition>

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
            <v-progress-linear
              class="mt-2 rounded"
              :model-value="usageInfo.percent"
              :color="usageInfo.percent >= 90 ? 'error' : usageInfo.percent >= 70 ? 'warning' : 'primary'"
              height="6"
              rounded
            />
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

<style scoped>
.profile-card {
  min-height: calc(100vh - 24px);
  max-height: calc(100vh - 24px);
  overflow-y: scroll;
}

.profile-card::-webkit-scrollbar {
  width: 6px;
}

.profile-card::-webkit-scrollbar-track {
  background: transparent;
}

.profile-card::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.2);
  border-radius: 3px;
}

.profile-card::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.3);
}
</style>
