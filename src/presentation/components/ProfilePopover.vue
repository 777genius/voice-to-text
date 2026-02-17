<script setup lang="ts">
import { onMounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { useProfile } from '@/features/profile';
import ProfileInfoSection from '@/features/profile/presentation/components/sections/ProfileInfoSection.vue';
import LicenseClaimSection from '@/features/profile/presentation/components/sections/LicenseClaimSection.vue';
import GiftRedeemSection from '@/features/profile/presentation/components/sections/GiftRedeemSection.vue';
import LinkedAccountsSection from '@/features/profile/presentation/components/sections/LinkedAccountsSection.vue';
import type { ProfileSection } from '@/features/profile';

const props = withDefaults(defineProps<{
  initialSection?: ProfileSection;
}>(), {
  initialSection: 'none',
});

const emit = defineEmits<{
  close: []
}>();

const { t } = useI18n();
const profile = useProfile();

function handleClose() {
  emit('close');
}

async function handleLogout() {
  await profile.handleLogout();
  emit('close');
}

onMounted(async () => {
  await profile.fetchProfile(props.initialSection);
});
</script>

<template>
  <v-dialog
    :model-value="true"
    max-width="360"
    :scrim="false"
    @update:model-value="handleClose"
  >
    <v-card class="profile-card">
      <v-card-title class="d-flex align-center">
        <v-icon class="mr-2">mdi-account-circle</v-icon>
        {{ t('profile.title') }}
        <v-spacer />
        <v-btn
          icon="mdi-close"
          variant="text"
          size="small"
          @click="handleClose"
        />
      </v-card-title>

      <v-card-text class="pt-0">
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
      </v-card-text>

      <v-card-actions>
        <v-btn
          color="error"
          variant="tonal"
          block
          :loading="profile.isLoggingOut.value"
          @click="handleLogout"
        >
          <v-icon start>mdi-logout</v-icon>
          {{ profile.isLoggingOut.value ? t('profile.loggingOut') : t('profile.logout') }}
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
