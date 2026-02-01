import { computed, watch } from "vue";
import { useThemeStore } from "~/stores/theme";

export const useBrowserTheme = () => {
  const themeStore = useThemeStore();
  const { $vuetifyTheme } = useNuxtApp();
  const vuetifyTheme = $vuetifyTheme as { global: { name: import("vue").Ref<string> } } | null;

  const applyVuetifyTheme = (name: "light" | "dark") => {
    if (vuetifyTheme) {
      vuetifyTheme.global.name.value = name;
    }
  };

  const applyTheme = (name: "light" | "dark") => {
    themeStore.setTheme(name, true);
    applyVuetifyTheme(name);
  };

  const initTheme = () => {
    if (!process.client) return;
    const initialTheme = themeStore.getInitialTheme();
    themeStore.setTheme(initialTheme, false);
    applyVuetifyTheme(initialTheme);

    if (process.client && !themeStore.userSelected) {
      const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
      const handler = (event: MediaQueryListEvent) => {
        if (!themeStore.userSelected) {
          const newTheme = event.matches ? "dark" : "light";
          themeStore.setTheme(newTheme, false);
          applyVuetifyTheme(newTheme);
        }
      };
      mediaQuery.addEventListener("change", handler);
    }
  };

  const toggleTheme = () => {
    applyTheme(themeStore.current === "dark" ? "light" : "dark");
  };

  watch(
    () => themeStore.current,
    (value) => {
      applyVuetifyTheme(value as "light" | "dark");
    }
  );

  return {
    currentTheme: computed(() => themeStore.current),
    isDark: computed(() => themeStore.current === "dark"),
    initTheme,
    toggleTheme
  };
};
