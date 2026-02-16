<script setup lang="ts">
import { mdiCheckCircle, mdiShieldCheckOutline } from '@mdi/js'
import { useI18n } from 'vue-i18n'
import { useLandingContent } from '~/composables/useLandingContent'

const { content } = useLandingContent()
const { t, locale } = useI18n()
const cfg = useRuntimeConfig()
const { $paddle } = useNuxtApp()

const isCheckoutDialogOpen = ref(false)
const checkoutEmail = ref("")
const selectedPlanId = ref<string | null>(null)

const priceIdByPlan = computed<Record<string, string>>(() => {
  const p = (cfg.public as any)?.paddle?.priceIds || {}
  return {
    pro: p.pro || "",
    business: p.business || ""
  }
})

const canUsePaddle = computed(() => Boolean(($paddle as any) && (cfg.public as any)?.paddle?.clientToken))

function onGetStarted(plan: { id: string }) {
  // Free tier → просто ведём на скачивание как раньше.
  if (plan.id === "free") {
    const el = document.getElementById('download')
    if (el) {
      el.scrollIntoView({ behavior: 'smooth' })
    }
    return
  }

  // Paid plans → открываем checkout (если Paddle.js настроен).
  const priceId = priceIdByPlan.value[plan.id]
  if (!priceId || !canUsePaddle.value) {
    const el = document.getElementById('download')
    if (el) {
      el.scrollIntoView({ behavior: 'smooth' })
    }
    return
  }

  selectedPlanId.value = plan.id
  isCheckoutDialogOpen.value = true
}

function openCheckout() {
  const planId = selectedPlanId.value
  if (!planId) return

  const priceId = priceIdByPlan.value[planId]
  if (!priceId || !canUsePaddle.value) return

  const email = checkoutEmail.value.trim()
  if (!email) return

  const localePrefix = locale.value === "en" ? "" : `/${locale.value}`
  const successUrl = `${window.location.origin}${localePrefix}/checkout-success`
  // Paddle recommends passing chosen locale if you have a language selector.
  // We pass only locales Paddle Checkout lists as supported; otherwise it falls back to browser locale.
  // Source: Paddle.Checkout.open() docs → settings.locale allowed values.
  const paddleLocale = ([
    "ar",
    "zh-Hans",
    "zh-TW",
    "da",
    "nl",
    "en",
    "fr",
    "de",
    "it",
    "ja",
    "ko",
    "no",
    "pl",
    "pt",
    "pt-BR",
    "tr",
    "ru",
    "es",
    "sv"
  ] as const).includes(locale.value as any)
    ? (locale.value as string)
    : undefined

  ;($paddle as any).Checkout.open({
    // В доке рекомендуют открывать overlay через settings.displayMode
    settings: {
      displayMode: "overlay",
      variant: "multi-page",
      // Важно: мы используем email как ключевой идентификатор для доставки license key.
      // Если дать пользователю поменять email в самом checkout, то `customData.voicetext_customer_email`
      // может разъехаться с фактическим email в Paddle — и мы отправим ключ не туда.
      allowLogout: false,
      ...(paddleLocale ? { locale: paddleLocale } : {}),
      // По оф. доке можно редиректить на свою success page. Это UX-сигнал, не “истина”.
      successUrl
    },
    items: [{ priceId, quantity: 1 }],
    // Prefill email to speed up checkout (and makes it deterministic for our webhooks).
    customer: { email },
    // We rely on this value as fallback for email extraction in webhooks.
    customData: {
      voicetext_customer_email: email
    }
  })

  isCheckoutDialogOpen.value = false
  checkoutEmail.value = ""
  selectedPlanId.value = null

  const el = document.getElementById('download')
  if (el) {
    el.scrollIntoView({ behavior: 'smooth' })
  }
}
</script>

