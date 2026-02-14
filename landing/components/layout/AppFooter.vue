<script setup lang="ts">
const { t } = useI18n();
const year = new Date().getFullYear();
</script>

<template>
  <footer class="app-footer">
    <v-container class="app-footer__inner">
      <span class="app-footer__copy">{{ t("footer.copyright", { year }) }} · {{ t("footer.tagline") }}</span>
      <div class="app-footer__links">
        <NuxtLink class="app-footer__link" to="/terms">{{ t("footer.links.terms") }}</NuxtLink>
        <span class="app-footer__sep">·</span>
        <NuxtLink class="app-footer__link" to="/privacy-policy">{{ t("footer.links.privacyPolicy") }}</NuxtLink>
        <span class="app-footer__sep">·</span>
        <NuxtLink class="app-footer__link" to="/refund-policy">{{ t("footer.links.refundPolicy") }}</NuxtLink>
      </div>
    </v-container>
  </footer>
</template>

<style scoped>
.app-footer {
  border-top: 1px solid rgba(255, 255, 255, 0.08);
  padding: 20px 0;
}

.app-footer__inner {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.app-footer__copy {
  font-size: 13px;
  opacity: 0.5;
}

.app-footer__links {
  display: flex;
  align-items: center;
  gap: 12px;
}

.app-footer__link {
  color: inherit;
  text-decoration: none;
  font-size: 13px;
  opacity: 0.5;
  transition: opacity 0.2s ease;
}

.app-footer__link:hover {
  opacity: 0.85;
}

.app-footer__sep {
  opacity: 0.25;
  user-select: none;
}

/* Light theme */
.v-theme--light .app-footer {
  border-top-color: rgba(0, 0, 0, 0.08);
}

@media (max-width: 600px) {
  .app-footer__inner {
    flex-direction: column;
    gap: 10px;
    text-align: center;
  }
}
</style>
