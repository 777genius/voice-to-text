# Voice to Text

Privacy-focused voice-to-text application with offline support, built with Tauri, Rust, and Vue 3.

## Architecture

This project follows **Clean Architecture** principles with clear separation of concerns:

### Backend (Rust)

```
src-tauri/src/
├── domain/              # Domain Layer - Business Logic
│   ├── models/          # Value Objects & Entities
│   │   ├── transcription.rs
│   │   ├── audio_chunk.rs
│   │   └── config.rs
│   └── ports/           # Interfaces (Traits)
│       ├── stt_provider.rs    # STT Provider Interface
│       └── audio_capture.rs   # Audio Capture Interface
│
├── application/         # Application Layer - Use Cases
│   └── services/
│       └── transcription_service.rs  # Main orchestration service
│
├── infrastructure/      # Infrastructure Layer - Implementations
│   ├── stt/             # STT Provider Implementations
│   │   ├── mock.rs      # Mock provider for testing
│   │   ├── deepgram.rs  # Deepgram cloud provider (stub)
│   │   └── whisper_local.rs  # Local Whisper.cpp (stub)
│   ├── audio/           # Audio Capture Implementations
│   │   └── mock_capture.rs   # Mock audio for testing
│   └── factory.rs       # Provider Factory (DI)
│
└── presentation/        # Presentation Layer - Tauri API
    ├── commands.rs      # Tauri commands
    ├── events.rs        # Event definitions
    └── state.rs         # Global application state
```

### Frontend (Vue 3 + TypeScript)

```
src/
├── types/               # TypeScript type definitions
│   └── index.ts
├── stores/              # Pinia stores
│   └── transcription.ts
├── presentation/        # UI Components
│   └── components/
│       └── RecordingPopover.vue
├── assets/
│   └── style.css
├── main.ts
└── App.vue
```

## Key Design Patterns

### SOLID Principles

- **Single Responsibility**: Each module has one clear purpose
- **Open/Closed**: Extensible through interfaces, closed for modification
- **Liskov Substitution**: STT providers are interchangeable
- **Interface Segregation**: Focused, minimal interfaces
- **Dependency Inversion**: Domain depends on abstractions, not implementations

### Patterns Used

1. **Repository Pattern**: Audio capture and STT providers abstract data sources
2. **Factory Pattern**: `SttProviderFactory` creates providers based on configuration
3. **Observer Pattern**: Event-driven communication between Rust and Vue
4. **Strategy Pattern**: Switchable STT provider strategies

## Features

- **System Tray Integration**: Runs in background, accessible from tray icon
- **Global Hotkey**: Quick access with customizable hotkeys (default: Cmd+Shift+X / Ctrl+Shift+X)
- **Auto-Updates**: Automatic update checks every 6 hours with secure cryptographic signatures
- **Real-time Transcription**: Partial and final results from cloud providers
- **Auto-copy to Clipboard**: Instant access to transcribed text
- **Multiple STT Providers**:
  - **Deepgram** (Nova-2/3, low latency, high quality) ✅
  - **AssemblyAI** (Universal-Streaming v3) ✅
  - Whisper.cpp (offline, stub) 🚧
- **Cross-Platform**: macOS, Windows, Linux support
- **Privacy-Focused**: API keys from environment variables, no cloud storage
- **Beautiful UI**: Minimal design with glass morphism effects

## Getting Started

### Prerequisites

- Node.js 18+ and pnpm
- Rust 1.77+
- macOS, Windows, or Linux

### Installation

#### Linux System Dependencies

On Linux, you'll need to install some system libraries:

**Debian/Ubuntu:**
```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libasound2-dev \
  pkg-config
```

**Arch Linux:**
```bash
sudo pacman -S webkit2gtk-4.1 \
  base-devel \
  curl \
  wget \
  file \
  openssl \
  libayatana-appindicator \
  librsvg \
  alsa-lib
```

**Fedora:**
```bash
sudo dnf install webkit2gtk4.1-devel \
  openssl-devel \
  curl \
  wget \
  file \
  libappindicator-gtk3-devel \
  librsvg2-devel \
  alsa-lib-devel
```

#### Build and Run

```bash
# Install dependencies
pnpm install

# Run in development mode
pnpm tauri:dev

# Build for production (creates .deb, .appimage on Linux)
pnpm tauri:build
```

## Development Status

### Completed ✅
- Clean Architecture foundation
- Domain layer with interfaces and value objects
- Real Deepgram provider implementation (Nova-2/3)
- Real AssemblyAI provider implementation
- System audio capture (cpal with automatic resampling)
- Global hotkey registration
- Settings UI with microphone test
- **System Tray integration**
- **Background mode (always running)**
- **Auto-updates via GitHub Releases**
- **Cross-platform support (macOS, Windows, Linux)**
- WebSocket streaming for real-time transcription
- VAD (Voice Activity Detection) with silence timeout
- Transcription history
- Multi-language support

### TODO 📋
- Whisper.cpp local implementation (currently stub)
- Transcription history UI
- Auto-start on system boot
- More STT provider integrations

## Platform Support

### All Platforms ✅
- **Audio Capture**: cpal library with automatic resampling (16kHz mono)
  - macOS: CoreAudio
  - Windows: WASAPI
  - Linux: ALSA/PulseAudio/PipeWire
- **Hotkeys**: Global hotkey registration (Tauri plugin)
- **Clipboard**: Auto-copy to system clipboard
- **WebSocket Streaming**: Real-time STT with Deepgram, AssemblyAI
- **Configuration**: Persistent settings storage

### Platform-Specific Notes

**macOS:**
- Uses CoreAudio for low-latency audio capture
- Transparent window with proper backdrop effects
- Default hotkey: Cmd+Shift+X

**Windows:**
- Uses WASAPI for audio capture
- NSIS installer with Russian/English support
- Default hotkey: Ctrl+Shift+X

**Linux:**
- Uses ALSA/PulseAudio for audio capture
- Builds .deb (Debian/Ubuntu) and .appimage (universal)
- Default hotkey: Ctrl+Shift+X
- Requires system libraries (see Installation section)

## Architecture Benefits

1. **Testability**: Mock implementations allow testing without external dependencies
2. **Maintainability**: Clear boundaries between layers
3. **Extensibility**: Easy to add new STT providers
4. **Flexibility**: Switch between online/offline modes
5. **Independence**: Domain logic independent of frameworks
6. **Cross-platform**: Single codebase for macOS, Windows, and Linux

## Technologies

- **Tauri 2.0**: Native desktop app framework
- **Rust**: Backend and system integration
- **Vue 3**: Reactive UI framework
- **TypeScript**: Type-safe frontend code
- **Pinia**: State management
- **Vite**: Fast build tool

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please follow the existing architecture patterns and maintain clean separation of concerns.