<template>
  <section id="pricing" class="pricing-section section anchor-offset">

    <v-container>
      <div class="pricing-section__header">
        <h2 class="pricing-section__title">
          {{ t("pricing.sectionTitle") }}
        </h2>
        <p class="pricing-section__subtitle">
          {{ t("pricing.sectionSubtitle") }}
        </p>
      </div>

      <v-row justify="center" class="pricing-section__grid">
        <v-col
          v-for="(plan, index) in content.pricing"
          :key="plan.id"
          cols="12"
          sm="6"
          lg="4"
        >
          <div
            class="pricing-card"
            :class="{
              'pricing-card--highlighted': plan.highlighted,
            }"
            :style="{ '--delay': `${index * 0.1}s` }"
          >
            <div v-if="plan.highlighted" class="pricing-card__popular">
              {{ t("pricing.popular") }}
            </div>

            <div class="pricing-card__header">
              <h3 class="pricing-card__name">{{ plan.name }}</h3>
              <div class="pricing-card__price-wrap">
                <span class="pricing-card__price">{{ plan.price }}</span>
                <span class="pricing-card__period">/ {{ plan.period }}</span>
              </div>
              <p class="pricing-card__description">{{ plan.description }}</p>
            </div>

            <div class="pricing-card__divider" />

            <ul class="pricing-card__features">
              <li
                v-for="(feature, fIndex) in plan.features"
                :key="fIndex"
                class="pricing-card__feature"
              >
                <v-icon size="18" class="pricing-card__check" :icon="mdiCheckCircle" />
                {{ feature }}
              </li>
            </ul>

            <button
              class="pricing-card__btn"
              :class="{ 'pricing-card__btn--primary': plan.highlighted }"
              @click="onGetStarted(plan)"
            >
              {{ t("pricing.getStarted") }}
            </button>
          </div>
        </v-col>
      </v-row>

      <p class="pricing-section__refund-note">
        <v-icon size="16" class="pricing-section__refund-icon" :icon="mdiShieldCheckOutline" />
        {{ t("pricing.refundNote") }}
      </p>

      <v-dialog v-model="isCheckoutDialogOpen" max-width="520">
        <v-card>
          <v-card-title class="text-h6">Checkout</v-card-title>
          <v-card-text>
            <p style="opacity: 0.75; margin-bottom: 12px">
              Enter your email to continue. We use it to send receipts and to deliver your license key.
            </p>
            <v-text-field
              v-model="checkoutEmail"
              label="Email"
              type="email"
              autocomplete="email"
              variant="outlined"
              placeholder="you@example.com"
            />
          </v-card-text>
          <v-card-actions>
            <v-spacer />
            <v-btn variant="text" @click="isCheckoutDialogOpen = false">Cancel</v-btn>
            <v-btn color="primary" :disabled="!checkoutEmail.trim()" @click="openCheckout">
              Continue
            </v-btn>
          </v-card-actions>
        </v-card>
      </v-dialog>
    </v-container>
  </section>
</template>

<style scoped>
.pricing-section {
  position: relative;
}

/* Header */
.pricing-section__header {
  text-align: center;
  max-width: 640px;
  margin: 0 auto 56px;
  position: relative;
  z-index: 1;
}

.pricing-section__badge {
  display: inline-block;
  padding: 6px 18px;
  border-radius: 100px;
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.15), rgba(6, 182, 212, 0.15));
  color: #8b5cf6;
  margin-bottom: 20px;
  border: 1px solid rgba(139, 92, 246, 0.2);
}

