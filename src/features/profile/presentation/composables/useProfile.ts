import { ref, computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useAuth } from '@/features/auth/presentation/composables/useAuth';
import { useAuthStore } from '@/features/auth/store/authStore';
import { api } from '@/features/auth/infrastructure/api/apiClient';
import type { LicenseInfo, GiftRedeemResult, LinkedProvider, ProfileSection } from '../../domain/types';

export function useProfile() {
  const { t } = useI18n();
  const auth = useAuth();
  const authStore = useAuthStore();

  const isLoggingOut = ref(false);
  const license = ref<LicenseInfo | null>(null);
  const licenseLoading = ref(false);
  const linkedProviders = ref<LinkedProvider[]>([]);
  const bonusSecondsBalance = ref(0);

  const licenseKeyInput = ref('');
  const isClaiming = ref(false);
  const claimError = ref<string | null>(null);

  const activeSection = ref<ProfileSection>('none');

  const giftCodeInput = ref('');
  const isRedeemingGift = ref(false);
  const giftError = ref<string | null>(null);
  const giftSuccessMessage = ref<string | null>(null);

  const userEmail = computed(() => {
    if (authStore.userEmail) return authStore.userEmail;
    if (authStore.session?.user?.email) return authStore.session.user.email;
    return '—';
  });

  const planLabel = computed(() => {
    if (!license.value) return null;
    const key = `profile.plans.${license.value.plan}`;
    const translated = t(key);
    return translated === key ? license.value.plan : translated;
  });

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

  const usageInfo = computed(() => {
    if (!license.value) return null;
    const totalSeconds = license.value.seconds_limit + bonusSecondsBalance.value;
    const totalMin = Math.round(totalSeconds / 60);
    const usedMin = Math.round(license.value.seconds_used / 60);
    const remainMin = Math.max(0, totalMin - usedMin);
    const percent = totalMin > 0 ? Math.round((usedMin / totalMin) * 100) : 0;
    return { used: usedMin, total: totalMin, remaining: remainMin, percent };
  });

  function providerIcon(provider: string): string {
    const icons: Record<string, string> = { google: 'mdi-google' };
    return icons[provider] ?? 'mdi-link-variant';
  }

  function providerLabel(provider: string): string {
    const key = `profile.providers.${provider}`;
    const translated = t(key);
    return translated === key ? provider : translated;
  }

  async function fetchLicense() {
    licenseLoading.value = true;
    try {
      const data = await api.get<{ licenses: LicenseInfo[] }>('/api/v1/account/licenses');
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

  async function fetchBonus() {
    try {
      const data = await api.get<{ bonus_seconds_balance: number }>('/api/v1/account/bonus');
      bonusSecondsBalance.value = Math.max(0, data.bonus_seconds_balance ?? 0);
    } catch (err) {
      console.error('Не удалось загрузить бонусный баланс:', err);
      bonusSecondsBalance.value = 0;
    }
  }

  function toggleSection(section: 'license' | 'gift') {
    if (activeSection.value === section) {
      activeSection.value = 'none';
    } else {
      activeSection.value = section;
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
      bonusSecondsBalance.value = Math.max(0, data.bonus_seconds_balance ?? 0);
      giftCodeInput.value = '';
      await fetchLicense();
    } catch (err: any) {
      const msg = String(err?.message || '');
      giftError.value = msg.trim() ? msg : t('profile.gift.errors.generic');
    } finally {
      isRedeemingGift.value = false;
    }
  }

  async function handleLogout() {
    isLoggingOut.value = true;
    try {
      await auth.logout();
    } finally {
      isLoggingOut.value = false;
    }
  }

  // Загрузка всех данных и установка начальной секции
  async function fetchProfile(initialSection: ProfileSection = 'none') {
    await Promise.all([
      fetchLicense(),
      fetchBonus(),
      fetchLinkedProviders(),
    ]);

    if (initialSection !== 'none') {
      activeSection.value = initialSection;
      // Фокус обрабатывается самими section-компонентами через watch на expanded
    }
  }

  return {
    // State
    isLoggingOut,
    license,
    licenseLoading,
    linkedProviders,
    licenseKeyInput,
    isClaiming,
    claimError,
    activeSection,
    giftCodeInput,
    isRedeemingGift,
    giftError,
    giftSuccessMessage,

    // Computed
    userEmail,
    planLabel,
    statusColor,
    statusLabel,
    usageInfo,

    // Methods
    providerIcon,
    providerLabel,
    fetchLicense,
    fetchLinkedProviders,
    fetchProfile,
    toggleSection,
    claimLicense,
    redeemGift,
    handleLogout,
  };
}
