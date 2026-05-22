import { computed, onMounted, ref } from "vue";
import { detectMacArch, detectPlatform, getNavigatorPlatformSignature } from "~/utils/platform";

export const usePlatform = () => {
  const platform = ref("unknown");
  const arch = ref("unknown");

  onMounted(() => {
    const signature = getNavigatorPlatformSignature(navigator);
    platform.value = detectPlatform(signature);
    if (platform.value === "macos") {
      arch.value = detectMacArch(signature);
    }
  });

  const label = computed(() => {
    if (platform.value === "macos") return "macOS";
    if (platform.value === "windows") return "Windows";
    if (platform.value === "linux") return "Linux";
    return "OS";
  });

  return { platform, arch, label };
};
