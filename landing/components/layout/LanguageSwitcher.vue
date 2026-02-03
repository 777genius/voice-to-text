<script setup lang="ts">
import { supportedLocales } from "~/data/i18n";
import type { LocaleCode } from "~/data/i18n";
import { useLocaleStore } from "~/stores/locale";

const { t, locale } = useI18n();
const nuxtApp = useNuxtApp();
const switchLocalePath = useSwitchLocalePath();
const props = defineProps<{ fullWidth?: boolean; compact?: boolean; iconOnly?: boolean }>();
const localeStore = useLocaleStore();

const flagIconMap: Record<string, string> = {
  en: "circle-flags:us",
  ru: "circle-flags:ru",
  es: "circle-flags:es",
  fr: "circle-flags:fr",
  de: "circle-flags:de",
  uk: "circle-flags:ua"
};

const items = computed(() =>
  supportedLocales.map((item) => ({
    title: item.name,
    value: item.code as LocaleCode,
    flagIcon: flagIconMap[item.code] ?? "circle-flags:xx"
  }))
);

const currentFlagIcon = computed(() => {
  return flagIconMap[locale.value as string] ?? "circle-flags:xx";
});

const onChange = async (value: string | LocaleCode) => {
  const nextLocale = value as LocaleCode;
  localeStore.setLocale(nextLocale, true);
  if (nuxtApp.$i18n?.setLocale) {
    await nuxtApp.$i18n.setLocale(nextLocale);
  } else {
    locale.value = nextLocale;
  }
  const path = switchLocalePath(nextLocale);
  if (path) {
    await navigateTo(path);
  }
};
</script>

<template>
  <!-- Icon-only mode -->
  <v-menu v-if="props.iconOnly" location="bottom" :close-on-content-click="false">
    <template #activator="{ props: menuProps }">
      <v-btn variant="text" v-bind="menuProps" :aria-label="t('language.label')">
        <Icon :name="currentFlagIcon" class="language-switcher__flag-icon" />
      </v-btn>
    </template>
    <v-autocomplete
      :items="items"
      :model-value="locale"
      density="compact"
      variant="solo-filled"
      hide-details
      auto-select-first
      class="language-switcher__menu-autocomplete"
      @update:model-value="onChange"
    >
      <template #selection>
        <Icon :name="currentFlagIcon" class="language-switcher__flag-icon" />
      </template>
      <template #item="{ item, props: itemProps }">
        <v-list-item v-bind="itemProps">
          <template #title>
            <span class="language-switcher__item">
              <Icon :name="item.raw.flagIcon" class="language-switcher__flag-icon" />
              <span>{{ item.raw.title }}</span>
            </span>
          </template>
        </v-list-item>
      </template>
    </v-autocomplete>
  </v-menu>

  <!-- Standard mode with search -->
  <v-autocomplete
    v-else
    :label="props.compact ? undefined : t('language.label')"
    :placeholder="props.compact ? t('language.label') : undefined"
    :items="items"
    :model-value="locale"
    density="compact"
    :variant="props.compact ? 'plain' : 'outlined'"
    hide-details
    auto-select-first
    @update:model-value="onChange"
    :style="props.fullWidth ? { maxWidth: '100%', width: '100%' } : { maxWidth: '220px' }"
    :class="{
      'language-switcher--full': props.fullWidth,
      'language-switcher--compact': props.compact
    }"
    :aria-label="t('language.label')"
    :single-line="props.compact"
  >
    <template #selection>
      <Icon :name="currentFlagIcon" class="language-switcher__flag-icon" />
    </template>
    <template #item="{ item, props: itemProps }">
      <v-list-item v-bind="itemProps">
        <template #title>
          <span class="language-switcher__item">
            <Icon :name="item.raw.flagIcon" class="language-switcher__flag-icon" />
            <span>{{ item.raw.title }}</span>
          </span>
        </template>
      </v-list-item>
    </template>
  </v-autocomplete>
</template>

<style scoped>
.language-switcher__flag-icon {
  width: 22px;
  height: 22px;
  flex-shrink: 0;
  border-radius: 50%;
}

.language-switcher__item {
  display: flex;
  align-items: center;
  gap: 8px;
}

.language-switcher--compact :deep(.v-field) {
  min-height: 36px;
}

.language-switcher--compact :deep(.v-field__input) {
  padding-top: 6px;
  padding-bottom: 6px;
  min-height: 36px;
}

.language-switcher--compact {
  min-width: 60px;
  position: relative;
  z-index: 2;
}

.language-switcher--compact :deep(.v-field__outline) {
  display: none;
}

.language-switcher--compact :deep(.v-field__overlay) {
  background-color: transparent;
}

.language-switcher__menu-autocomplete {
  min-width: 220px;
}
</style>
