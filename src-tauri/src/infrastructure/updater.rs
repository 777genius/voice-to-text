use std::{
    sync::{Arc, Mutex},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_updater::UpdaterExt;
#[cfg(target_os = "windows")]
use super::config_store::ConfigStore;

/// Защита от двойного старта установки.
///
/// В Tauri окна — это отдельные webview'ы, и пользователь теоретически может нажать "Обновить"
/// в двух местах почти одновременно. Обновление — это ресурсная операция, поэтому делаем простой
/// глобальный lock на процесс.
static INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Информация о доступном обновлении, которую отдаём во frontend.
#[derive(Clone, serde::Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub body: String,
}

#[derive(Clone, serde::Serialize)]
struct UpdateDownloadProgressPayload {
    version: String,
    downloaded: u64,
    total: Option<u64>,
    progress: Option<u8>,
}

#[derive(Clone, serde::Serialize)]
struct UpdateInstallStagePayload {
    version: String,
}

/// Запускает фоновую проверку обновлений: сразу при старте, далее каждые 6 часов
pub fn start_background_update_check<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        // Небольшая задержка чтобы приложение успело инициализироваться
        tokio::time::sleep(Duration::from_secs(5)).await;

        loop {
            log::info!("Checking for app updates (background check)");

            match check_for_update(app.clone()).await {
                Ok(Some(update)) => {
                    log::info!("Update available: {}", update.version);
                    // Уведомляем frontend о доступном обновлении
                    if let Err(e) = app.emit("update:available", update) {
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
) -> Result<Option<UpdateInfo>, String> {
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
            Ok(Some(UpdateInfo {
                version: update.version.clone(),
                body: update.body.clone().unwrap_or_default(),
            }))
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

/// Проверяет и устанавливает обновление.
///
/// Важно: подтверждение делаем во frontend (наш UpdateDialog), поэтому тут
/// не показываем системный диалог — иначе получится двойное подтверждение.
pub async fn check_and_install_update<R: Runtime>(
    app: AppHandle<R>,
) -> Result<String, String> {
    let updater = app
        .updater_builder()
        .build()
        .map_err(|e| format!("Failed to build updater: {}", e))?;

    if INSTALL_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Update installation is already in progress".to_string());
    }

    let result = match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let current_version = update.current_version.clone();
            let body = update.body.clone().unwrap_or_default();

            log::info!("Update found: {} -> {}", current_version, version);
            log::info!("Release notes: {}", body);

            // Скачиваем и устанавливаем
            log::info!("Downloading and installing update...");

            let _ = app.emit(
                "update:download-started",
                UpdateInstallStagePayload {
                    version: version.clone(),
                },
            );

            let app_handle_progress = app.clone();
            let version_progress = version.clone();
            let app_handle_installing = app.clone();
            let version_installing = version.clone();
            let downloaded_total = Arc::new(Mutex::new(0u64));
            let downloaded_total_progress = Arc::clone(&downloaded_total);

            update
                .download_and_install(
                    move |chunk_length, content_length| {
                        let chunk_length = chunk_length as u64;

                        // В зависимости от платформы/реализации `chunk_length` может быть:
                        // - либо "сколько скачано всего"
                        // - либо "размер последнего чанка"
                        // Поэтому используем простую эвристику, чтобы корректно считать прогресс.
                        let mut downloaded_total = downloaded_total_progress
                            .lock()
                            .expect("update downloaded_total mutex poisoned");

                        let previous = *downloaded_total;
                        let downloaded = if let Some(total) = content_length {
                            if chunk_length <= total && chunk_length >= previous {
                                chunk_length
                            } else {
                                previous.saturating_add(chunk_length)
                            }
                        } else {
                            previous.saturating_add(chunk_length)
                        };

                        *downloaded_total = downloaded;

                        let progress = content_length.and_then(|total| {
                            if total == 0 {
                                return Some(0);
                            }
                            let pct = ((*downloaded_total as f64 / total as f64) * 100.0)
                                .clamp(0.0, 100.0) as u8;
                            Some(pct)
                        });

                        let _ = app_handle_progress.emit(
                            "update:download-progress",
                            UpdateDownloadProgressPayload {
                                version: version_progress.clone(),
                                downloaded: *downloaded_total,
                                total: content_length,
                                progress,
                            },
                        );
                    },
                    move || {
                        log::info!("Download completed, installing...");
                        let _ = app_handle_installing.emit(
                            "update:installing",
                            UpdateInstallStagePayload {
                                version: version_installing.clone(),
                            },
                        );
                    },
                )
                .await
                .map_err(|e| format!("Failed to download/install update: {}", e))?;

            log::info!("Update installed successfully, restarting...");

            // На Windows приложение стартует скрытым (без taskbar), поэтому после апдейта
            // пользователь может подумать, что "ничего не запустилось".
            // Ставим one-shot marker, чтобы на следующем запуске один раз показать окно.
            #[cfg(target_os = "windows")]
            if let Err(e) = ConfigStore::save_post_update_marker(&version).await {
                log::warn!("Failed to save post-update marker: {}", e);
            }

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
    };

    // В случае успеха приложение перезапустится (и код дальше не продолжится).
    // Если же мы дошли до сюда — значит либо обновления нет, либо была ошибка, и lock надо снять.
    INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);

    result
}