.pricing-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 16px;
  background: linear-gradient(135deg, currentColor 0%, rgba(139, 92, 246, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.pricing-section__subtitle {
  font-size: 1.1rem;
  opacity: 0.6;
  line-height: 1.6;
  margin: 0;
}

/* Card grid */
.pricing-section__grid {
  position: relative;
  z-index: 1;
}

/* Pricing Card */
.pricing-card {
  position: relative;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 20px;
  padding: 36px 28px;
  height: 100%;
  display: flex;
  flex-direction: column;
  transition: transform 0.3s ease, box-shadow 0.3s ease, border-color 0.3s ease;
  animation: fadeInUp 0.5s ease both;
  animation-delay: var(--delay, 0s);
  backdrop-filter: blur(12px);
}

.pricing-card:hover {
  transform: translateY(-4px);
  box-shadow: 0 20px 40px rgba(0, 0, 0, 0.1);
}

.pricing-card--highlighted {
  border-color: rgba(139, 92, 246, 0.4);
  background: linear-gradient(
    180deg,
    rgba(139, 92, 246, 0.08) 0%,
    rgba(6, 182, 212, 0.04) 100%
  );
  box-shadow: 0 0 40px rgba(139, 92, 246, 0.08);
}

.pricing-card--highlighted:hover {
  box-shadow: 0 20px 60px rgba(139, 92, 246, 0.15);
  border-color: rgba(139, 92, 246, 0.6);
}

/* Popular badge */
.pricing-card__popular {
  position: absolute;
  top: -1px;
  right: 24px;
  padding: 6px 16px;
  border-radius: 0 0 12px 12px;
  font-size: 0.7rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  background: linear-gradient(135deg, #8b5cf6, #06b6d4);
  color: #fff;
}

/* Card header */
.pricing-card__header {
  margin-bottom: 4px;
}

.pricing-card__name {
  font-size: 1.2rem;
  font-weight: 700;
  margin-bottom: 12px;
  opacity: 0.9;
}

.pricing-card__price-wrap {
  display: flex;
  align-items: baseline;
  gap: 4px;
  margin-bottom: 12px;
}

.pricing-card__price {
  font-size: 3rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1;
  background: linear-gradient(135deg, currentColor, rgba(139, 92, 246, 0.8));
  -webkit-background-clip: text;
  background-clip: text;
}

.pricing-card--highlighted .pricing-card__price {
  background: linear-gradient(135deg, #8b5cf6, #06b6d4);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.pricing-card__period {
  font-size: 0.9rem;
  opacity: 0.5;
  font-weight: 500;
}

.pricing-card__description {
  font-size: 0.9rem;
  opacity: 0.55;
  line-height: 1.5;
  margin: 0;
}

/* Divider */
.pricing-card__divider {
  height: 1px;
  background: linear-gradient(
    90deg,
    transparent,
    rgba(139, 92, 246, 0.2),
    transparent
  );
  margin: 20px 0;
}

/* Features list */
.pricing-card__features {
  list-style: none;
  padding: 0;
  margin: 0 0 28px;
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.pricing-card__feature {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 0.88rem;
  opacity: 0.75;
  line-height: 1.4;
}

.pricing-card__check {
  color: #8b5cf6;
  flex-shrink: 0;
}

.pricing-card--highlighted .pricing-card__check {
  color: #06b6d4;
}

/* Button */
.pricing-card__btn {
  width: 100%;
  padding: 14px 24px;
  border-radius: 12px;
  font-size: 0.95rem;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.25s ease;
  border: 1px solid rgba(139, 92, 246, 0.25);
  background: transparent;
  color: inherit;
  opacity: 0.85;
}

.pricing-card__btn:hover {
  border-color: rgba(139, 92, 246, 0.5);
  background: rgba(139, 92, 246, 0.06);
  opacity: 1;
}

.pricing-card__btn--primary {
  background: linear-gradient(135deg, #8b5cf6, #06b6d4);
  color: #fff;
  border: none;
  opacity: 1;
  box-shadow: 0 4px 20px rgba(139, 92, 246, 0.3);
}

.pricing-card__btn--primary:hover {
  box-shadow: 0 6px 30px rgba(139, 92, 246, 0.45);
  transform: translateY(-1px);
  background: linear-gradient(135deg, #7c3aed, #0891b2);
}

/* Refund note */
.pricing-section__refund-note {
  text-align: center;
  margin-top: 32px;
  font-size: 0.85rem;
  opacity: 0.55;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  position: relative;
  z-index: 1;
}

.pricing-section__refund-icon {
  opacity: 0.7;
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

/* Dark theme */
.v-theme--dark .pricing-section__badge {
  background: linear-gradient(135deg, rgba(167, 139, 250, 0.15), rgba(34, 211, 238, 0.15));
  color: #a78bfa;
  border-color: rgba(167, 139, 250, 0.25);
}

.v-theme--dark .pricing-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #a78bfa 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .pricing-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .pricing-card {
  background: rgba(255, 255, 255, 0.03);
  border-color: rgba(255, 255, 255, 0.08);
}

.v-theme--dark .pricing-card--highlighted {
  background: linear-gradient(
    180deg,
    rgba(139, 92, 246, 0.1) 0%,
    rgba(6, 182, 212, 0.05) 100%
  );
  border-color: rgba(139, 92, 246, 0.35);
}

/* Light theme */
.v-theme--light .pricing-section__badge {
  color: #7c3aed;
}

.v-theme--light .pricing-card {
  background: rgba(255, 255, 255, 0.8);
  border-color: rgba(0, 0, 0, 0.08);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
}

.v-theme--light .pricing-card:hover {
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.08);
}

.v-theme--light .pricing-card--highlighted {
  background: linear-gradient(
    180deg,
    rgba(139, 92, 246, 0.06) 0%,
    rgba(6, 182, 212, 0.03) 100%
  );
  border-color: rgba(139, 92, 246, 0.25);
  box-shadow: 0 4px 20px rgba(139, 92, 246, 0.06);
}

.v-theme--light .pricing-card--highlighted:hover {
  box-shadow: 0 12px 40px rgba(139, 92, 246, 0.12);
}

.v-theme--light .pricing-card__btn {
  border-color: rgba(139, 92, 246, 0.3);
}

/* Responsive */
@media (max-width: 960px) {
  .pricing-section__title {
    font-size: 1.85rem;
  }

  .pricing-section__header {
    margin-bottom: 40px;
  }

  .pricing-section__subtitle {
    font-size: 1rem;
  }
}

@media (max-width: 600px) {
  .pricing-section__title {
    font-size: 1.6rem;
  }

  .pricing-section__header {
    margin-bottom: 32px;
  }

  .pricing-card {
    padding: 28px 22px;
  }

  .pricing-card__price {
    font-size: 2.4rem;
  }
}
</style>
