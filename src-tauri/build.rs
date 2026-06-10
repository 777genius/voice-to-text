use std::env;
use std::fs;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
const SWIFT_CONCURRENCY_DYLIB: &str = "libswift_Concurrency.dylib";

fn main() {
    // Сначала запускаем стандартный билд Tauri
    tauri_build::build();

    #[cfg(target_os = "macos")]
    {
        prepare_swift_runtime_for_cargo();
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg-bin=voice-to-text=-Wl,-rpath,@executable_path/Frameworks");
        println!(
            "cargo:rustc-link-arg-bin=voice-to-text=-Wl,-rpath,@executable_path/../Frameworks"
        );
    }

    // Загружаем .env файл если он существует
    if let Err(e) = dotenv::dotenv() {
        println!("cargo:warning=No .env file found: {}", e);
    }

    // Читаем API ключи из переменных окружения
    let deepgram_key = env::var("DEEPGRAM_API_KEY").unwrap_or_else(|_| {
        println!("cargo:warning=DEEPGRAM_API_KEY not found in environment");
        String::new()
    });

    let assemblyai_key = env::var("ASSEMBLYAI_API_KEY").unwrap_or_else(|_| {
        println!("cargo:warning=ASSEMBLYAI_API_KEY not found in environment");
        String::new()
    });

    // Генерируем Rust код с встроенными ключами
    let embedded_keys_code = format!(
        r#"// Этот файл сгенерирован автоматически build.rs
// НЕ РЕДАКТИРУЙТЕ ВРУЧНУЮ

/// Встроенный API ключ для Deepgram
pub const EMBEDDED_DEEPGRAM_KEY: &str = "{}";

/// Встроенный API ключ для AssemblyAI
pub const EMBEDDED_ASSEMBLYAI_KEY: &str = "{}";

/// Проверяет есть ли встроенный ключ для Deepgram
pub fn has_embedded_deepgram_key() -> bool {{
    !EMBEDDED_DEEPGRAM_KEY.is_empty()
}}

/// Проверяет есть ли встроенный ключ для AssemblyAI
pub fn has_embedded_assemblyai_key() -> bool {{
    !EMBEDDED_ASSEMBLYAI_KEY.is_empty()
}}
"#,
        deepgram_key, assemblyai_key
    );

    // Путь к генерируемому файлу
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let dest_path = out_dir.join("embedded_keys.rs");

    // Записываем сгенерированный код
    fs::write(&dest_path, embedded_keys_code).expect("Failed to write embedded_keys.rs");

    println!("cargo:rerun-if-changed=../.env");
    println!("cargo:rerun-if-env-changed=DEEPGRAM_API_KEY");
    println!("cargo:rerun-if-env-changed=ASSEMBLYAI_API_KEY");

    println!("✅ Generated embedded_keys.rs with API keys");
}

#[cfg(target_os = "macos")]
fn prepare_swift_runtime_for_cargo() {
    let Some(source_path) = find_swift_concurrency_runtime() else {
        panic!(
            "Could not find {}. Install or select Xcode with xcode-select.",
            SWIFT_CONCURRENCY_DYLIB
        );
    };

    let Some(profile_dir) = cargo_profile_dir() else {
        panic!("Could not resolve Cargo profile directory from OUT_DIR.");
    };

    let frameworks_dir = profile_dir.join("Frameworks");
    copy_swift_runtime(&source_path, &frameworks_dir);

    if let Some(target_dir) = profile_dir.parent() {
        copy_swift_runtime(&source_path, &target_dir.join("swift-runtime"));
    }
}

#[cfg(target_os = "macos")]
fn copy_swift_runtime(source_path: &Path, dest_dir: &Path) {
    let dest_path = dest_dir.join(SWIFT_CONCURRENCY_DYLIB);
    fs::create_dir_all(dest_dir).expect("Failed to create Swift runtime destination directory");
    fs::copy(source_path, &dest_path).expect("Failed to copy Swift Concurrency runtime");
    println!(
        "cargo:warning=Copied Swift Concurrency runtime: {} -> {}",
        source_path.display(),
        dest_path.display()
    );
}

#[cfg(target_os = "macos")]
fn cargo_profile_dir() -> Option<PathBuf> {
    let mut path = PathBuf::from(env::var_os("OUT_DIR")?);
    for _ in 0..3 {
        path.pop();
    }
    Some(path)
}

#[cfg(target_os = "macos")]
fn find_swift_concurrency_runtime() -> Option<PathBuf> {
    swift_runtime_candidates()
        .into_iter()
        .find(|candidate| candidate.exists())
}

#[cfg(target_os = "macos")]
fn swift_runtime_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(output) = std::process::Command::new("xcrun")
        .args(["--find", "swift"])
        .output()
    {
        if output.status.success() {
            if let Ok(swift_bin) = String::from_utf8(output.stdout) {
                let swift_bin = swift_bin.trim();
                if !swift_bin.is_empty() {
                    if let Some(toolchain_usr) =
                        Path::new(swift_bin).parent().and_then(Path::parent)
                    {
                        let lib_root = toolchain_usr.join("lib");
                        for dir_name in ["swift-5.5", "swift"] {
                            candidates.push(
                                lib_root
                                    .join(dir_name)
                                    .join("macosx")
                                    .join(SWIFT_CONCURRENCY_DYLIB),
                            );
                            candidates.push(lib_root.join(dir_name).join(SWIFT_CONCURRENCY_DYLIB));
                        }

                        if let Ok(entries) = fs::read_dir(&lib_root) {
                            for entry in entries.flatten() {
                                let Ok(file_type) = entry.file_type() else {
                                    continue;
                                };
                                if !file_type.is_dir() {
                                    continue;
                                }
                                let name = entry.file_name();
                                let name = name.to_string_lossy();
                                if !name.starts_with("swift") {
                                    continue;
                                }
                                candidates.push(
                                    lib_root
                                        .join(name.as_ref())
                                        .join("macosx")
                                        .join(SWIFT_CONCURRENCY_DYLIB),
                                );
                                candidates.push(
                                    lib_root.join(name.as_ref()).join(SWIFT_CONCURRENCY_DYLIB),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    candidates.extend([
        PathBuf::from(format!(
            "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx/{}",
            SWIFT_CONCURRENCY_DYLIB
        )),
        PathBuf::from(format!(
            "/Library/Developer/CommandLineTools/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx/{}",
            SWIFT_CONCURRENCY_DYLIB
        )),
        PathBuf::from(format!("/usr/lib/swift/{}", SWIFT_CONCURRENCY_DYLIB)),
    ]);

    candidates
}
