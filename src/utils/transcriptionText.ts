const ZERO_WIDTH_RE = /[\u200B-\u200D\uFEFF]/g;
const PUNCT_RE = /[.,!?;:"'`(){}\[\]<>\-–—_\\/|@$%^&*=~]/g;

function normalizeSpaces(text: string): string {
  return String(text ?? '').replace(/\s+/g, ' ').trim();
}

function splitWords(text: string): string[] {
  const t = normalizeSpaces(text);
  return t ? t.split(' ') : [];
}

function normalizeWord(word: string): string {
  return String(word ?? '')
    .toLowerCase()
    .replace(ZERO_WIDTH_RE, '')
    .replace(PUNCT_RE, '')
    .trim();
}

function normalizeForCompare(text: string): string {
  const words = splitWords(text);
  const out: string[] = [];
  for (const w of words) {
    const n = normalizeWord(w);
    if (n) out.push(n);
  }
  return out.join(' ');
}

function tokenize(text: string): { raw: string[]; norm: string[] } {
  const rawWords = splitWords(text);
  const raw: string[] = [];
  const norm: string[] = [];
  for (const w of rawWords) {
    const n = normalizeWord(w);
    if (!n) continue;
    raw.push(w);
    norm.push(n);
  }
  return { raw, norm };
}

function shouldAcceptSingleWordOverlap(word: string): boolean {
  const w = String(word ?? '').trim();
  return w.length >= 6;
}

function computeOverlapWords(aNorm: string[], bNorm: string[]): number {
  const max = Math.min(aNorm.length, bNorm.length);
  for (let k = max; k >= 1; k--) {
    let ok = true;
    for (let i = 0; i < k; i++) {
      if (aNorm[aNorm.length - k + i] !== bNorm[i]) {
        ok = false;
        break;
      }
    }
    if (ok) return k;
  }
  return 0;
}

function joinWithSpace(a: string, b: string): string {
  const left = normalizeSpaces(a);
  const right = normalizeSpaces(b);
  if (!left) return right;
  if (!right) return left;
  return `${left} ${right}`.trim();
}

export function appendTranscriptText(base: string, next: string): string {
  return joinWithSpace(base, next);
}

export function mergeTranscriptText(base: string, next: string): string {
  const a = normalizeSpaces(base);
  const b = normalizeSpaces(next);

  if (!a) return b;
  if (!b) return a;
  if (a === b) return a;

  const an = normalizeForCompare(a);
  const bn = normalizeForCompare(b);

  if (an && bn) {
    if (an === bn) return a.length >= b.length ? a : b;
    const anWords = an.split(' ').filter(Boolean);
    const bnWords = bn.split(' ').filter(Boolean);
    const canUseContainment =
      (anWords.length >= 3 && bnWords.length >= 3) || (an.length >= 18 && bn.length >= 18);
    if (canUseContainment) {
      if (bn.includes(an) && bn.length >= an.length) return b;
      if (an.includes(bn) && an.length >= bn.length) return a;
    }
  }

  const A = tokenize(a);
  const B = tokenize(b);
  const overlap = computeOverlapWords(A.norm, B.norm);
  if (overlap >= 2 || (overlap === 1 && shouldAcceptSingleWordOverlap(B.norm[0]))) {
    const suffix = B.raw.slice(overlap).join(' ');
    return suffix ? joinWithSpace(a, suffix) : a;
  }

  return joinWithSpace(a, b);
}
