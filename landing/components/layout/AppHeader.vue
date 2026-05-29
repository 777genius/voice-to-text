<script setup lang="ts">
import { mdiClose, mdiDownload, mdiMenu } from '@mdi/js';

const { t } = useI18n();
const menuOpen = ref(false);
const { trackNavClick } = useAnalytics();
const { isDark } = useBrowserTheme();

const desktopNavItems = computed(() => [
  { id: 'features', label: t('nav.features') },
  { id: 'pricing', label: t('nav.pricing') },
  { id: 'faq', label: t('nav.faq') },
]);

const mobileNavItems = computed(() => [
  ...desktopNavItems.value,
  { id: 'download', label: t('nav.download') },
]);
</script>

<template>
  <header class="app-header">
    <div class="app-header__inner">
      <div class="app-header__brand">
        <AppLogo />
      </div>

      <nav class="app-header__nav">
        <v-btn
          v-for="item in desktopNavItems"
          :key="item.id"
          variant="text"
          :href="`#${item.id}`"
          @click="trackNavClick(item.id)"
        >
          {{ item.label }}
        </v-btn>
      </nav>

      <div class="app-header__spacer" />

      <div class="app-header__desktop-actions">
        <v-btn
          class="app-header__download-btn"
          :prepend-icon="mdiDownload"
          href="#download"
          variant="flat"
          @click="trackNavClick('download')"
        >
          {{ t('nav.download') }}
        </v-btn>
        <div class="app-header__language">
          <LanguageSwitcher compact />
        </div>
        <div class="app-header__action-divider" />
        <div class="app-header__theme">
          <ThemeToggle />
        </div>
      </div>

      <div class="app-header__mobile-actions">
        <v-btn class="app-header__menu-btn" :icon="mdiMenu" variant="text" aria-label="Open menu" @click="menuOpen = true" />
        <Teleport to="body">
          <Transition name="mobile-menu-fade">
            <div
              v-if="menuOpen"
              class="mobile-menu-overlay"
              :class="isDark ? 'mobile-menu-overlay--dark' : 'mobile-menu-overlay--light'"
              @click.self="menuOpen = false"
            >
              <div class="mobile-menu">
                <div class="mobile-menu__header">
                  <div class="mobile-menu__brand">
                    <AppLogo />
                  </div>
                  <div style="flex: 1" />
                  <v-btn class="mobile-menu__close" :icon="mdiClose" variant="text" aria-label="Close menu" @click="menuOpen = false" />
                </div>
                <hr class="mobile-menu__divider">
                <nav class="mobile-menu__list">
                  <a
                    v-for="item in mobileNavItems"
                    :key="item.id"
                    :href="`#${item.id}`"
                    class="mobile-menu__link"
                    @click="trackNavClick(item.id); menuOpen = false"
                  >
                    {{ item.label }}
                  </a>
                </nav>
                <hr class="mobile-menu__divider">
                <div class="mobile-menu__actions">
                  <LanguageSwitcher compact />
                  <div class="mobile-menu__theme">
                    <ThemeToggle />
                  </div>
                </div>
              </div>
            </div>
          </Transition>
        </Teleport>
      </div>
    </div>
  </header>
</template>

<style scoped>
.app-header {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  z-index: 1000;
  height: 96px;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 10px 0;
  pointer-events: none;
}

.app-header__inner {
  position: relative;
  width: min(calc(100vw - clamp(48px, 7.4vw, 156px)), 1080px);
  min-height: 74px;
  display: flex;
  align-items: center;
  gap: clamp(10px, 1.25vw, 24px);
  padding: 8px clamp(14px, 1.2vw, 22px) 8px clamp(12px, 1vw, 18px);
  pointer-events: auto;
  overflow: hidden;
  border: 1px solid rgba(165, 180, 252, 0.34);
  border-radius: 28px;
  background:
    linear-gradient(135deg, rgba(10, 17, 34, 0.9), rgba(8, 12, 24, 0.78)),
    radial-gradient(circle at 12% 0%, rgba(74, 158, 255, 0.18), transparent 28%),
    radial-gradient(circle at 82% 100%, rgba(99, 102, 241, 0.16), transparent 32%);
  box-shadow:
    0 22px 60px rgba(2, 6, 23, 0.38),
    0 0 0 1px rgba(255, 255, 255, 0.04) inset,
    0 0 32px rgba(74, 158, 255, 0.16);
  backdrop-filter: blur(22px) saturate(1.25);
  -webkit-backdrop-filter: blur(22px) saturate(1.25);
}

