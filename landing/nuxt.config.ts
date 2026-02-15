import vuetify from "vite-plugin-vuetify";
import { generateI18nRoutes, supportedLocales } from "./data/i18n";

// В nuxt.config.ts мы используем process.env, но не тянем node-тайпинги в lint.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
declare const process: any;

const siteUrl = process.env.NUXT_PUBLIC_SITE_URL || "https://example.com";
const githubRepo = process.env.NUXT_PUBLIC_GITHUB_REPO || "777genius/voice-to-text";
const githubReleasesUrl = `https://github.com/${githubRepo}/releases`;
const tauriUpdaterUrl =
  process.env.NUXT_PUBLIC_TAURI_UPDATER_URL ||
  `https://github.com/${githubRepo}/releases/latest/download/latest.json`;

export default defineNuxtConfig({
  compatibilityDate: "2026-01-19",
  ssr: true,
  experimental: {
    // Не инлайним стили — они раздувают HTML на 60+ КиБ,
    // а render-blocking Vuetify CSS всё равно грузится отдельно
    inlineSSRStyles: false
  },
  app: {
    head: {
      link: [
        { rel: "icon", type: "image/x-icon", href: "/favicon.ico" },
        { rel: "icon", type: "image/png", sizes: "32x32", href: "/favicon-32.png" },
        { rel: "apple-touch-icon", sizes: "192x192", href: "/logo-192.png" },
        // Ускоряем загрузку внешних ресурсов
        { rel: "preconnect", href: "https://api.voicetext.site" },
        { rel: "dns-prefetch", href: "https://api.voicetext.site" },
        { rel: "preconnect", href: "https://www.googletagmanager.com" },
        { rel: "preconnect", href: "https://www.google-analytics.com" }
      ]
    }
  },
  modules: [
    "@pinia/nuxt",
    "@nuxtjs/i18n",
    "@vueuse/nuxt",
    "nuxt-icon",
    "@nuxt/eslint",
    "nuxt-gtag"
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
    compressPublicAssets: true,
    prerender: {
      // Важно для static деплоя (Render): пререндерим не только страницы, но и "сервисные" файлы
      // (sitemap/robots) + слепок актуальных ссылок на релизы.
      routes: [
        ...generateI18nRoutes(),
        "/sitemap.xml",
        "/robots.txt",
        "/_robots.txt",
        // "/releases.json" — теперь подгружаем с GitHub API в рантайме
      ]
    }
  },
  routeRules: {
    // Статика с хешем в имени — кешируем навсегда
    "/_nuxt/**": {
      headers: { "Cache-Control": "public, max-age=31536000, immutable" }
    }
  },
  i18n: {
    restructureDir: false,
    // Nuxt i18n expects a mutable array type, but our list is declared as readonly for safety.
    // Spread makes it mutable without changing runtime behavior.
    locales: [...supportedLocales] as any,
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
  // @ts-expect-error - поле предоставляется nuxt-модулями (например sitemap/site), типы в текущем конфиге не подключены.
  site: {
    url: siteUrl,
    name: "VoicetextAI"
  },
  gtag: {
    id: process.env.NUXT_PUBLIC_GTAG_ID,
    loadingStrategy: "defer",
    config: {
      send_page_view: true
    }
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
      apiBaseUrl: process.env.NUXT_PUBLIC_API_BASE_URL || "https://api.voicetext.site",
      paddle: {
        // Paddle.js client-side token (test_... for sandbox, live_... for production)
        clientToken: process.env.NUXT_PUBLIC_PADDLE_CLIENT_TOKEN || "",
        // "sandbox" | "live"
        environment: process.env.NUXT_PUBLIC_PADDLE_ENVIRONMENT || "live",
        // Price IDs for Paddle.Checkout.open()
        priceIds: {
          pro: process.env.NUXT_PUBLIC_PADDLE_PRICE_ID_PRO || "",
          business: process.env.NUXT_PUBLIC_PADDLE_PRICE_ID_BUSINESS || ""
        }
      }
    }
  }
});
