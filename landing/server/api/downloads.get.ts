import type { DownloadsApiResponse, GitHubRelease } from "../../utils/releaseDownloads";
import { createEmptyDownloadsResponse, normalizeGitHubReleases } from "../../utils/releaseDownloads";

const RELEASES_PER_PAGE = 30;
let cache: { ts: number; value: DownloadsApiResponse } | null = null;

export default defineEventHandler(async (): Promise<DownloadsApiResponse> => {
  if (cache && Date.now() - cache.ts < 20 * 60 * 1000) return cache.value;

  const config = useRuntimeConfig();
  const githubRepo = (config.public.githubRepo as string) || "777genius/voice-to-text";
  const githubToken = ((config.github as Record<string, string>)?.token as string) || "";

  try {
    const releases = await $fetch<GitHubRelease[]>(
      `https://api.github.com/repos/${githubRepo}/releases?per_page=${RELEASES_PER_PAGE}`,
      {
        headers: {
          "User-Agent": "voicetextai-landing",
          Accept: "application/vnd.github+json",
          ...(githubToken && { Authorization: `Bearer ${githubToken}` }),
        },
      }
    );

    const value = normalizeGitHubReleases(releases);
    cache = { ts: Date.now(), value };
    return value;
  } catch {
    const empty = createEmptyDownloadsResponse();
    cache = { ts: Date.now(), value: empty };
    return empty;
  }
});
