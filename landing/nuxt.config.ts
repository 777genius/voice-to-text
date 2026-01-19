import vuetify from "vite-plugin-vuetify";
import { generateI18nRoutes, supportedLocales } from "./data/i18n";

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
      routes: generateI18nRoutes()
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
    url: "https://example.com",
    name: "Voice to Text"
  },
  runtimeConfig: {
    public: {
      siteUrl: "https://example.com"
    }
  }
});
