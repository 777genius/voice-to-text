import { computed, onMounted, ref } from "vue";
import { detectPlatformInfo } from "~/utils/platform";
import type { PlatformArch, PlatformOs } from "~/types/platform";

export const usePlatform = () => {
  const platform = ref<PlatformOs>("unknown");
  const arch = ref<PlatformArch>("unknown");

  onMounted(async () => {
    const detected = await detectPlatformInfo(window.navigator);
    platform.value = detected.os;
    arch.value = detected.arch;
  });

  const label = computed(() => {
    if (platform.value === "macos") return "macOS";
    if (platform.value === "windows") return "Windows";
    if (platform.value === "linux") return "Linux";
    return "OS";
  });

  return { platform, arch, label };
};
