<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { useProfile } from '../composables/useProfile';
import type { ProfileSection, ProfileWindowOpenedPayload } from '../../domain/types';
import ProfileInfoSection from './sections/ProfileInfoSection.vue';
import LicenseClaimSection from './sections/LicenseClaimSection.vue';
import GiftRedeemSection from './sections/GiftRedeemSection.vue';
import LinkedAccountsSection from './sections/LinkedAccountsSection.vue';

const { t } = useI18n();
const profile = useProfile();

let unlistenOpened: UnlistenFn | null = null;

function handleClose() {
  try {
    invoke('show_recording_window');
  } catch {}
}

function onKeyDown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    handleClose();
  }
}

onMounted(async () => {
  document.addEventListener('keydown', onKeyDown);

  unlistenOpened = await listen<ProfileWindowOpenedPayload>('profile-window-opened', async (event) => {
    const section = (event.payload?.initialSection ?? 'none') as ProfileSection;
    // Сбрасываем состояние и подтягиваем свежие данные
    profile.activeSection.value = 'none';
    profile.claimError.value = null;
    profile.giftError.value = null;
    profile.giftSuccessMessage.value = null;
    profile.licenseKeyInput.value = '';
    profile.giftCodeInput.value = '';
    await profile.fetchProfile(section);
  });

  // Первоначальная загрузка данных
  await profile.fetchProfile();
});

onUnmounted(() => {
  document.removeEventListener('keydown', onKeyDown);
  if (unlistenOpened) {
    unlistenOpened();
  }
});
</script>

<template>
  <div class="profile-window">
    <!-- Кастомный тайтлбар с drag region -->
    <div class="profile-header" data-tauri-drag-region>
      <div class="profile-title">
        <v-icon class="mr-2" size="20">mdi-account-circle</v-icon>
        {{ t('profile.title') }}
      </div>
      <v-btn
        class="no-drag"
        icon="mdi-close"
        variant="text"
        size="small"
        @click="handleClose"
      />
    </div>

    <!-- Скроллируемый контент -->
    <div class="profile-body">
      <ProfileInfoSection
        :user-email="profile.userEmail.value"
        :license-loading="profile.licenseLoading.value"
        :license="profile.license.value"
        :plan-label="profile.planLabel.value"
        :status-color="profile.statusColor.value"
        :status-label="profile.statusLabel.value"
        :usage-info="profile.usageInfo.value"
      />

      <v-divider class="my-1" />

      <div class="d-flex flex-column align-center ga-2 px-4 py-2">
        <LicenseClaimSection
          :expanded="profile.activeSection.value === 'license'"
          :license-key-input="profile.licenseKeyInput.value"
          :is-claiming="profile.isClaiming.value"
          :claim-error="profile.claimError.value"
          @toggle="profile.toggleSection('license')"
          @update:license-key-input="profile.licenseKeyInput.value = $event"
          @claim="profile.claimLicense()"
        />
        <GiftRedeemSection
          :expanded="profile.activeSection.value === 'gift'"
          :gift-code-input="profile.giftCodeInput.value"
          :is-redeeming-gift="profile.isRedeemingGift.value"
          :gift-error="profile.giftError.value"
          :gift-success-message="profile.giftSuccessMessage.value"
          @toggle="profile.toggleSection('gift')"
          @update:gift-code-input="profile.giftCodeInput.value = $event"
          @redeem="profile.redeemGift()"
        />
      </div>

      <LinkedAccountsSection
        :linked-providers="profile.linkedProviders.value"
        :provider-icon="profile.providerIcon"
        :provider-label="profile.providerLabel"
      />
    </div>

    <!-- Кнопка логаута -->
    <div class="profile-footer">
      <v-btn
        color="error"
        variant="tonal"
        block
        :loading="profile.isLoggingOut.value"
        @click="profile.handleLogout()"
      >
        <v-icon start>mdi-logout</v-icon>
        {{ profile.isLoggingOut.value ? t('profile.loggingOut') : t('profile.logout') }}
      </v-btn>
    </div>
  </div>
</template>

<style scoped>
.profile-window {
  width: 100%;
  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--glass-bg);
  border: 1px solid var(--glass-border);
  border-radius: var(--radius-xl);
  overflow: hidden;
}

:global(.theme-light) .profile-window {
  background: rgba(255, 255, 255, 0.98);
}

.profile-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--spacing-md);
  border-bottom: 1px solid var(--glass-border);
}

.profile-title {
  display: flex;
  align-items: center;
  font-size: 16px;
  font-weight: 600;
}

.profile-body {
  flex: 1;
  overflow-y: scroll;
  padding: var(--spacing-sm);
}

.profile-body::-webkit-scrollbar {
  width: 6px;
}

.profile-body::-webkit-scrollbar-track {
  background: transparent;
}

.profile-body::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.2);
  border-radius: 3px;
}

.profile-body::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.3);
}

.profile-footer {
  padding: var(--spacing-md);
  border-top: 1px solid var(--glass-border);
}
</style>
