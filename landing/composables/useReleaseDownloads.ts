import type { DownloadArch, DownloadOs } from "~/data/downloads";
import type { DownloadsApiResponse, GitHubRelease, ResolveResult } from "~/utils/releaseDownloads";
import {
  createEmptyDownloadsResponse,
  normalizeGitHubReleases,
  resolveReleaseDownload,
} from "~/utils/releaseDownloads";

const CACHE_KEY = "vtai_releases_v3";
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

  const data = useState<DownloadsApiResponse | null>("release-downloads:data", () => null);
  const pending = useState<boolean>("release-downloads:pending", () => false);
  const error = useState<unknown | null>("release-downloads:error", () => null);

  const load = async (force = false): Promise<DownloadsApiResponse | null> => {
    if (data.value && !force) return data.value;
    if (pending.value) return data.value;

    pending.value = true;
    error.value = null;

    try {
      const cached = !force ? readCache() : null;
      if (cached) {
        data.value = cached;
        return cached;
      }

      try {
        const snapshot = await fetchFromStaticSnapshot();
        if (snapshot.ok) {
          writeCache(snapshot);
          data.value = snapshot;
          return snapshot;
        }
      } catch {
        // Fall back to GitHub below.
      }

      const fresh = await fetchFromGitHub();
      writeCache(fresh);
      data.value = fresh;
      return fresh;
    } catch (err) {
      error.value = err;
      const empty = createEmptyDownloadsResponse();
      data.value = empty;
      return empty;
    } finally {
      pending.value = false;
    }
  };

  const refreshFromGitHub = async (): Promise<void> => {
    try {
      const fresh = await fetchFromGitHub();
      writeCache(fresh);
      data.value = fresh;
    } catch {
      // Static release snapshot stays visible if GitHub is slow or rate-limited.
    }
  };

  onMounted(() => {
    void load().then((loaded) => {
      if (loaded?.ok) void refreshFromGitHub();
    });
  });

  const ensureLoaded = async (): Promise<DownloadsApiResponse | null> => {
    return await load();
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
