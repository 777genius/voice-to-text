import vuetify from "vite-plugin-vuetify";
import { generateI18nRoutes, supportedLocales } from "./data/i18n";

const siteUrl = process.env.NUXT_PUBLIC_SITE_URL || "https://example.com";
const githubRepo = process.env.NUXT_PUBLIC_GITHUB_REPO || "777genius/voice-to-text";
const githubReleasesUrl = `https://github.com/${githubRepo}/releases`;
const tauriUpdaterUrl =
  process.env.NUXT_PUBLIC_TAURI_UPDATER_URL ||
  `https://github.com/${githubRepo}/releases/latest/download/latest.json`;

export default defineNuxtConfig({
  compatibilityDate: "2026-01-19",
  ssr: true,
  app: {
    head: {
      link: [
        // Ускоряем загрузку внешних ресурсов
        { rel: "preconnect", href: "https://api.voicetext.site" },
        { rel: "dns-prefetch", href: "https://api.voicetext.site" }
      ]
    }
  },
  modules: [
    "@pinia/nuxt",
    "@nuxtjs/i18n",
    "@vueuse/nuxt",
    "nuxt-icon",
    "@nuxt/eslint"
  ],
  css: ["@mdi/font/css/materialdesignicons.css", "~/assets/styles/main.scss"],
  components: [
    {
      path: "~/components",
      pathPrefix: false
    }
  ],
  build: {
    transpile: ["vuetify"]
  },
  vite: {
    plugins: [vuetify({ autoImport: true })]
  },
  nitro: {
    prerender: {
      // Важно для static деплоя (Render): пререндерим не только страницы, но и “сервисные” файлы
      // (sitemap/robots) + слепок актуальных ссылок на релизы.
      routes: [
        ...generateI18nRoutes(),
        "/sitemap.xml",
        "/robots.txt",
        "/_robots.txt",
        "/releases.json"
      ]
    }
  },
  i18n: {
    restructureDir: false,
    locales: supportedLocales,
    defaultLocale: "en",
    strategy: "prefix_except_default",
    lazy: true,
    langDir: "locales",
    bundle: {
      optimizeTranslationDirective: false
    },
    detectBrowserLanguage: {
      useCookie: true,
      cookieKey: "i18n_redirected",
      redirectOn: "root",
      alwaysRedirect: false,
      fallbackLocale: "en"
    }
  },
  site: {
    url: siteUrl,
    name: "VoicetextAI"
  },
  runtimeConfig: {
    // Опциональный токен для GitHub API: без него лимит 60 req/час на IP,
    // с токеном — 5000 req/час. Для прода рекомендуется выставить GITHUB_TOKEN.
    github: {
      token: process.env.GITHUB_TOKEN
    },
    public: {
      siteUrl,
      githubRepo,
      githubReleasesUrl,
      tauriUpdaterUrl,
      apiBaseUrl: process.env.NUXT_PUBLIC_API_BASE_URL || "https://api.voicetext.site"
    }
  }
});
