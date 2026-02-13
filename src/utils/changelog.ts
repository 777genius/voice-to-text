type GithubRepoRef = {
  owner: string;
  repo: string;
};

type ChangelogSource = GithubRepoRef & {
  /**
   * Путь к файлу в репозитории. Важно держать синхронно с релиз-процессом.
   */
  changelogPath: string;
};

// Репозиторий, из которого берём апдейты (см. `src-tauri/tauri.conf.json`).
const RELEASE_REPO: ChangelogSource = {
  owner: '777genius',
  repo: 'voice-to-text',
  // В релиз-репозитории changelog лежит в корне (см. кандидаты ниже).
  changelogPath: 'CHANGELOG.md',
};

const CHANGELOG_PATH_CANDIDATES = Array.from(
  new Set([RELEASE_REPO.changelogPath, 'CHANGELOG.md', 'docs/CHANGELOG.md'])
);

const inMemoryCache = new Map<string, string>();

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function normalizeVersion(version: string): string {
  return version.trim().replace(/^v/i, '');
}

/**
 * Вырезает секцию нужной версии из Keep a Changelog (пример: "## [0.7.0] — 2026-02-02").
 * Возвращает ТОЛЬКО содержимое секции (без заголовка версии).
 */
export function extractChangelogSection(changelog: string, version: string): string | null {
  const v = normalizeVersion(version);
  if (!v) return null;

  // Заголовок версии может быть в формате:
  // - ## [0.7.0] — 2026-02-02
  // - ## [0.7.0] - 2026-02-02
  // - ## 0.7.0
  const headerRe = new RegExp(
    String.raw`^##\s*(?:\[\s*)?${escapeRegExp(v)}(?:\s*\])?(?:\s*(?:—|-).*)?\s*$`,
    'm'
  );

  const headerMatch = changelog.match(headerRe);
  if (!headerMatch || headerMatch.index == null) return null;

  // Начинаем с конца строки заголовка версии.
  const afterHeaderIdx = changelog.indexOf('\n', headerMatch.index);
  const contentStart = afterHeaderIdx === -1 ? changelog.length : afterHeaderIdx + 1;

  // Конец секции — следующий заголовок версии (## ...), начиная со следующей строки.
  const remainder = changelog.slice(contentStart);
  const nextHeaderMatch = remainder.match(/^##\s+/m);
  const contentEnd = nextHeaderMatch?.index != null ? contentStart + nextHeaderMatch.index : changelog.length;

  const section = changelog.slice(contentStart, contentEnd).trim();
  return section ? section : null;
}

/**
 * Чуть-чуть нормализуем markdown секции, чтобы:
 * - убрать разделители `---`
 * - не притащить случайные заголовки `## ...` (на случай вложенных версий)
 */
export function normalizeChangelogSectionMarkdown(section: string): string {
  const lines = section.split(/\r?\n/);
  const out: string[] = [];

  for (const raw of lines) {
    const line = raw.trimEnd();
    const trimmed = line.trim();

    if (trimmed === '---') continue;
    if (trimmed.startsWith('## ')) continue;

    out.push(line);
  }

  const normalized = out.join('\n').trim();
  return normalized;
}

async function fetchRawFileFromGithub(repo: GithubRepoRef, tagOrBranch: string, path: string) {
  const url = `https://raw.githubusercontent.com/${repo.owner}/${repo.repo}/${tagOrBranch}/${path}`;
  const res = await fetch(url, { method: 'GET' });
  if (!res.ok) {
    throw new Error(`Failed to fetch ${url}: ${res.status}`);
  }
  return await res.text();
}

/**
 * Пытается получить "что нового" для конкретной версии из `frontend/CHANGELOG.md` в репозитории релизов.
 * Если секция не найдена или сетка недоступна — вернёт null (чтобы caller сделал fallback).
 */
export async function loadUpdateNotesFromChangelog(version: string): Promise<string | null> {
  const v = normalizeVersion(version);
  if (!v) return null;

  const cacheKey = `notes:${v}`;
  const cached = inMemoryCache.get(cacheKey);
  if (cached) return cached;

  // На практике теги у нас обычно "v0.7.0". Но на всякий случай поддержим и "0.7.0".
  const tagCandidates = [`v${v}`, v];

  for (const tag of tagCandidates) {
    for (const changelogPath of CHANGELOG_PATH_CANDIDATES) {
      try {
        const changelog = await fetchRawFileFromGithub(RELEASE_REPO, tag, changelogPath);
        const section = extractChangelogSection(changelog, v);
        if (!section) continue;

        const md = normalizeChangelogSectionMarkdown(section);
        if (!md.trim()) continue;

        inMemoryCache.set(cacheKey, md);
        return md;
      } catch {
        // Игнорируем: попробуем следующий путь/тег.
      }
    }
  }

  return null;
}

