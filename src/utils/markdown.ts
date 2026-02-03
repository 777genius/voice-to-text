function escapeHtml(input: string): string {
  return input
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}

function escapeHtmlAttr(input: string): string {
  // Нам достаточно закрыть кавычки и угловые скобки + амперсанд.
  return escapeHtml(input);
}

function linkify(text: string): string {
  // Простейшие markdown-ссылки: [text](url)
  // URL оставляем только http/https чтобы не получить javascript:.
  return text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, label: string, url: string) => {
    const safeLabel = escapeHtml(String(label));
    const rawUrl = String(url).trim();
    if (!/^https?:\/\//i.test(rawUrl)) return safeLabel;
    const safeUrl = escapeHtmlAttr(rawUrl);
    return `<a href="${safeUrl}" target="_blank" rel="noopener noreferrer">${safeLabel}</a>`;
  });
}

function renderInlineMarkdown(text: string): string {
  // Порядок важен: сначала экранируем весь текст, потом разрешаем немного разметки.
  let s = escapeHtml(text);

  // Inline code: `code`
  s = s.replace(/`([^`]+)`/g, (_m, code: string) => `<code>${code}</code>`);

  // Bold: **text**
  s = s.replace(/\*\*([^*]+)\*\*/g, (_m, bold: string) => `<strong>${bold}</strong>`);

  // Links
  s = linkify(s);

  return s;
}

/**
 * Минимальный безопасный markdown→HTML для отображения релиз-ноутов в диалоге обновления.
 * Поддержка:
 * - ### заголовки
 * - списки "- "
 * - inline code `...`, bold **...**, ссылки [t](https://...)
 *
 * Не поддерживаем HTML внутри markdown — всё экранируем.
 */
export function renderMarkdownToSafeHtml(markdown: string): string {
  const lines = String(markdown ?? '').split(/\r?\n/);
  const out: string[] = [];

  let inList = false;
  const closeListIfNeeded = () => {
    if (inList) {
      out.push('</ul>');
      inList = false;
    }
  };

  for (const raw of lines) {
    const line = raw.trimEnd();
    const trimmed = line.trim();

    if (!trimmed) {
      closeListIfNeeded();
      out.push('<br/>');
      continue;
    }

    if (trimmed === '---') {
      closeListIfNeeded();
      continue;
    }

    if (trimmed.startsWith('### ')) {
      closeListIfNeeded();
      const title = trimmed.slice(4).trim();
      out.push(`<div class="md-h3">${renderInlineMarkdown(title)}</div>`);
      continue;
    }

    // список "- item"
    if (/^-\s+/.test(trimmed)) {
      if (!inList) {
        out.push('<ul class="md-ul">');
        inList = true;
      }
      const item = trimmed.replace(/^-+\s+/, '');
      out.push(`<li class="md-li">${renderInlineMarkdown(item)}</li>`);
      continue;
    }

    closeListIfNeeded();
    out.push(`<div class="md-p">${renderInlineMarkdown(trimmed)}</div>`);
  }

  closeListIfNeeded();

  // Убираем лишние <br/> в начале/конце
  while (out[0] === '<br/>') out.shift();
  while (out[out.length - 1] === '<br/>') out.pop();

  return out.join('\n');
}

