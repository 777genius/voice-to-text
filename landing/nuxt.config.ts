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
        "/downloads.json"
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
    // Приватный токен не обязателен (мы в основном читаем public latest.json),
    // но оставляем на будущее, если захотим дергать GitHub API без лимитов.
    github: {
      token: process.env.GITHUB_TOKEN
    },
    public: {
      siteUrl,
      githubRepo,
      githubReleasesUrl,
      tauriUpdaterUrl,
      apiBaseUrl: process.env.NUXT_PUBLIC_API_BASE_URL || "http://localhost:8080"
    }
  }
});
