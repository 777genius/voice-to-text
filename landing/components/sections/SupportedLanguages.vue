<script setup lang="ts">
const { languages, loading } = useSupportedLanguages();
const { t } = useI18n();
</script>

<template>
  <section
    v-if="!loading && languages.length > 0"
    id="languages"
    class="languages-section section"
  >

    <v-container fluid class="languages-section__container">
      <div class="languages-section__header">
        <h2 class="languages-section__title">
          {{ t("languages.title") }}
        </h2>
        <p class="languages-section__subtitle">
          {{ t("languages.subtitle") }}
        </p>
      </div>

      <div class="languages-section__grid">
        <v-tooltip
          v-for="lang in languages"
          :key="lang.code"
          :text="lang.name"
          location="bottom"
        >
          <template #activator="{ props }">
            <div
              v-bind="props"
              class="languages-section__item"
            >
              <img
                :src="`https://flagcdn.com/${lang.country_code}.svg`"
                :alt="lang.name"
                class="languages-section__flag"
                loading="lazy"
                width="28"
                height="20"
              />
            </div>
          </template>
        </v-tooltip>
      </div>
    </v-container>
  </section>
</template>

<style scoped>
.languages-section {
  position: relative;
  padding: 80px 0;
}

.languages-section__container {
  max-width: 1400px;
  padding: 0 48px;
}

/* ─── Header ─── */
.languages-section__header {
  text-align: center;
  margin-bottom: 48px;
}

.languages-section__badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 16px;
  border-radius: 100px;
  font-size: 0.78rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  //background: linear-gradient(135deg, rgba(99, 102, 241, 0.1), rgba(236, 72, 153, 0.1));
  color: #6366f1;
  border: 1px solid rgba(99, 102, 241, 0.15);
  margin-bottom: 20px;
}

.languages-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.2;
  margin-bottom: 14px;
  background: linear-gradient(135deg, currentColor 0%, #6366f1 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.languages-section__subtitle {
  font-size: 1.1rem;
  line-height: 1.6;
  opacity: 0.6;
  max-width: 540px;
  margin: 0 auto;
}

/* ─── Grid ─── */
.languages-section__grid {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 10px;
  max-width: 1100px;
  margin: 0 auto;
}

.languages-section__item {
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 10px;
  transition: all 0.25s ease;
  cursor: default;
}

.languages-section__item:hover {
  background: rgba(var(--v-theme-surface-variant), 0.12);
  transform: translateY(-3px) scale(1.15);
  box-shadow: 0 6px 20px rgba(99, 102, 241, 0.12);
}

.languages-section__flag {
  width: 28px;
  height: 20px;
  object-fit: cover;
  display: block;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
}

/* ─── Dark Theme ─── */
.v-theme--dark .languages-section__badge {
  background: linear-gradient(135deg, rgba(129, 140, 248, 0.15), rgba(244, 114, 182, 0.15));
  color: #a5b4fc;
  border-color: rgba(129, 140, 248, 0.25);
}

.v-theme--dark .languages-section__title {
  background: linear-gradient(135deg, #f1f5f9 0%, #a5b4fc 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .languages-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .languages-section__item {
  background: rgba(255, 255, 255, 0.03);
}

.v-theme--dark .languages-section__item:hover {
  background: rgba(255, 255, 255, 0.08);
  box-shadow: 0 6px 20px rgba(129, 140, 248, 0.12);
}

/* ─── Light Theme ─── */
.v-theme--light .languages-section__title {
  background: linear-gradient(135deg, #1e293b 0%, #4f46e5 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--light .languages-section__subtitle {
  color: #475569;
}

.v-theme--light .languages-section__item {
  background: rgba(99, 102, 241, 0.02);
}

.v-theme--light .languages-section__item:hover {
  background: rgba(99, 102, 241, 0.06);
}

/* ─── Responsive ─── */
@media (max-width: 960px) {
  .languages-section {
    padding: 60px 0;
  }

  .languages-section__container {
    padding: 0 32px;
  }

  .languages-section__title {
    font-size: 2rem;
  }

  .languages-section__header {
    margin-bottom: 36px;
  }
}

@media (max-width: 600px) {
  .languages-section {
    padding: 48px 0;
  }

  .languages-section__container {
    padding: 0 16px;
  }

  .languages-section__title {
    font-size: 1.6rem;
  }

  .languages-section__subtitle {
    font-size: 0.95rem;
  }

  .languages-section__grid {
    gap: 6px;
  }

  .languages-section__item {
    padding: 6px 8px;
    border-radius: 8px;
  }

  .languages-section__flag {
    width: 24px;
    height: 17px;
  }
}
</style>
