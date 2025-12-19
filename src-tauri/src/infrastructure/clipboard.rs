use arboard::Clipboard;
use anyhow::{Context, Result};

/// Записывает текст в системный clipboard
/// Работает на всех платформах (macOS/Windows/Linux) без активации окна
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    log::info!("📋 Копирую текст в clipboard ({} символов)", text.len());

    // Создаем экземпляр clipboard
    let mut clipboard = Clipboard::new()
        .context("Не удалось инициализировать clipboard")?;

    // Записываем текст
    clipboard.set_text(text)
        .context("Не удалось записать текст в clipboard")?;

    log::info!("✅ Текст успешно скопирован в clipboard");
    Ok(())
}

/// Читает текст из системного clipboard (опциональная функция)
#[allow(dead_code)]
pub fn read_from_clipboard() -> Result<String> {
    log::debug!("📋 Читаю текст из clipboard");

    let mut clipboard = Clipboard::new()
        .context("Не удалось инициализировать clipboard")?;

    let text = clipboard.get_text()
        .context("Не удалось прочитать текст из clipboard")?;

    log::debug!("✅ Текст прочитан из clipboard ({} символов)", text.len());
    Ok(text)
}
