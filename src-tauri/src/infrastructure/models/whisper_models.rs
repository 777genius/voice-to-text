use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

fn app_data_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "voice-to-text-dev"
    } else {
        "voice-to-text"
    }
}

fn legacy_shared_dir_name() -> &'static str {
    "voice-to-text"
}

fn scoped_app_data_dir(root: &std::path::Path) -> PathBuf {
    root.join(app_data_dir_name())
}

fn legacy_shared_app_data_dir(root: &std::path::Path) -> PathBuf {
    root.join(legacy_shared_dir_name())
}

fn copy_dir_recursive_once(
    source: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<()> {
    if !source.exists() {
        return Ok(());
    }
    if target.exists() {
        return Ok(());
    }

    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_recursive_once(&source_path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path)?;
        }
    }

    Ok(())
}

fn migrate_legacy_models_dir_once(root: &std::path::Path) -> anyhow::Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }

    let target_dir = scoped_app_data_dir(root).join("models");
    let legacy_dir = legacy_shared_app_data_dir(root).join("models");
    if target_dir == legacy_dir {
        return Ok(());
    }

    copy_dir_recursive_once(&legacy_dir, &target_dir)?;
    if target_dir.exists() {
        log::info!(
            "Ensured scoped models directory exists at {}",
            target_dir.display()
        );
    }
    Ok(())
}

/// Информация о модели Whisper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperModelInfo {
    /// Название модели (tiny, base, small, medium, large)
    pub name: String,

    /// Размер файла модели в байтах
    pub size_bytes: u64,

    /// Размер модели в человекочитаемом формате
    pub size_human: String,

    /// URL для загрузки с HuggingFace
    pub download_url: String,

    /// Описание модели
    pub description: String,

    /// Относительная скорость обработки (1.0 = base)
    pub speed_factor: f32,

    /// Относительное качество (1.0 = base)
    pub quality_factor: f32,
}

/// Доступные модели Whisper
pub const AVAILABLE_MODELS: &[(&str, &str, u64, f32, f32)] = &[
    // (name, description, size_bytes, speed_factor, quality_factor)
    (
        "tiny",
        "Самая быстрая модель, базовое качество",
        75_000_000, // ~75 MB
        4.0,        // 4x быстрее base
        0.6,        // 60% качества от base
    ),
    (
        "base",
        "Хороший баланс скорости и качества",
        142_000_000, // ~142 MB
        1.0,         // базовая скорость
        1.0,         // базовое качество
    ),
    (
        "small",
        "Рекомендуется для большинства случаев",
        466_000_000, // ~466 MB
        0.5,         // 2x медленнее base
        1.4,         // 140% качества от base
    ),
    (
        "medium",
        "Очень высокое качество, медленнее",
        1_500_000_000, // ~1.5 GB
        0.25,          // 4x медленнее base
        1.7,           // 170% качества от base
    ),
    (
        "large",
        "Максимальное качество, очень медленно",
        2_900_000_000, // ~2.9 GB
        0.125,         // 8x медленнее base
        2.0,           // 200% качества от base
    ),
];

/// Получает путь к директории хранения моделей
pub fn get_models_dir() -> anyhow::Result<PathBuf> {
    let app_data_dir =
        dirs::data_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine app data directory"))?;

    migrate_legacy_models_dir_once(&app_data_dir)?;

    let models_dir = scoped_app_data_dir(&app_data_dir).join("models");

    // Создаем директорию если не существует
    if !models_dir.exists() {
        fs::create_dir_all(&models_dir)?;
    }

    Ok(models_dir)
}

/// Получает путь к файлу конкретной модели
pub fn get_model_path(model_name: &str) -> anyhow::Result<PathBuf> {
    let models_dir = get_models_dir()?;
    Ok(models_dir.join(format!("ggml-{}.bin", model_name)))
}

