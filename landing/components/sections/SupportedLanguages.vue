<script setup lang="ts">
const { languages, loading } = useSupportedLanguages();
</script>

<template>
  <div v-if="!loading && languages.length > 0" class="supported-languages">
    <v-container class="supported-languages__container">
      <div class="supported-languages__flags">
        <img
          v-for="lang in languages"
          :key="lang.code"
          :src="`https://flagcdn.com/${lang.country_code}.svg`"
          :alt="lang.name"
          :title="lang.name"
          class="supported-languages__flag"
          loading="lazy"
          width="20"
          height="15"
        />
      </div>
    </v-container>
  </div>
</template>

<style scoped>
.supported-languages {
  padding: 8px 0;
}

.supported-languages__container {
  display: flex;
  align-items: center;
  justify-content: center;
}

.supported-languages__flags {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
  justify-content: center;
}

.supported-languages__flag {
  width: 24px;
  height: 18px;
  border-radius: 3px;
  object-fit: cover;
  flex-shrink: 0;
}

@media (max-width: 600px) {
  .supported-languages__flags {
    gap: 4px;
  }

  .supported-languages__flag {
    width: 20px;
    height: 15px;
  }
}
</style>
