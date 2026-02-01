<script setup lang="ts">
usePageSeo("meta.privacyPolicyTitle", "meta.privacyPolicyDescription");
</script>

<template>
  <v-container class="section legal-page">
    <h1 class="text-h4 section-title">Privacy Policy</h1>
    <p class="text-body-2 legal-updated">Last updated: February 1, 2025</p>

    <div class="legal-content">
      <section class="legal-section">
        <h2 class="text-h6">1. Introduction</h2>
        <p>
          Welcome to Voice to Text ("we," "our," or "us"). We are committed to protecting your
          privacy. This Privacy Policy explains how we collect, use, and safeguard your information
          when you use our desktop application and related services (collectively, the "Service").
        </p>
        <p>
          By using the Service, you agree to the collection and use of information in accordance
          with this policy.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">2. Information We Collect</h2>

        <h3 class="text-subtitle-1 font-weight-bold">2.1 Account Information</h3>
        <p>
          When you create an account, we may collect your email address, username, and
          authentication credentials. If you sign in via a third-party provider (e.g., Google,
          GitHub), we receive your basic profile information as authorized by you.
        </p>

        <h3 class="text-subtitle-1 font-weight-bold">2.2 Audio and Transcription Data</h3>
        <p>
          Our Service processes voice audio to produce real-time text transcriptions. Depending on
          the speech-to-text provider you choose:
        </p>
        <ul>
          <li>
            <strong>Local processing (Whisper):</strong> Audio is processed entirely on your device.
            No audio or transcription data leaves your computer.
          </li>
          <li>
            <strong>Cloud providers (Deepgram, AssemblyAI):</strong> Audio is streamed to the
            selected third-party provider for transcription. We do not store or retain this audio or
            the resulting transcriptions on our servers.
          </li>
        </ul>

        <h3 class="text-subtitle-1 font-weight-bold">2.3 API Keys</h3>
        <p>
          If you use your own API keys for third-party speech-to-text providers, these keys are
          stored locally on your device using the operating system's secure storage (e.g., macOS
          Keychain). We never transmit or store your API keys on our servers.
        </p>

        <h3 class="text-subtitle-1 font-weight-bold">2.4 Usage Data</h3>
        <p>
          We may collect anonymous usage data such as application version, operating system, feature
          usage statistics, and error reports to improve the Service.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">3. How We Use Your Information</h2>
        <p>We use the information we collect to:</p>
        <ul>
          <li>Provide, maintain, and improve the Service</li>
          <li>Authenticate your account and manage your sessions</li>
          <li>Process licensing and subscription management</li>
          <li>Communicate with you regarding updates, security notices, and support</li>
          <li>Analyze usage patterns to improve user experience</li>
          <li>Detect, prevent, and address technical issues</li>
        </ul>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">4. Data Storage and Security</h2>
        <p>
          We implement industry-standard security measures to protect your data, including:
        </p>
        <ul>
          <li>AES-GCM encryption for sensitive data at rest</li>
          <li>TLS encryption for all data in transit</li>
          <li>Argon2id password hashing</li>
          <li>JWT-based authentication with Ed25519 signatures</li>
          <li>Rate limiting to prevent abuse</li>
        </ul>
        <p>
          Account data is stored on secured servers. Transcription data and API keys remain on your
          local device and are never uploaded to our servers.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">5. Third-Party Services</h2>
        <p>
          When you choose to use cloud-based speech-to-text providers, your audio data is processed
          by those third-party services according to their respective privacy policies:
        </p>
        <ul>
          <li>
            <a href="https://deepgram.com/privacy" target="_blank" rel="noopener noreferrer"
              >Deepgram Privacy Policy</a
            >
          </li>
          <li>
            <a href="https://www.assemblyai.com/privacy" target="_blank" rel="noopener noreferrer"
              >AssemblyAI Privacy Policy</a
            >
          </li>
        </ul>
        <p>
          We encourage you to review the privacy policies of any third-party provider before using
          their services through our application.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">6. Data Retention</h2>
        <p>
          We retain your account information for as long as your account is active. You may request
          deletion of your account and associated data at any time by contacting us. Usage
          statistics are retained in anonymized form for analytical purposes.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">7. Your Rights</h2>
        <p>Depending on your jurisdiction, you may have the right to:</p>
        <ul>
          <li>Access the personal data we hold about you</li>
          <li>Request correction of inaccurate data</li>
          <li>Request deletion of your data</li>
          <li>Object to or restrict processing of your data</li>
          <li>Data portability</li>
          <li>Withdraw consent at any time</li>
        </ul>
        <p>
          To exercise any of these rights, please contact us at
          <a href="mailto:iliyazelenkog@gmail.com">iliyazelenkog@gmail.com</a>.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">8. Children's Privacy</h2>
        <p>
          The Service is not directed to individuals under the age of 13. We do not knowingly
          collect personal information from children. If we become aware that a child under 13 has
          provided us with personal data, we will take steps to delete such information.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">9. International Data Transfers</h2>
        <p>
          Your information may be transferred to and maintained on servers located outside of your
          state, province, or country. By using the Service, you consent to such transfers. We take
          appropriate steps to ensure your data is treated securely and in accordance with this
          Privacy Policy.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">10. Changes to This Policy</h2>
        <p>
          We may update this Privacy Policy from time to time. We will notify you of any material
          changes by posting the updated policy on this page and updating the "Last updated" date.
          Your continued use of the Service after changes constitutes acceptance of the revised
          policy.
        </p>
      </section>

      <section class="legal-section">
        <h2 class="text-h6">11. Contact Us</h2>
        <p>
          If you have questions or concerns about this Privacy Policy, please contact us at:
        </p>
        <p>
          <strong>Email:</strong>
          <a href="mailto:iliyazelenkog@gmail.com">iliyazelenkog@gmail.com</a>
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

.legal-section h3 {
  margin-top: 16px;
  margin-bottom: 8px;
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
