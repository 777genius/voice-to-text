<script setup lang="ts">
import { useDisplay } from 'vuetify';

const { t } = useI18n();
const { smAndDown } = useDisplay();
const menuOpen = ref(false);
const { trackNavClick } = useAnalytics();

const navItems = computed(() => [
  { id: 'features', label: t('nav.features') },
  { id: 'pricing', label: t('nav.pricing') },
  { id: 'download', label: t('nav.download') },
]);
</script>

<template>
  <v-app-bar flat class="app-header">
    <v-container class="d-flex align-center header-container ml-24">
      <AppLogo />
      <div class="nav-links ml-6" v-show="!smAndDown">
        <v-btn v-for="item in navItems" :key="item.id" variant="text" :href="`#${item.id}`" @click="trackNavClick(item.id)">
          {{ item.label }}
        </v-btn>
      </div>
      <v-spacer />
      <div class="desktop-actions" v-show="!smAndDown">
        <LanguageSwitcher compact />
        <ThemeToggle />
      </div>
      <div class="mobile-actions" v-show="smAndDown">
        <v-btn icon="mdi-menu" variant="text" @click="menuOpen = true" />
        <v-dialog v-model="menuOpen" fullscreen scrim>
          <v-card class="mobile-menu">
            <div class="mobile-menu__header">
              <AppLogo />
              <v-spacer />
              <v-btn icon="mdi-close" variant="text" @click="menuOpen = false" />
            </div>
            <v-divider />
            <v-list class="mobile-menu__list">
              <v-list-item
                v-for="item in navItems"
                :key="item.id"
                :title="item.label"
                :href="`#${item.id}`"
                @click="trackNavClick(item.id); menuOpen = false"
              />
            </v-list>
            <v-divider />
            <div class="mobile-menu__actions">
              <LanguageSwitcher icon-only />
              <ThemeToggle />
            </div>
          </v-card>
        </v-dialog>
      </div>
    </v-container>
  </v-app-bar>
</template>

<style scoped>
.nav-links {
  display: flex;
  align-self: stretch;
  align-items: stretch;
}

.nav-links :deep(.v-btn) {
  height: 100% !important;
  border-radius: 0;
}

.header-container {
  flex-wrap: nowrap;
}

.desktop-actions {
  display: flex;
  gap: 8px;
  align-items: center;
}

.mobile-actions :deep(.v-list-item) {
  min-height: 40px;
}

.mobile-menu {
  padding: 16px 16px 24px;
}

.mobile-menu__header {
  display: flex;
  align-items: center;
  gap: 12px;
  padding-bottom: 12px;
}

.mobile-menu__list {
  padding: 8px 0;
}

.mobile-menu__actions {
  display: flex;
  flex-direction: row;
  gap: 8px;
  align-items: center;
  justify-content: center;
  padding-top: 16px;
}
</style>
