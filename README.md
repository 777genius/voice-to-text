# VoicetextAI

Modern voice-to-text desktop application using AI, built with Tauri 2.0, Rust, and Vue 3.

[![GitHub release](https://img.shields.io/github/v/release/777genius/voice-to-text)](https://github.com/777genius/voice-to-text/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

**Website:** [voicetext.site](https://voicetext.site)

## Features

- **45+ Languages** — Full Deepgram Nova-3 support with real-time multilingual detection
- **Real-time Transcription** — Instant partial and final results via WebSocket streaming
- **Global Hotkey** — Quick access with customizable hotkeys (default: Cmd+Shift+X / Ctrl+Shift+X)
- **System Tray** — Runs in background, accessible from tray icon
- **Auto-Updates** — Automatic update checks with secure cryptographic signatures
- **Cross-Platform** — macOS (Intel & Apple Silicon), Windows, Linux
- **Privacy-Focused** — No cloud storage of audio, secure API handling
- **Beautiful UI** — Minimal design with glass morphism effects, dark/light/system themes
- **Multi-window Sync** — State synchronization between app windows
- **OAuth Authentication** — Google sign-in support

## Architecture

This project follows **Clean Architecture** principles with clear separation of concerns.

### Backend (Rust)

```
src-tauri/src/
├── domain/              # Domain Layer — Business Logic
│   ├── models/          # Value Objects & Entities
│   └── ports/           # Interfaces (Traits)
│
├── application/         # Application Layer — Use Cases
│   └── services/        # Transcription orchestration
│
├── infrastructure/      # Infrastructure Layer — Implementations
│   ├── stt/             # STT Providers (Deepgram, AssemblyAI, Whisper)
│   ├── audio/           # Audio Capture (cpal)
│   ├── licensing/       # License validation (Paddle)
│   ├── config_store.rs  # Persistent configuration
│   ├── clipboard.rs     # Clipboard integration
│   ├── auto_paste.rs    # Legacy auto-paste command (disabled in current UI/runtime)
│   ├── updater.rs       # Auto-update logic
│   └── factory.rs       # Provider Factory (DI)
│
└── presentation/        # Presentation Layer — Tauri API
    ├── commands.rs      # Tauri commands
    ├── events.rs        # Event definitions
    ├── state.rs         # Global application state
    └── tray.rs          # System tray integration
```

### Frontend (Vue 3 + TypeScript)

```
src/
├── features/            # Feature modules (DDD)
│   ├── auth/            # Authentication (OAuth, sessions)
│   │   ├── domain/      # Auth entities & interfaces
│   │   ├── application/ # DTOs, ports, services, use cases
│   │   ├── infrastructure/
│   │   ├── presentation/
│   │   └── store/       # Auth Pinia store
│   └── settings/        # Settings management
│       ├── domain/
│       ├── store/
│       └── presentation/
│
├── stores/              # Global Pinia stores
│   ├── transcription.ts # Recording & transcription state
│   ├── sttConfig.ts     # STT configuration sync
│   ├── appConfig.ts     # App configuration sync
│   └── update.ts        # Auto-update state
│
├── windowing/           # Multi-window support
│   └── stateSync/       # Cross-window state synchronization
│
├── presentation/        # Shared UI components
├── composables/         # Vue composables
├── utils/               # Utility functions
│
├── i18n.ts              # Internationalization (6 UI locales)
├── i18n.locales.ts      # STT languages (45+) & locale definitions
└── App.vue
```

### Landing Page (Nuxt 3)

```
landing/
├── components/          # Vue components (sections, layout, ui)
├── pages/               # Routes (index, download, terms, privacy, refund-policy)
├── locales/             # i18n translations (6 languages)
├── content/             # Page content per locale (features, FAQ, pricing)
├── composables/         # Vue composables
├── layouts/             # Page layouts
├── server/              # API routes (downloads, sitemap)
├── api/                 # External API integrations
└── stores/              # Pinia stores
```

## Getting Started

### Prerequisites

- Node.js 18+ and pnpm
- Rust 1.77+
- macOS, Windows, or Linux

### Installation

```bash
# Install dependencies
pnpm install

# Run in development mode
pnpm tauri:dev

# Build for production
pnpm tauri:build

# Run tests
pnpm test
```

### Backend And Deepgram Notes

- Backend repo: `/Users/belief/dev/projects/VoicetextAI/backend`
- Production backend is deployed on Render. Use Render MCP when checking production deploy status, logs, or backend health.
- Deepgram streaming display should follow Deepgram websocket semantics:
  - append every non-empty `is_final=true` transcript segment to the current utterance buffer
  - only move the buffered utterance into final UI text on `speech_final=true`
  - also flush the current utterance when the backend marks an explicit Deepgram `Finalize` result with `from_finalize=true`
  - send websocket control messages as JSON text frames, including `KeepAlive`, `Finalize`, and `CloseStream`
- Real sample audio files useful for manual/e2e transcription checks:
  - `/Users/belief/Documents/2026-05-14 23.56.41.ogg`
  - `/Users/belief/Documents/2026-05-14 23.57.41.ogg`
  - `/Users/belief/Documents/2026-05-14 23.57.56.ogg`

#### Linux System Dependencies

**Debian/Ubuntu:**
```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev \
  libasound2-dev pkg-config
```

**Arch Linux:**
```bash
sudo pacman -S webkit2gtk-4.1 base-devel curl wget file openssl \
  libayatana-appindicator librsvg alsa-lib
```

**Fedora:**
```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file \
  libappindicator-gtk3-devel librsvg2-devel alsa-lib-devel
```

## Platform Support

| Platform | Audio Backend | Installer |
|----------|---------------|-----------|
| macOS Intel | CoreAudio | `.dmg` |
| macOS ARM | CoreAudio | `.dmg` |
| Windows | WASAPI | `.msi` |
| Linux | ALSA/PulseAudio/PipeWire | `.deb`, `.AppImage` |

## Tech Stack

- **Tauri 2.0** — Native desktop framework
- **Rust** — Backend, audio capture, system integration
- **Vue 3** — Reactive UI with Composition API
- **TypeScript** — Type-safe frontend
- **Pinia** — State management
- **Vuetify 3** — Material Design components
- **vue-i18n** — Internationalization
- **Vite** — Fast build tool
- **Nuxt 3** — Landing page SSR
- **Deepgram Nova-3** — Speech-to-text engine

## Scripts

```bash
pnpm dev              # Vite dev server (frontend only)
pnpm build            # Build frontend
pnpm tauri:dev        # Full Tauri development
pnpm tauri:build      # Production build
pnpm test             # Run Vitest tests (watch mode)
pnpm test:run         # Run Vitest tests (single run)
pnpm test:coverage    # Run tests with coverage
pnpm typecheck        # TypeScript type checking
```

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please follow the existing architecture patterns and maintain clean separation of concerns.

## Links

- [Website](https://voicetext.site)
- [GitHub](https://github.com/777genius/voice-to-text)
- [Releases](https://github.com/777genius/voice-to-text/releases)
- [Changelog](CHANGELOG.md)
