export type LocaleCode = "en" | "ru" | "es" | "fr" | "de" | "uk";

export const supportedLocales = [
  { code: "en", iso: "en-US", name: "English", flag: "🇺🇸", file: "en.json" },
  { code: "ru", iso: "ru-RU", name: "Русский", flag: "🇷🇺", file: "ru.json" },
  { code: "es", iso: "es-ES", name: "Español", flag: "🇪🇸", file: "es.json" },
  { code: "fr", iso: "fr-FR", name: "Français", flag: "🇫🇷", file: "fr.json" },
  { code: "de", iso: "de-DE", name: "Deutsch", flag: "🇩🇪", file: "de.json" },
  { code: "uk", iso: "uk-UA", name: "Українська", flag: "🇺🇦", file: "uk.json" }
] as const;

export const defaultLocale: LocaleCode = "en";

export const pages = ["/", "/download"] as const;

export const generateI18nRoutes = (): string[] => {
  const routes: string[] = [];
  for (const page of pages) {
    routes.push(page);
    for (const locale of supportedLocales) {
      if (locale.code === defaultLocale) {
        continue;
      }
      routes.push(page === "/" ? `/${locale.code}` : `/${locale.code}${page}`);
    }
  }
  return routes;
};
