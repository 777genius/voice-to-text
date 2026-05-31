import type { PlatformArch, PlatformOs } from "~/types/platform";

type NavigatorWithUserAgentData = Navigator & {
  userAgentData?: {
    platform?: string;
    getHighEntropyValues?: (hints: string[]) => Promise<{
      architecture?: string;
      bitness?: string;
      platform?: string;
    }>;
  };
};

export const getNavigatorPlatformSignature = (navigatorLike: Navigator): string => {
  const nav = navigatorLike as NavigatorWithUserAgentData;

  return [
    nav.userAgent,
    nav.platform,
    nav.userAgentData?.platform,
  ].filter(Boolean).join(" ");
};

export const detectPlatform = (userAgent: string): PlatformOs => {
  const ua = userAgent.toLowerCase();
  if (ua.includes("mac")) return "macos";
  if (ua.includes("win")) return "windows";
  if (ua.includes("linux")) return "linux";
  return "unknown";
};

export const detectArch = (signature: string): PlatformArch => {
  const value = signature.toLowerCase();
  if (/(^|[_\-. ])(arm64|aarch64|arm)([_\-. ]|$)/i.test(value)) return "arm64";
  if (/(^|[_\-. ])(x64|x86_64|amd64|x86|64)([_\-. ]|$)/i.test(value)) return "x64";
  return "unknown";
};

export const detectMacArch = (userAgent: string): PlatformArch => {
  const ua = userAgent.toLowerCase();
  const arch = detectArch(ua);
  if (arch !== "unknown") return arch;

  // Браузеры на Apple Silicon всё равно шлют "Intel Mac OS X" в UA,
  // поэтому проверяем GPU через WebGL - Apple Silicon репортится как "Apple M1/M2/..."
  if (typeof document !== "undefined") {
    try {
      const canvas = document.createElement("canvas");
      const gl = canvas.getContext("webgl2") || canvas.getContext("webgl");
      if (gl) {
        const dbg = gl.getExtension("WEBGL_debug_renderer_info");
        if (dbg) {
          const renderer = gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) as string;
          if (/apple\s*m\d|apple\s*gpu/i.test(renderer)) return "arm64";
        }
      }
    } catch {
      // WebGL недоступен - fallback на x64
    }
  }

  return "x64";
};

export const detectPlatformInfo = async (navigatorLike: Navigator): Promise<{
  os: PlatformOs;
  arch: PlatformArch;
}> => {
  const signature = getNavigatorPlatformSignature(navigatorLike);
  const nav = navigatorLike as NavigatorWithUserAgentData;
  let os = detectPlatform(signature);
  let arch = detectArch(signature);

  try {
    const highEntropy = await nav.userAgentData?.getHighEntropyValues?.([
      "architecture",
      "bitness",
      "platform",
    ]);

    if (highEntropy) {
      const highEntropySignature = [
        highEntropy.platform,
        highEntropy.architecture,
        highEntropy.bitness,
      ].filter(Boolean).join(" ");

      const highEntropyOs = detectPlatform(`${signature} ${highEntropySignature}`);
      const highEntropyArch = detectArch(highEntropySignature);

      if (highEntropyOs !== "unknown") os = highEntropyOs;
      if (highEntropyArch !== "unknown") arch = highEntropyArch;
    }
  } catch {
    // Client Hints могут быть недоступны или запрещены браузером.
  }

  if (os === "macos" && arch === "unknown") {
    arch = detectMacArch(signature);
  }

  if (os !== "unknown" && arch === "unknown") {
    arch = "x64";
  }

  return { os, arch };
};
