# Setup Instructions

## Prerequisites

Before you can build and run this application, you need to install the following:

### 1. Rust

```bash
# Install Rust using rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# After installation, reload your shell or run:
source $HOME/.cargo/env

# Verify installation
rustc --version
cargo --version
```

### 2. System Dependencies (macOS)

```bash
# Install Xcode Command Line Tools (if not already installed)
xcode-select --install

# Verify
xcode-select -p
```

### 3. Node.js and pnpm

Node.js and pnpm should already be installed based on the project setup.

```bash
# Verify
node --version
pnpm --version
```

## Building the Application

Once all prerequisites are installed:

```bash
# From project root directory

# Install JavaScript dependencies (if not already done)
pnpm install

# Build in debug mode (faster, includes debug symbols)
pnpm tauri build --debug

# Or build for production (optimized)
pnpm tauri build
```

## Running in Development Mode

```bash
# Start development server with hot reload
pnpm tauri:dev
```

This will:
1. Start Vite dev server for Vue 3 frontend
2. Compile Rust backend
3. Launch the application with live reload

## Project Structure Overview

```
voice_to_text/
├── src/                    # Vue 3 frontend
│   ├── main.ts            # Entry point
│   ├── App.vue            # Root component
│   ├── types/             # TypeScript definitions
│   ├── stores/            # Pinia stores
│   └── presentation/      # UI components
│       └── components/
│           └── RecordingPopover.vue
│
├── src-tauri/             # Rust backend
│   ├── src/
│   │   ├── lib.rs        # Library entry point
│   │   ├── main.rs       # Application entry point
│   │   ├── domain/       # Business logic & interfaces
│   │   ├── application/  # Use cases & services
│   │   ├── infrastructure/ # Implementations
│   │   └── presentation/ # Tauri commands & state
│   ├── Cargo.toml        # Rust dependencies
│   ├── tauri.conf.json   # Tauri configuration
│   └── capabilities/     # Permission definitions
│
├── package.json          # Node.js configuration
├── vite.config.ts        # Vite configuration
├── tsconfig.json         # TypeScript configuration
└── README.md             # Project documentation
```

## Next Steps

After successful build, you can:

1. **Test the mock implementation**: The app currently uses mock STT provider for testing
2. **Add real STT providers**: Implement Deepgram or Whisper.cpp integrations
3. **Add real audio capture**: Integrate cpal for system microphone access
4. **Implement VAD**: Add WebRTC VAD for automatic silence detection
5. **Configure global hotkey**: Set up system-wide hotkey registration

## Troubleshooting

### "cargo not found"
- Make sure Rust is installed: `rustc --version`
- Reload your shell after installing Rust

### "No such file or directory (os error 2)"
- This usually means Rust/Cargo is not in your PATH
- Run: `source $HOME/.cargo/env`

### Compilation errors
- Make sure you have the latest Rust: `rustup update`
- Clear Cargo cache: `cd src-tauri && cargo clean`

### Permission errors on macOS
- Grant microphone permission in System Settings
- For global hotkey, you may need Accessibility permissions

## Development Tips

1. **Use mock providers first**: Test the architecture without external dependencies
2. **Check logs**: Use `log::info!` in Rust and `console.log` in Vue
3. **Hot reload**: Changes to Vue code reload instantly; Rust changes require recompilation
4. **Test incrementally**: Build features one layer at a time

## Architecture Validation

To verify the architecture is properly set up:

1. ✅ Domain layer has no external dependencies
2. ✅ Application layer depends only on domain
3. ✅ Infrastructure implements domain interfaces
4. ✅ Presentation layer uses application services
5. ✅ Easy to swap STT providers via factory pattern

## Ready to Code!

The foundation is solid. You can now:
- Add real implementations
- Extend with new features
- Test independently
- Deploy with confidence

The Clean Architecture ensures your code remains maintainable and testable as it grows.