/// Проверяет, существует ли модель локально
pub fn is_model_downloaded(model_name: &str) -> bool {
    if let Ok(model_path) = get_model_path(model_name) {
        model_path.exists()
    } else {
        false
    }
}

/// Получает размер загруженной модели в байтах
pub fn get_model_size(model_name: &str) -> Option<u64> {
    get_model_path(model_name)
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
}

/// Получает информацию о всех доступных моделях
pub fn get_available_models() -> Vec<WhisperModelInfo> {
    AVAILABLE_MODELS
        .iter()
        .map(|(name, desc, size, speed, quality)| {
            let size_human = format_size(*size);
            let download_url = format!(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
                name
            );

            WhisperModelInfo {
                name: name.to_string(),
                size_bytes: *size,
                size_human,
                download_url,
                description: desc.to_string(),
                speed_factor: *speed,
                quality_factor: *quality,
            }
        })
        .collect()
}

/// Форматирует размер файла в человекочитаемый формат
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Скачивает модель Whisper с HuggingFace
///
/// Использует streaming для экономии памяти и поддержки больших файлов.
/// Callback вызывается для отслеживания прогресса (downloaded_bytes, total_bytes).
pub async fn download_model<F>(model_name: &str, progress_callback: F) -> anyhow::Result<PathBuf>
where
    F: Fn(u64, u64) + Send + Sync,
{
    let model_info = get_available_models()
        .into_iter()
        .find(|m| m.name == model_name)
        .ok_or_else(|| anyhow::anyhow!("Model '{}' not found", model_name))?;

    let model_path = get_model_path(model_name)?;

    log::info!(
        "Downloading model '{}' from {}",
        model_name,
        model_info.download_url
    );
    log::info!("Target path: {}", model_path.display());

    // Создаем директорию если не существует
    if let Some(parent) = model_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Скачиваем файл через reqwest с streaming
    let client = reqwest::Client::new();
    let response = client.get(&model_info.download_url).send().await?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to download model: HTTP {}", response.status());
    }

    let total_size = response.content_length().unwrap_or(model_info.size_bytes);
    let mut downloaded: u64 = 0;

    // Создаем временный файл
    let temp_path = model_path.with_extension("tmp");
    let mut file = fs::File::create(&temp_path)?;

    // Скачиваем по частям
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        use std::io::Write;
        file.write_all(&chunk)?;

        downloaded += chunk.len() as u64;
        progress_callback(downloaded, total_size);
    }

    // Переименовываем временный файл в финальный
    fs::rename(&temp_path, &model_path)?;

    log::info!(
        "Model '{}' downloaded successfully to {}",
        model_name,
        model_path.display()
    );
    Ok(model_path)
}

/// Удаляет модель с диска
pub fn delete_model(model_name: &str) -> anyhow::Result<()> {
    let model_path = get_model_path(model_name)?;

    if model_path.exists() {
        fs::remove_file(&model_path)?;
        log::info!("Model '{}' deleted", model_name);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn app_data_dir_name_matches_build_profile() {
        #[cfg(debug_assertions)]
        assert_eq!(app_data_dir_name(), "voice-to-text-dev");

        #[cfg(not(debug_assertions))]
        assert_eq!(app_data_dir_name(), "voice-to-text");
    }

    #[test]
    fn migrate_legacy_models_dir_once_copies_models_to_scoped_dir() {
        let root = std::env::temp_dir().join(format!("voice-to-text-models-{}", Uuid::new_v4()));
        let legacy_models_dir = root.join("voice-to-text").join("models");
        fs::create_dir_all(&legacy_models_dir).unwrap();
        fs::write(legacy_models_dir.join("ggml-base.bin"), b"model-bytes").unwrap();

        migrate_legacy_models_dir_once(&root).unwrap();

        let target_models_dir = scoped_app_data_dir(&root).join("models");
        assert_eq!(
            fs::read(target_models_dir.join("ggml-base.bin")).unwrap(),
            b"model-bytes"
        );

        let _ = fs::remove_dir_all(root);
    }
}
