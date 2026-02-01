<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { features } from '~/data/features'
import { useLandingContent } from '~/composables/useLandingContent'

const { content } = useLandingContent();
const { t } = useI18n();

const items = computed(() =>
  features
    .map((feature) => {
      const contentItem = content.value.features.find((item) => item.id === feature.id);
      if (!contentItem) return null;
      return { ...contentItem, icon: feature.icon, accent: feature.accent };
    })
    .filter(Boolean)
);
</script>

<template>  
  <section id="features" class="features-section section anchor-offset">
    <div class="features-section__bg">
      <div class="features-section__orb features-section__orb--1" />
      <div class="features-section__orb features-section__orb--2" />
    </div>
    <v-container>
      <div class="features-section__header">
        <span class="features-section__badge">{{ t("nav.features") }}</span>
        <h2 class="features-section__title">
          {{ t("features.sectionTitle") }}
        </h2>
        <p class="features-section__subtitle">
          {{ t("features.sectionSubtitle") }}
        </p>
      </div>

      <v-row>
        <v-col
          v-for="(item, index) in items"
          :key="item.id"
          cols="12"
          sm="6"
          lg="4"
        >
          <div
            class="features-section__card-wrap"
            :style="{ '--delay': `${index * 0.06}s` }"
          >
            <FeatureCard
              :title="item.title"
              :description="item.description"
              :icon="item.icon"
              :accent="item.accent"
            />
          </div>
        </v-col>
      </v-row>
    </v-container>
  </section>
</template>

<style scoped>
.features-section {
  position: relative;
  overflow: hidden;
}

.features-section__bg {
  position: absolute;
  inset: 0;
  pointer-events: none;
  overflow: hidden;
}

.features-section__orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(100px);
  opacity: 0.08;
}

.features-section__orb--1 {
  width: 600px;
  height: 600px;
  background: #6366f1;
  top: -200px;
  right: -100px;
}

.features-section__orb--2 {
  width: 500px;
  height: 500px;
  background: #ec4899;
  bottom: -150px;
  left: -100px;
}

.features-section__header {
  text-align: center;
  max-width: 640px;
  margin: 0 auto 56px;
  position: relative;
  z-index: 1;
}

.features-section__badge {
  display: inline-block;
  padding: 6px 18px;
  border-radius: 100px;
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(99, 102, 241, 0.15), rgba(236, 72, 153, 0.15));
  color: #6366f1;
  margin-bottom: 20px;
  border: 1px solid rgba(99, 102, 241, 0.2);
}

.features-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 16px;
  background: linear-gradient(135deg, currentColor 0%, rgba(99, 102, 241, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.features-section__subtitle {
  font-size: 1.1rem;
  opacity: 0.6;
  line-height: 1.6;
  margin: 0;
}

.features-section__card-wrap {
  animation: fadeInUp 0.5s ease both;
  animation-delay: var(--delay, 0s);
  height: 100%;
}

@keyframes fadeInUp {
  from {
    opacity: 0;
    transform: translateY(24px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

/* Dark theme adjustments */
.v-theme--dark .features-section__orb {
  opacity: 0.12;
}

.v-theme--dark .features-section__orb--1 {
  background: #818cf8;
}

.v-theme--dark .features-section__orb--2 {
  background: #f472b6;
}

.v-theme--dark .features-section__badge {
  background: linear-gradient(135deg, rgba(129, 140, 248, 0.15), rgba(244, 114, 182, 0.15));
  color: #a5b4fc;
  border-color: rgba(129, 140, 248, 0.25);
}

.v-theme--dark .features-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #a5b4fc 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .features-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

/* Light theme adjustments */
.v-theme--light .features-section__orb {
  opacity: 0.06;
}

.v-theme--light .features-section__badge {
  color: #4f46e5;
}

@media (max-width: 960px) {
  .features-section__title {
    font-size: 1.85rem;
  }

  .features-section__header {
    margin-bottom: 40px;
  }

  .features-section__subtitle {
    font-size: 1rem;
  }
}

@media (max-width: 600px) {
  .features-section__title {
    font-size: 1.6rem;
  }

  .features-section__header {
    margin-bottom: 32px;
  }
}
</style>
