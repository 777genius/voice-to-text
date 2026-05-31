import type { DownloadArch, DownloadOs } from "~/data/downloads";
import type { DownloadsApiResponse, GitHubRelease, ResolveResult } from "~/utils/releaseDownloads";
import {
  createEmptyDownloadsResponse,
  normalizeGitHubReleases,
  resolveReleaseDownload,
} from "~/utils/releaseDownloads";

const CACHE_KEY = "vtai_releases_v2";
const CACHE_TTL = 10 * 60 * 1000;
const RELEASES_PER_PAGE = 30;

const readCache = (): DownloadsApiResponse | null => {
  try {
    const raw = sessionStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const { ts, data } = JSON.parse(raw);
    if (Date.now() - ts > CACHE_TTL) return null;
    return data;
  } catch {
    return null;
  }
};

const writeCache = (data: DownloadsApiResponse): void => {
  try {
    sessionStorage.setItem(CACHE_KEY, JSON.stringify({ ts: Date.now(), data }));
  } catch {
    // sessionStorage может быть недоступен в private mode.
  }
};

export const useReleaseDownloads = () => {
  const config = useRuntimeConfig();
  const githubRepo = (config.public.githubRepo as string) || "777genius/voice-to-text";
  const fallbackUrl =
    (config.public.githubReleasesUrl as string) ||
    `https://github.com/${githubRepo}/releases`;

  const fetchFromGitHub = async (): Promise<DownloadsApiResponse> => {
    const releases = await $fetch<GitHubRelease[]>(
      `https://api.github.com/repos/${githubRepo}/releases?per_page=${RELEASES_PER_PAGE}`,
      {
        headers: { Accept: "application/vnd.github+json" },
      }
    );
    return normalizeGitHubReleases(releases);
  };

  const fetchFromStaticSnapshot = async (): Promise<DownloadsApiResponse> => {
    return await $fetch<DownloadsApiResponse>("/releases.json");
  };

  const { data, pending, error, execute } = useAsyncData<DownloadsApiResponse>("releases", async () => {
    const cached = readCache();
    if (cached) return cached;

    try {
      const fresh = await fetchFromGitHub();
      writeCache(fresh);
      return fresh;
    } catch {
      try {
        const snapshot = await fetchFromStaticSnapshot();
        writeCache(snapshot);
        return snapshot;
      } catch {
        return createEmptyDownloadsResponse();
      }
    }
  }, {
    server: false,
    immediate: true,
    lazy: true,
  });

  onMounted(() => {
    if (!data.value && !pending.value) {
      void execute();
    }
  });

  const ensureLoaded = async (): Promise<DownloadsApiResponse | null> => {
    if (!data.value) {
      await execute();
    }
    return data.value || null;
  };

  const resolve = (os: DownloadOs, arch: DownloadArch | "unknown"): ResolveResult => {
    return resolveReleaseDownload(data.value, os, arch);
  };

  const resolveUrlOrFallback = (os: DownloadOs, arch: DownloadArch | "unknown"): string => {
    return resolve(os, arch)?.url || (data.value ? fallbackUrl : "#download");
  };

  return {
    data,
    pending,
    error,
    fallbackUrl,
    ensureLoaded,
    resolve,
    resolveUrlOrFallback,
  };
};
