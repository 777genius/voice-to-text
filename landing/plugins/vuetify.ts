import "vuetify/styles";
import { createVuetify } from "vuetify";
import { useThemeStore } from "~/stores/theme";

export default defineNuxtPlugin({
  name: "vuetify",
  setup(nuxtApp) {
    const vuetify = createVuetify({
      theme: {
        defaultTheme: "dark",
        themes: {
          light: {
            colors: {
              primary: "#6366f1",
              secondary: "#8b5cf6",
              background: "#ffffff",
              surface: "#f8fafc"
            }
          },
          dark: {
            colors: {
              primary: "#6366f1",
              secondary: "#8b5cf6",
              background: "#0f172a",
              surface: "#1e293b"
            }
          }
        }
      }
    });

    nuxtApp.vueApp.use(vuetify);
    nuxtApp.provide("vuetifyTheme", vuetify.theme);
  }
});
