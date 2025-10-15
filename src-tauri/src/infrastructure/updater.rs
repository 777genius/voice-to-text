use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_updater::UpdaterExt;
use std::time::Duration;

/// Запускает фоновую проверку обновлений каждые 6 часов
pub fn start_background_update_check<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        // Первая проверка через 2 минуты после запуска
        tokio::time::sleep(Duration::from_secs(120)).await;

        loop {
            log::info!("Checking for app updates (background check)");

            match check_for_update(app.clone()).await {
                Ok(Some(version)) => {
                    log::info!("Update available: {}", version);
                    // Уведомляем frontend о доступном обновлении
                    if let Err(e) = app.emit("update:available", version) {
                        log::error!("Failed to emit update event: {}", e);
                    }
                }
                Ok(None) => {
                    log::debug!("No updates available");
                }
                Err(e) => {
                    log::error!("Failed to check for updates: {}", e);
                }
            }

            // Ждем 6 часов до следующей проверки
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    });
}

/// Проверяет наличие обновлений (без установки)
/// Возвращает версию если доступна, None если обновлений нет
pub async fn check_for_update<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<String>, String> {
    let updater = app
        .updater_builder()
        .build()
        .map_err(|e| format!("Failed to build updater: {}", e))?;

    match updater.check().await {
        Ok(Some(update)) => {
            log::info!(
                "Update found: {} (current: {})",
                update.version,
                update.current_version
            );
            Ok(Some(update.version.clone()))
        }
        Ok(None) => {
            log::info!("App is up to date");
            Ok(None)
        }
        Err(e) => {
            log::error!("Update check failed: {}", e);
            Err(format!("Update check failed: {}", e))
        }
    }
}

/// Проверяет и устанавливает обновление с подтверждением пользователя
pub async fn check_and_install_update<R: Runtime>(
    app: AppHandle<R>,
) -> Result<String, String> {
    let updater = app
        .updater_builder()
        .build()
        .map_err(|e| format!("Failed to build updater: {}", e))?;

    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let current_version = update.current_version.clone();
            let body = update.body.clone().unwrap_or_default();

            log::info!("Update found: {} -> {}", current_version, version);
            log::info!("Release notes: {}", body);

            // Показываем диалог подтверждения
            let confirmed = show_update_dialog(&app, &version, &body).await?;

            if !confirmed {
                return Ok("Update cancelled by user".to_string());
            }

            // Скачиваем и устанавливаем
            log::info!("Downloading and installing update...");

            update
                .download_and_install(
                    |chunk_length, content_length| {
                        let progress = if let Some(total) = content_length {
                            (chunk_length as f64 / total as f64 * 100.0) as u32
                        } else {
                            0
                        };
                        log::debug!("Download progress: {}%", progress);
                    },
                    || {
                        log::info!("Download completed, installing...");
                    },
                )
                .await
                .map_err(|e| format!("Failed to download/install update: {}", e))?;

            log::info!("Update installed successfully, restarting...");

            // Перезапускаем приложение
            app.restart();
        }
        Ok(None) => {
            log::info!("App is already up to date");
            Ok("No updates available".to_string())
        }
        Err(e) => {
            log::error!("Update check failed: {}", e);
            Err(format!("Failed to check for updates: {}", e))
        }
    }
}

/// Показывает диалог подтверждения обновления
async fn show_update_dialog<R: Runtime>(
    app: &AppHandle<R>,
    version: &str,
    body: &str,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let message = format!(
        "Доступна новая версия: {}\n\n{}\n\nУстановить обновление сейчас?",
        version,
        if body.is_empty() {
            "Улучшения и исправления ошибок"
        } else {
            body
        }
    );

    let result = app
        .dialog()
        .message(message)
        .title("Доступно обновление")
        .blocking_show();

    Ok(result)
}
