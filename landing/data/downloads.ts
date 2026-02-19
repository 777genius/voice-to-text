export type DownloadOs = "macos" | "windows" | "linux";
export type DownloadArch = "arm64" | "x64" | "universal";

export const downloadAssets = [
  {
    id: "macos-arm64",
    os: "macos",
    arch: "arm64",
    label: "macOS",
    archLabel: "Apple Silicon",
    url: "https://github.com/777genius/voice-to-text/releases"
  },
  {
    id: "macos-x64",
    os: "macos",
    arch: "x64",
    label: "macOS",
    archLabel: "Intel",
    url: "https://github.com/777genius/voice-to-text/releases"
  },
  {
    id: "windows-x64",
    os: "windows",
    arch: "x64",
    label: "Windows",
    archLabel: "64-bit",
    url: "https://github.com/777genius/voice-to-text/releases"
  },
  {
    id: "linux-appimage",
    os: "linux",
    arch: "x64",
    label: "Linux",
    archLabel: "64-bit",
    url: "https://github.com/777genius/voice-to-text/releases"
  }
] as const;
