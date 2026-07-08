import { open } from '@tauri-apps/plugin-shell';
import type { IUrlOpener } from '../../application/ports/IUrlOpener';

function normalizeHostname(hostname: string): string {
  return hostname.toLowerCase().replace(/^\[|\]$/g, '');
}

function isLoopbackIpv4(hostname: string): boolean {
  const parts = hostname.split('.');
  if (parts.length !== 4 || parts[0] !== '127') {
    return false;
  }

  return parts.every(part => {
    if (!/^\d+$/.test(part)) {
      return false;
    }

    const octet = Number(part);
    return octet >= 0 && octet <= 255;
  });
}

function isLoopbackHostname(hostname: string): boolean {
  const normalized = normalizeHostname(hostname);
  return normalized === 'localhost' || normalized === '::1' || isLoopbackIpv4(normalized);
}

export function assertSafeExternalAuthUrl(rawUrl: string): string {
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    throw new Error('Invalid auth URL');
  }

  if (parsed.username || parsed.password) {
    throw new Error('Unsafe auth URL credentials');
  }

  if (parsed.protocol === 'https:') {
    return parsed.toString();
  }

  if (parsed.protocol === 'http:' && isLoopbackHostname(parsed.hostname)) {
    return parsed.toString();
  }

  throw new Error('Blocked unsafe auth URL');
}

export class TauriUrlOpener implements IUrlOpener {
  async open(url: string): Promise<void> {
    await open(assertSafeExternalAuthUrl(url));
  }
}
