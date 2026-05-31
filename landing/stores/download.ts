import { defineStore } from "pinia";
import { downloadAssets } from "~/data/downloads";
import type { DownloadArch, DownloadOs } from "~/data/downloads";
import { detectPlatformInfo } from "~/utils/platform";

export const useDownloadStore = defineStore("download", {
  state: () => ({
    os: "unknown" as DownloadOs | "unknown",
    arch: "unknown" as DownloadArch | "unknown",
    selectedId: ""
  }),
  getters: {
    assets: () => downloadAssets,
    selectedAsset(state) {
      return downloadAssets.find((asset) => asset.id === state.selectedId);
    },
    isMacOs(state): boolean {
      return state.os === "macos";
    },
    macArch(state): "arm64" | "x64" {
      return state.arch === "arm64" ? "arm64" : "x64";
    }
  },
  actions: {
    async init() {
      if (!import.meta.client) return;
      const { os, arch } = await detectPlatformInfo(window.navigator);
      this.os = os;
      if (this.os === "macos") {
        this.arch = (arch === "arm64" ? "arm64" : "x64") as DownloadArch;
      } else if (this.os !== "unknown") {
        this.arch = "x64";
      } else {
        this.arch = "unknown";
      }

      // Для macOS - одна карточка, матчим по OS
      const match = downloadAssets.find((asset) => asset.os === this.os);
      if (match) {
        this.selectedId = match.id;
      }
    },
    setSelected(id: string) {
      this.selectedId = id;
    }
  }
});