.app-header__inner::before,
.app-header__inner::after {
  content: "";
  position: absolute;
  pointer-events: none;
}

.app-header__inner::before {
  left: 2.8%;
  right: 2.8%;
  top: 0;
  height: 2px;
  background:
    linear-gradient(90deg, transparent 2%, rgba(74, 158, 255, 0.68) 19%, transparent 37%),
    linear-gradient(90deg, transparent 58%, rgba(99, 102, 241, 0.58) 84%, transparent 98%);
  opacity: 0.82;
  filter: blur(0.35px);
}

.app-header__inner::after {
  left: 6%;
  right: 6%;
  bottom: 0;
  height: 2px;
  background:
    linear-gradient(90deg, transparent 6%, rgba(74, 158, 255, 0.26) 30%, transparent 52%),
    linear-gradient(90deg, transparent 62%, rgba(99, 102, 241, 0.88) 82%, transparent 98%);
  opacity: 0.82;
  filter: blur(0.45px);
}

.v-theme--light .app-header__inner {
  border-color: rgba(79, 70, 229, 0.18);
  background:
    linear-gradient(135deg, rgba(255, 255, 255, 0.88), rgba(242, 247, 255, 0.74)),
    radial-gradient(circle at 12% 0%, rgba(74, 158, 255, 0.14), transparent 28%),
    radial-gradient(circle at 82% 100%, rgba(99, 102, 241, 0.13), transparent 32%);
  box-shadow:
    0 22px 60px rgba(15, 23, 42, 0.12),
    0 0 0 1px rgba(255, 255, 255, 0.7) inset,
    0 0 30px rgba(74, 158, 255, 0.12);
}

.app-header__brand {
  position: relative;
  z-index: 1;
  flex: 0 0 auto;
  display: flex;
  align-items: center;
}

.app-header__brand :deep(.app-logo) {
  gap: clamp(9px, 0.85vw, 14px);
  font-size: clamp(1.22rem, 1.28vw, 1.62rem);
  font-weight: 800;
  letter-spacing: -0.03em;
  white-space: nowrap;
}

.app-header__brand :deep(.app-logo__icon) {
  width: clamp(50px, 2.78vw, 58px);
  height: clamp(50px, 2.78vw, 58px);
  margin-right: 0;
  border-radius: 17px;
  box-shadow:
    0 0 0 1px rgba(129, 140, 248, 0.32) inset,
    0 0 24px rgba(99, 102, 241, 0.2);
}

.app-header__nav {
  position: relative;
  z-index: 1;
  flex: 1 1 auto;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: clamp(2px, 0.8vw, 12px);
  min-width: 0;
}

.app-header__nav :deep(.v-btn) {
  min-width: 0;
  height: 46px !important;
  padding-inline: clamp(8px, 0.86vw, 16px) !important;
  border-radius: 999px !important;
  color: rgba(241, 245, 249, 0.9) !important;
  font-size: clamp(0.95rem, 0.94vw, 1.06rem) !important;
  font-weight: 700 !important;
  letter-spacing: 0 !important;
  text-transform: none !important;
}

.app-header__nav :deep(.v-btn:hover) {
  background: rgba(99, 102, 241, 0.12) !important;
  color: #fff !important;
}

.app-header__spacer {
  display: none;
}

.app-header__desktop-actions {
  position: relative;
  z-index: 1;
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: clamp(8px, 0.85vw, 14px);
}

