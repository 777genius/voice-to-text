import { createApp } from 'vue';
import { createPinia } from 'pinia';
import { i18n } from './i18n';
import vuetify from './plugins/vuetify';
import App from './App.vue';
import './assets/style.css';
import { installE2eHooks } from './e2e/e2eHooks';

const pinia = createPinia();
const app = createApp(App);

const storedTheme = localStorage.getItem('uiTheme') || 'dark';
if (storedTheme === 'light') {
  document.documentElement.classList.add('theme-light');
}

const isMacOS = navigator.platform.toUpperCase().includes('MAC');
if (isMacOS) {
  document.documentElement.classList.add('os-macos');
}

app.use(pinia);
app.use(i18n);
app.use(vuetify);

// Для e2e: даём WebDriver тестам безопасный доступ к состоянию store'ов.
installE2eHooks(pinia);

app.mount('#app');
