<script setup lang="ts">
usePageSeo("meta.refundPolicyTitle", "meta.refundPolicyDescription");
</script>

<template>
  <v-container class="section legal-page">
    <h1 class="text-h4 section-title">Refund Policy</h1>
    <p class="text-body-2 legal-updated">Last updated: February 2, 2026</p>

    <div class="legal-content">
      <section class="legal-section">
        <h2 class="text-h6">1. Overview</h2>
        <p>
          This Refund Policy applies to paid subscriptions to VoicetextAI (the "Service").
          All payments are processed by
          <a href="https://www.paddle.com" target="_blank" rel="noopener">Paddle.com</a>,
          which acts as the Merchant of Record for all transactions.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">2. 14-Day Refund Period</h2>
        <p>
          You may request a full refund within 14 days of your purchase or subscription renewal
          date, for any reason. Refunds are processed by Paddle and returned to the original
          payment method.
        </p>
        <p>
          Your refund rights are governed by
          <a href="https://www.paddle.com/legal/checkout-buyer-terms" target="_blank" rel="noopener">Paddle's Buyer Terms</a>.
          This policy does not limit any statutory rights you may have under applicable consumer
          protection laws.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">3. Subscriptions and Cancellations</h2>
        <p>
          You can cancel your subscription at any time. Cancellation takes effect at the end of the
          current billing period. If you wish to request a refund, please do so within the 14-day
          refund period described above.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">4. How to Request a Refund</h2>
        <p>You can request a refund in two ways:</p>
        <ul>
          <li>
            Through the refund link in your Paddle receipt or the
            <a href="https://www.paddle.com" target="_blank" rel="noopener">Paddle customer portal</a>
          </li>
          <li>
            By contacting us at
            <a href="mailto:iliyazelenkog@gmail.com">iliyazelenkog@gmail.com</a>
          </li>
        </ul>
        <p>
          Once approved, refunds are typically processed within several business days depending on
          your bank or card provider.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">5. Changes to This Policy</h2>
        <p>
          We may update this Refund Policy from time to time. Changes will be indicated by updating
          the "Last updated" date above.
        </p>
      </section>
    </div>
  </v-container>
</template>

<style scoped>
.legal-page {
  max-width: 800px;
}

.legal-updated {
  color: rgba(var(--v-theme-on-background), 0.6);
  margin-bottom: 32px;
}

.legal-content {
  line-height: 1.7;
}

.legal-section {
  margin-bottom: 32px;
}

.legal-section h2 {
  margin-bottom: 12px;
}

.legal-section p {
  margin-bottom: 12px;
}

.legal-section ul {
  margin: 8px 0 16px 24px;
}

.legal-section li {
  margin-bottom: 6px;
}

.legal-section a {
  color: rgb(var(--v-theme-primary));
}
</style>
