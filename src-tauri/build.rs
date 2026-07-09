use std::env;
use std::fs;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
const SWIFT_CONCURRENCY_DYLIB: &str = "libswift_Concurrency.dylib";
const BUILD_VERBOSE_ENV: &str = "VOICETEXT_BUILD_VERBOSE";
#[cfg(target_os = "macos")]
const COPY_SWIFT_RUNTIME_TO_CARGO_PROFILE_ENV: &str =
    "VOICETEXT_COPY_SWIFT_RUNTIME_TO_CARGO_PROFILE";

fn main() {
    // Сначала запускаем стандартный билд Tauri
    tauri_build::build();

    #[cfg(target_os = "macos")]
    {
        prepare_swift_runtime_for_cargo();
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg-bin=voice-to-text=-Wl,-rpath,/usr/lib/swift");
        println!("cargo:rustc-link-arg-bin=voice-to-text=-Wl,-rpath,@executable_path/Frameworks");
        println!(
            "cargo:rustc-link-arg-bin=voice-to-text=-Wl,-rpath,@executable_path/../Frameworks"
        );
    }

    // Загружаем .env файл если он существует
    let _ = dotenv::dotenv();

    // Читаем API ключи из переменных окружения
    let deepgram_key = env::var("DEEPGRAM_API_KEY").unwrap_or_else(|_| String::new());

    let assemblyai_key = env::var("ASSEMBLYAI_API_KEY").unwrap_or_else(|_| String::new());

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
    #[cfg(target_os = "macos")]
    println!(
        "cargo:rerun-if-env-changed={}",
        COPY_SWIFT_RUNTIME_TO_CARGO_PROFILE_ENV
    );

    if is_build_verbose() {
        println!("cargo:warning=Generated embedded_keys.rs");
    }
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
    if should_copy_swift_runtime_to_cargo_profile() {
        copy_swift_runtime(&source_path, &frameworks_dir);
    } else {
        remove_profile_swift_runtime_copy(&frameworks_dir);
    }

    if let Some(target_dir) = profile_dir.parent() {
        copy_swift_runtime(&source_path, &target_dir.join("swift-runtime"));
    }
}

#[cfg(target_os = "macos")]
fn copy_swift_runtime(source_path: &Path, dest_dir: &Path) {
    let dest_path = dest_dir.join(SWIFT_CONCURRENCY_DYLIB);
    fs::create_dir_all(dest_dir).expect("Failed to create Swift runtime destination directory");
    fs::copy(source_path, &dest_path).expect("Failed to copy Swift Concurrency runtime");
    if is_build_verbose() {
        println!(
            "cargo:warning=Copied Swift Concurrency runtime: {} -> {}",
            source_path.display(),
            dest_path.display()
        );
    }
}

#[cfg(target_os = "macos")]
fn should_copy_swift_runtime_to_cargo_profile() -> bool {
    env::var(COPY_SWIFT_RUNTIME_TO_CARGO_PROFILE_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

#[cfg(target_os = "macos")]
fn remove_profile_swift_runtime_copy(frameworks_dir: &Path) {
    let stale_path = frameworks_dir.join(SWIFT_CONCURRENCY_DYLIB);
    match fs::remove_file(&stale_path) {
        Ok(()) => {
            if is_build_verbose() {
                println!(
                    "cargo:warning=Removed stale Cargo profile Swift runtime copy: {}",
                    stale_path.display()
                );
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            println!(
                "cargo:warning=Failed to remove stale Cargo profile Swift runtime copy {}: {}",
                stale_path.display(),
                err
            );
        }
    }
}

fn is_build_verbose() -> bool {
    env::var(BUILD_VERBOSE_ENV)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
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
