const CHANGELOG_FALLBACK_RE = /^see\s+changelog\s+for\s+details\.?$/i;
const INSTALLATION_HEADING_RE = /^(#{2,6})\s*(installation|установка)\s*:?\s*$/i;
const MARKDOWN_HEADING_RE = /^(#{1,6})\s+\S/;

function markdownHeadingDepth(line: string): number | null {
  const match = line.trim().match(/^(#{1,6})\s+/);
  return match ? match[1].length : null;
}

export function normalizeAppUpdateNotes(markdown: string): string {
  const lines = String(markdown ?? '').split(/\r?\n/);
  const out: string[] = [];
  let skipUntilDepth: number | null = null;

  for (const raw of lines) {
    const trimmed = raw.trim();

    if (skipUntilDepth !== null) {
      const depth = markdownHeadingDepth(raw);
      if (depth !== null && depth <= skipUntilDepth) {
        skipUntilDepth = null;
      } else {
        continue;
      }
    }

    const installationMatch = trimmed.match(INSTALLATION_HEADING_RE);
    if (installationMatch) {
      skipUntilDepth = installationMatch[1].length;
      continue;
    }

    if (CHANGELOG_FALLBACK_RE.test(trimmed)) {
      continue;
    }

    // GitHub release body sometimes uses top-level headings. In-app notes are embedded inside
    // a dialog, so avoid oversized Markdown title hierarchy.
    if (MARKDOWN_HEADING_RE.test(trimmed)) {
      out.push(raw.replace(/^#{1,6}\s+/, '### '));
      continue;
    }

    out.push(raw);
  }

  return out.join('\n').trim();
}
