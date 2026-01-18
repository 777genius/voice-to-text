import { createApp } from 'vue';
import { createPinia } from 'pinia';
import { i18n } from './i18n';
import App from './App.vue';
import './assets/style.css';

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

const storedFontSize = Number(localStorage.getItem('uiFontSize') || 14);
if (!Number.isNaN(storedFontSize)) {
  document.documentElement.style.setProperty('--transcription-font-size', `${storedFontSize}px`);
}

app.use(pinia);
app.use(i18n);
app.mount('#app');
