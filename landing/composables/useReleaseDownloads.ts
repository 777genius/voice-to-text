import type { DownloadArch, DownloadOs } from "~/data/downloads";

type Variant = { url: string | null; platformKey: string | null; version: string | null };

type DownloadsApiResponse = {
  ok: boolean;
  source: "github-releases";
  fetchedAt: string;
  version: string | null;
  notes: string | null;
  pubDate: string | null;
  variants: {
    macos: { arm64: Variant; x64: Variant; universal: Variant };
    windows: { x64: Variant };
    linux: { appimage: Variant; deb: Variant };
  };
};

type ResolveResult = { url: string; version: string | null } | null;

export const useReleaseDownloads = () => {
  const config = useRuntimeConfig();

  const fallbackUrl =
    (config.public.githubReleasesUrl as string) ||
    (config.public.githubRepo ? `https://github.com/${config.public.githubRepo}/releases` : "https://github.com/777genius/voice-to-text/releases");

  // Для static деплоя нам нужен “файл”, а не runtime API (его может не быть).
  // Поэтому генерим слепок на билде через prerender и читаем как обычный JSON.
  const { data, pending, error } = useFetch<DownloadsApiResponse>("/releases.json", {
    server: false,
    lazy: true
  });

  const resolve = (os: DownloadOs, arch: DownloadArch | "unknown"): ResolveResult => {
    const api = data.value;
    if (!api?.ok) return null;

    if (os === "windows") {
      const v = api.variants.windows.x64;
      return v.url ? { url: v.url, version: v.version || api.version } : null;
    }

    if (os === "linux") {
      const v = api.variants.linux.appimage.url ? api.variants.linux.appimage : api.variants.linux.deb;
      return v.url ? { url: v.url, version: v.version || api.version } : null;
    }

    // macOS: сначала пытаемся найти “universal”, если его нет — выбираем по архитектуре.
    if (os === "macos") {
      const universal = api.variants.macos.universal;
      if (universal.url) return { url: universal.url, version: universal.version || api.version };

      const byArch = arch === "arm64" ? api.variants.macos.arm64 : api.variants.macos.x64;
      if (byArch.url) return { url: byArch.url, version: byArch.version || api.version };

      // На всякий случай: если не смогли определить — вернём хоть что-то.
      const any = api.variants.macos.arm64.url ? api.variants.macos.arm64 : api.variants.macos.x64;
      return any.url ? { url: any.url, version: any.version || api.version } : null;
    }

    return null;
  };

  const resolveUrlOrFallback = (os: DownloadOs, arch: DownloadArch | "unknown"): string => {
    return resolve(os, arch)?.url || fallbackUrl;
  };

  return { data, pending, error, fallbackUrl, resolve, resolveUrlOrFallback };
};

