import type { DownloadArch, DownloadOs } from "~/data/downloads";

export type ReleaseAsset = {
  name: string;
  browser_download_url: string;
  size: number;
};

export type GitHubRelease = {
  tag_name: string;
  name?: string;
  body?: string;
  published_at: string;
  assets?: ReleaseAsset[];
};

export type ReleaseVariant = {
  url: string | null;
  platformKey: string | null;
  version: string | null;
  pubDate: string | null;
};

export type DownloadsApiResponse = {
  ok: boolean;
  source: "github-releases";
  fetchedAt: string;
  version: string | null;
  notes: string | null;
  pubDate: string | null;
  variants: {
    macos: { arm64: ReleaseVariant; x64: ReleaseVariant; universal: ReleaseVariant };
    windows: { x64: ReleaseVariant };
    linux: { appimage: ReleaseVariant; deb: ReleaseVariant };
  };
  all: { name: string; url: string; size: number }[];
};

export type ResolveResult = { url: string; version: string | null; pubDate: string | null } | null;

const emptyVariant: ReleaseVariant = {
  url: null,
  platformKey: null,
  version: null,
  pubDate: null,
};

const installableAssetExtensions = [".dmg", ".msi", ".exe", ".appimage", ".deb"];

const getVersion = (release: GitHubRelease): string | null => release.tag_name?.replace(/^v/, "") || null;

const sortReleasesByDateDesc = (releases: GitHubRelease[]): GitHubRelease[] =>
  [...releases].sort((a, b) => {
    const aTime = Date.parse(a.published_at || "");
    const bTime = Date.parse(b.published_at || "");
    return (Number.isFinite(bTime) ? bTime : 0) - (Number.isFinite(aTime) ? aTime : 0);
  });

const isInstallerAsset = (asset: ReleaseAsset): boolean => {
  const name = asset.name.toLowerCase();
  if (name.endsWith(".sig") || name.endsWith(".json") || name.endsWith(".tar.gz")) return false;
  return installableAssetExtensions.some((ext) => name.endsWith(ext));
};

const hasArchToken = (name: string, tokens: string[]): boolean => {
  const lowerName = name.toLowerCase();
  return tokens.some((token) => new RegExp(`(^|[_\\-.])${token}([_\\-.]|$)`, "i").test(lowerName));
};

const isMacUniversalDmg = (asset: ReleaseAsset): boolean =>
  /\.dmg$/i.test(asset.name) && hasArchToken(asset.name, ["universal"]);

const isMacArmDmg = (asset: ReleaseAsset): boolean =>
  /\.dmg$/i.test(asset.name) && hasArchToken(asset.name, ["aarch64", "arm64"]);

const isMacX64Dmg = (asset: ReleaseAsset): boolean =>
  /\.dmg$/i.test(asset.name) && hasArchToken(asset.name, ["x64", "x86_64", "amd64"]);

const isWindowsX64Installer = (asset: ReleaseAsset): boolean =>
  /\.(msi|exe)$/i.test(asset.name) && (
    hasArchToken(asset.name, ["x64", "x86_64", "amd64"]) || !hasArchToken(asset.name, ["arm64", "aarch64"])
  );

const isLinuxAppImage = (asset: ReleaseAsset): boolean => /\.appimage$/i.test(asset.name);
const isLinuxDeb = (asset: ReleaseAsset): boolean => /\.deb$/i.test(asset.name);

const toVariant = (release: GitHubRelease, asset: ReleaseAsset | null): ReleaseVariant => {
  if (!asset) return { ...emptyVariant };
  return {
    url: asset.browser_download_url,
    platformKey: asset.name,
    version: getVersion(release),
    pubDate: release.published_at || null,
  };
};

const findLatestVariant = (
  releases: GitHubRelease[],
  matcher: (asset: ReleaseAsset) => boolean
): ReleaseVariant => {
  for (const release of releases) {
    const asset = (release.assets || []).filter(isInstallerAsset).find(matcher) || null;
    if (asset) return toVariant(release, asset);
  }
  return { ...emptyVariant };
};

export const createEmptyDownloadsResponse = (): DownloadsApiResponse => ({
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
});

export const normalizeGitHubReleases = (input: GitHubRelease | GitHubRelease[]): DownloadsApiResponse => {
  const releases = sortReleasesByDateDesc(Array.isArray(input) ? input : [input]);
  const latestInstallableRelease = releases.find((release) => (release.assets || []).some(isInstallerAsset));

  if (!latestInstallableRelease) {
    return createEmptyDownloadsResponse();
  }

  const variants = {
    macos: {
      arm64: findLatestVariant(releases, isMacArmDmg),
      x64: findLatestVariant(releases, isMacX64Dmg),
      universal: findLatestVariant(releases, isMacUniversalDmg),
    },
    windows: {
      x64: findLatestVariant(releases, isWindowsX64Installer),
    },
    linux: {
      appimage: findLatestVariant(releases, isLinuxAppImage),
      deb: findLatestVariant(releases, isLinuxDeb),
    },
  };

  const all = releases.flatMap((release) =>
    (release.assets || [])
      .filter(isInstallerAsset)
      .map((asset) => ({
        name: asset.name,
        url: asset.browser_download_url,
        size: asset.size,
      }))
  );

  return {
    ok: all.length > 0,
    source: "github-releases",
    fetchedAt: new Date().toISOString(),
    version: getVersion(latestInstallableRelease),
    notes: latestInstallableRelease.body || null,
    pubDate: latestInstallableRelease.published_at || null,
    variants,
    all,
  };
};

export const resolveReleaseDownload = (
  data: DownloadsApiResponse | null | undefined,
  os: DownloadOs,
  arch: DownloadArch | "unknown"
): ResolveResult => {
  if (!data?.ok) return null;

  if (os === "windows") {
    const variant = data.variants.windows.x64;
    return variant.url ? { url: variant.url, version: variant.version, pubDate: variant.pubDate } : null;
  }

  if (os === "linux") {
    const variant = data.variants.linux.appimage.url ? data.variants.linux.appimage : data.variants.linux.deb;
    return variant.url ? { url: variant.url, version: variant.version, pubDate: variant.pubDate } : null;
  }

  const universal = data.variants.macos.universal;
  if (universal.url) return { url: universal.url, version: universal.version, pubDate: universal.pubDate };

  const byArch = arch === "arm64" ? data.variants.macos.arm64 : data.variants.macos.x64;
  if (byArch.url) return { url: byArch.url, version: byArch.version, pubDate: byArch.pubDate };

  const any = data.variants.macos.arm64.url ? data.variants.macos.arm64 : data.variants.macos.x64;
  return any.url ? { url: any.url, version: any.version, pubDate: any.pubDate } : null;
};
