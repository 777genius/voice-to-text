// Артефакты из GitHub Releases API для прямого скачивания (.dmg, .msi, .AppImage, .deb)

type ReleaseAsset = {
  name: string;
  browser_download_url: string;
  size: number;
};

type GitHubRelease = {
  tag_name: string;
  name: string;
  body: string;
  published_at: string;
  assets: ReleaseAsset[];
};

type NormalizedVariant = {
  url: string | null;
  platformKey: string | null;
  version: string | null;
};

type ApiResponse = {
  ok: boolean;
  source: "github-releases";
  fetchedAt: string;
  version: string | null;
  notes: string | null;
  pubDate: string | null;
  variants: {
    macos: {
      arm64: NormalizedVariant;
      x64: NormalizedVariant;
      universal: NormalizedVariant;
    };
    windows: {
      x64: NormalizedVariant;
    };
    linux: {
      appimage: NormalizedVariant;
      deb: NormalizedVariant;
    };
  };
  all: { name: string; url: string; size: number }[];
};

const emptyVariant: NormalizedVariant = { url: null, platformKey: null, version: null };

// Паттерны для матчинга артефактов Tauri:
// VoicetextAI_0.6.0_aarch64.dmg, VoicetextAI_0.6.0_x64.dmg,
// VoicetextAI_0.6.0_x64_en-US.msi, VoicetextAI_0.6.0_amd64.AppImage, ...
const findAsset = (assets: ReleaseAsset[], pattern: RegExp): ReleaseAsset | null =>
  assets.find((a) => pattern.test(a.name)) || null;

const toVariant = (asset: ReleaseAsset | null, version: string | null): NormalizedVariant => {
  if (!asset) return emptyVariant;
  return { url: asset.browser_download_url, platformKey: asset.name, version };
};

let cache: { ts: number; value: ApiResponse } | null = null;

export default defineEventHandler(async (): Promise<ApiResponse> => {
  if (cache && Date.now() - cache.ts < 20 * 60 * 1000) return cache.value;

  const config = useRuntimeConfig();
  const githubRepo = (config.public.githubRepo as string) || "777genius/voice-to-text";
  const githubToken = ((config.github as Record<string, string>)?.token as string) || "";

  const empty: ApiResponse = {
    ok: false,
    source: "github-releases",
    fetchedAt: new Date().toISOString(),
    version: null,
    notes: null,
    pubDate: null,
    variants: {
      macos: { arm64: { ...emptyVariant }, x64: { ...emptyVariant }, universal: { ...emptyVariant } },
      windows: { x64: { ...emptyVariant } },
      linux: { appimage: { ...emptyVariant }, deb: { ...emptyVariant } },
    },
    all: [],
  };

  try {
    const release = await $fetch<GitHubRelease>(
      `https://api.github.com/repos/${githubRepo}/releases/latest`,
      {
        headers: {
          "User-Agent": "voicetextai-landing",
          Accept: "application/vnd.github+json",
          ...(githubToken && { Authorization: `Bearer ${githubToken}` }),
        },
      }
    );

    const version = release.tag_name?.replace(/^v/, "") || null;
    const assets = release.assets || [];

    // Installer-артефакты (исключаем .tar.gz, .sig, .json — это для Tauri updater)
    const installerAssets = assets.filter(
      (a) => !a.name.endsWith(".sig") && !a.name.endsWith(".json") && !a.name.endsWith(".tar.gz")
    );

    const macArm64 = findAsset(installerAssets, /_aarch64\.dmg$/i);
    const macX64 = findAsset(installerAssets, /_x64\.dmg$/i);
    const winX64 = findAsset(installerAssets, /\.msi$/i);
    const linuxAppImage = findAsset(installerAssets, /\.AppImage$/i);
    const linuxDeb = findAsset(installerAssets, /\.deb$/i);

    const value: ApiResponse = {
      ok: installerAssets.length > 0,
      source: "github-releases",
      fetchedAt: new Date().toISOString(),
      version,
      notes: release.body || null,
      pubDate: release.published_at || null,
      variants: {
        macos: {
          arm64: toVariant(macArm64, version),
          x64: toVariant(macX64, version),
          // Universal нет в Tauri-сборках, но оставляем слот
          universal: emptyVariant,
        },
        windows: {
          x64: toVariant(winX64, version),
        },
        linux: {
          appimage: toVariant(linuxAppImage, version),
          deb: toVariant(linuxDeb, version),
        },
      },
      all: installerAssets.map((a) => ({ name: a.name, url: a.browser_download_url, size: a.size })),
    };

    cache = { ts: Date.now(), value };
    return value;
  } catch {
    cache = { ts: Date.now(), value: empty };
    return empty;
  }
});
