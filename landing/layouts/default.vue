<template>
  <v-app class="app-layout">
    <AppHeader />
    <v-main class="app-layout__main">
      <SupportedLanguages />
      <div>
        <slot />
      </div>
    </v-main>
    <AppFooter />
  </v-app>
</template>

<style scoped>
.app-layout__main {
  flex: 1;
}
</style>