.app-header__download-btn {
  position: relative !important;
  min-width: clamp(142px, 11.2vw, 220px) !important;
  height: clamp(46px, 2.5vw, 52px) !important;
  padding-inline: clamp(16px, 1.32vw, 24px) !important;
  border-radius: 999px !important;
  border: 1px solid rgba(125, 211, 252, 0.58) !important;
  background:
    radial-gradient(circle at 22% 18%, rgba(255, 255, 255, 0.2), transparent 28%),
    linear-gradient(135deg, #08a9ff 0%, #6366f1 50%, #a855f7 100%) !important;
  color: #ffffff !important;
  box-shadow:
    0 12px 34px rgba(59, 130, 246, 0.35),
    0 0 28px rgba(168, 85, 247, 0.2),
    0 0 0 1px rgba(255, 255, 255, 0.12) inset;
  font-size: clamp(0.96rem, 1vw, 1.1rem) !important;
  font-weight: 800 !important;
  letter-spacing: 0 !important;
  text-transform: none !important;
  overflow: hidden;
  isolation: isolate;
  transition:
    transform 0.24s ease,
    filter 0.24s ease,
    box-shadow 0.24s ease !important;
}

.app-header__download-btn::after {
  content: "";
  position: absolute;
  inset: -60% -30%;
  background: linear-gradient(105deg, transparent 36%, rgba(255, 255, 255, 0.48) 48%, transparent 61%);
  transform: translateX(-72%) rotate(7deg);
  transition: transform 0.6s cubic-bezier(0.22, 0.72, 0.2, 1);
  pointer-events: none;
  z-index: 0;
}

.app-header__download-btn:hover {
  filter: saturate(1.12) brightness(1.05);
  transform: translateY(-1px);
  box-shadow:
    0 16px 42px rgba(59, 130, 246, 0.42),
    0 0 34px rgba(168, 85, 247, 0.26),
    0 0 0 1px rgba(255, 255, 255, 0.16) inset;
}

.app-header__download-btn:hover::after {
  transform: translateX(72%) rotate(7deg);
}

.app-header__download-btn :deep(.v-btn__prepend) {
  position: relative;
  z-index: 1;
  color: #22c55e;
}

.app-header__download-btn :deep(.v-icon) {
  font-size: clamp(20px, 1.2vw, 25px) !important;
}

.app-header__download-btn :deep(.v-btn__content) {
  position: relative;
  z-index: 1;
}

.app-header__language {
  width: clamp(108px, 7.6vw, 154px);
}

.app-header__language :deep(.language-switcher--compact) {
  width: 100%;
  min-width: 0;
}

.app-header__language :deep(.v-field) {
  min-height: clamp(46px, 2.5vw, 52px);
  border-radius: 999px;
  background:
    linear-gradient(135deg, rgba(255, 255, 255, 0.08), rgba(255, 255, 255, 0.03));
  border: 1px solid rgba(255, 255, 255, 0.12);
  box-shadow: 0 10px 26px rgba(2, 6, 23, 0.24);
}

.app-header__language :deep(.v-field__input) {
  min-height: clamp(46px, 2.5vw, 52px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding-inline: clamp(14px, 1.1vw, 20px) 6px;
}

.app-header__language :deep(.language-switcher__selection) {
  align-items: center;
  gap: clamp(8px, 0.7vw, 12px);
  line-height: 1;
}

.app-header__language :deep(.language-switcher__flag-icon) {
  width: clamp(23px, 1.38vw, 29px);
  height: clamp(23px, 1.38vw, 29px);
}

.app-header__language :deep(.language-switcher__code) {
  display: inline-flex;
  align-items: center;
  min-height: 1em;
  font-size: clamp(0.92rem, 0.9vw, 1.04rem);
  font-weight: 800;
}

.app-header__action-divider {
  width: 1px;
  height: clamp(28px, 1.72vw, 34px);
  background: linear-gradient(180deg, transparent, rgba(255, 255, 255, 0.24), transparent);
}

.app-header__theme :deep(.v-btn),
.app-header__menu-btn,
.mobile-menu__close {
  width: clamp(46px, 2.42vw, 50px) !important;
  height: clamp(46px, 2.42vw, 50px) !important;
  border-radius: 16px !important;
  background: rgba(255, 255, 255, 0.06) !important;
  border: 1px solid rgba(255, 255, 255, 0.12) !important;
  color: rgba(241, 245, 249, 0.94) !important;
  box-shadow: 0 10px 28px rgba(2, 6, 23, 0.24);
}

.app-header__theme :deep(.v-icon),
.app-header__menu-btn :deep(.v-icon),
.mobile-menu__close :deep(.v-icon) {
  font-size: clamp(22px, 1.22vw, 25px) !important;
}

.v-theme--light .app-header__nav :deep(.v-btn) {
  color: rgba(15, 23, 42, 0.82) !important;
}

.v-theme--light .app-header__nav :deep(.v-btn:hover) {
  background: rgba(99, 102, 241, 0.09) !important;
  color: #1e293b !important;
}

.v-theme--light .app-header__language :deep(.v-field),
.v-theme--light .app-header__theme :deep(.v-btn),
.v-theme--light .app-header__menu-btn {
  background: rgba(255, 255, 255, 0.72) !important;
  border-color: rgba(15, 23, 42, 0.1) !important;
  color: rgba(15, 23, 42, 0.78) !important;
  box-shadow: 0 10px 26px rgba(15, 23, 42, 0.1);
}

.app-header__mobile-actions {
  display: none;
}

@media (min-width: 1360px) {
  .app-header__inner {
    width: min(calc(100vw - clamp(260px, 24vw, 580px)), 1180px);
  }
}

@media (max-width: 1099px) {
  .app-header {
    height: 82px;
    padding: 10px 12px;
  }

  .app-header__inner {
    width: min(100%, 720px);
    min-height: 62px;
    gap: 10px;
    padding: 8px 10px 8px 10px;
    border-radius: 22px;
  }

  .app-header__brand :deep(.app-logo) {
    gap: 10px;
    font-size: clamp(1.05rem, 5vw, 1.28rem);
  }

  .app-header__brand :deep(.app-logo__icon) {
    width: 44px;
    height: 44px;
    border-radius: 13px;
  }

  .app-header__nav {
    display: none;
  }

  .app-header__desktop-actions {
    display: none;
  }

  .app-header__mobile-actions {
    display: flex;
    margin-left: auto;
  }

  .app-header__menu-btn,
  .mobile-menu__close {
    width: 46px !important;
    height: 46px !important;
    border-radius: 16px !important;
  }
}

@media (max-width: 380px) {
  .app-header__brand :deep(.app-logo) {
    font-size: 1rem;
  }

  .app-header__brand :deep(.app-logo__icon) {
    width: 40px;
    height: 40px;
  }
}

.mobile-menu-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
  display: flex;
  justify-content: center;
  padding: 12px;
  backdrop-filter: blur(18px);
  -webkit-backdrop-filter: blur(18px);
}

.mobile-menu-overlay--dark {
  background:
    radial-gradient(circle at 50% 0%, rgba(74, 158, 255, 0.18), transparent 36%),
    rgba(2, 6, 23, 0.78);
}

.mobile-menu-overlay--light {
  background:
    radial-gradient(circle at 50% 0%, rgba(74, 158, 255, 0.16), transparent 38%),
    rgba(248, 250, 252, 0.74);
}

.mobile-menu {
  width: min(100%, 440px);
  max-height: calc(100dvh - 24px);
  align-self: flex-start;
  padding: 14px 14px 20px;
  overflow-y: auto;
  color: rgba(241, 245, 249, 0.94);
  border: 1px solid rgba(165, 180, 252, 0.24);
  border-radius: 24px;
  background:
    linear-gradient(135deg, rgba(10, 17, 34, 0.94), rgba(8, 12, 24, 0.88)),
    radial-gradient(circle at 20% 0%, rgba(74, 158, 255, 0.16), transparent 35%);
  box-shadow:
    0 28px 80px rgba(0, 0, 0, 0.46),
    0 0 0 1px rgba(255, 255, 255, 0.04) inset;
}

.mobile-menu-overlay--light .mobile-menu {
  color: rgba(15, 23, 42, 0.9);
  border-color: rgba(79, 70, 229, 0.18);
  background:
    linear-gradient(135deg, rgba(255, 255, 255, 0.94), rgba(240, 247, 255, 0.88)),
    radial-gradient(circle at 20% 0%, rgba(74, 158, 255, 0.16), transparent 35%);
  box-shadow:
    0 28px 80px rgba(15, 23, 42, 0.16),
    0 0 0 1px rgba(255, 255, 255, 0.72) inset;
}

.mobile-menu__header {
  display: flex;
  align-items: center;
  gap: 12px;
  padding-bottom: 12px;
}

.mobile-menu__brand :deep(.app-logo) {
  gap: 10px;
  font-size: 1.08rem;
  font-weight: 800;
  color: inherit;
}

.mobile-menu__brand :deep(.app-logo__icon) {
  width: 42px;
  height: 42px;
  margin-right: 0;
  border-radius: 13px;
}

.mobile-menu__divider {
  border: none;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.mobile-menu-overlay--light .mobile-menu__divider {
  border-top-color: rgba(15, 23, 42, 0.12);
}

.mobile-menu__list {
  display: flex;
  flex-direction: column;
  padding: 8px 0;
}

.mobile-menu__link {
  display: flex;
  align-items: center;
  padding: 14px 16px;
  font-size: 1.02rem;
  font-weight: 650;
  color: inherit;
  text-decoration: none;
  border-radius: 16px;
  transition: background-color 0.15s;
}

.mobile-menu__link:hover {
  background: rgba(99, 102, 241, 0.12);
}

.mobile-menu__actions {
  display: flex;
  flex-direction: row;
  gap: 12px;
  align-items: center;
  justify-content: space-between;
  padding-top: 16px;
}

.mobile-menu__actions :deep(.language-switcher--compact) {
  flex: 0 0 178px;
  width: 178px;
  max-width: calc(100% - 64px);
  min-width: 0;
}

.mobile-menu__actions :deep(.v-field) {
  min-height: 52px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.06);
  border: 1px solid rgba(255, 255, 255, 0.12);
  color: rgba(241, 245, 249, 0.94);
}

.mobile-menu-overlay--light .mobile-menu__actions :deep(.v-field) {
  background: rgba(255, 255, 255, 0.72);
  border-color: rgba(15, 23, 42, 0.12);
  color: rgba(15, 23, 42, 0.9);
  box-shadow: 0 10px 28px rgba(15, 23, 42, 0.08);
}

.mobile-menu__actions :deep(.v-field__input) {
  min-height: 52px;
  display: flex;
  align-items: center;
  justify-content: flex-start;
  padding: 0 10px 0 16px;
}

.mobile-menu__actions :deep(.v-field__append-inner) {
  align-self: center;
  display: flex;
  align-items: center;
  padding-top: 0;
  margin-inline-start: auto;
  color: rgba(203, 213, 225, 0.78);
}

.mobile-menu-overlay--light .mobile-menu__actions :deep(.v-field__append-inner) {
  color: rgba(15, 23, 42, 0.58);
}

.mobile-menu__actions :deep(.v-autocomplete__selection) {
  margin: 0;
}

.mobile-menu__actions :deep(.language-switcher__selection) {
  gap: 10px;
  line-height: 1;
}

.mobile-menu__actions :deep(.language-switcher__flag-icon) {
  width: 28px;
  height: 28px;
}

.mobile-menu__actions :deep(.language-switcher__code) {
  color: currentColor;
  font-size: 1rem;
  font-weight: 800;
}

.mobile-menu__theme :deep(.v-btn) {
  width: 46px !important;
  height: 46px !important;
  border-radius: 16px !important;
  background: rgba(255, 255, 255, 0.06) !important;
  border: 1px solid rgba(255, 255, 255, 0.12) !important;
  color: rgba(241, 245, 249, 0.94) !important;
}

.mobile-menu-overlay--light .mobile-menu__theme :deep(.v-btn),
.mobile-menu-overlay--light .mobile-menu__close {
  background: rgba(255, 255, 255, 0.72) !important;
  border-color: rgba(15, 23, 42, 0.12) !important;
  color: rgba(15, 23, 42, 0.82) !important;
  box-shadow: 0 10px 28px rgba(15, 23, 42, 0.08);
}

.mobile-menu-fade-enter-active,
.mobile-menu-fade-leave-active {
  transition: opacity 0.2s ease;
}

.mobile-menu-fade-enter-from,
.mobile-menu-fade-leave-to {
  opacity: 0;
}
</style>
