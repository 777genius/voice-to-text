use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Сначала запускаем стандартный билд Tauri
    tauri_build::build();

    // Загружаем .env файл если он существует
    if let Err(e) = dotenv::dotenv() {
        println!("cargo:warning=No .env file found: {}", e);
    }

    // Читаем API ключи из переменных окружения
    let deepgram_key = env::var("DEEPGRAM_API_KEY")
        .unwrap_or_else(|_| {
            println!("cargo:warning=DEEPGRAM_API_KEY not found in environment");
            String::new()
        });

    let assemblyai_key = env::var("ASSEMBLYAI_API_KEY")
        .unwrap_or_else(|_| {
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
    let out_dir = Path::new("src/infrastructure");
    let dest_path = out_dir.join("embedded_keys.rs");

    // Записываем сгенерированный код
    fs::write(&dest_path, embedded_keys_code)
        .expect("Failed to write embedded_keys.rs");

    println!("cargo:rerun-if-changed=../.env");
    println!("cargo:rerun-if-env-changed=DEEPGRAM_API_KEY");
    println!("cargo:rerun-if-env-changed=ASSEMBLYAI_API_KEY");

    println!("✅ Generated embedded_keys.rs with API keys");
}
