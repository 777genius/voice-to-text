//! Протокол сообщений между клиентом и нашим Backend API
//!
//! Формат совпадает с api/src/features/transcription/messages.rs

use serde::{Deserialize, Serialize};

/// Сообщения от клиента к бэкенду
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Конфигурация сессии (первое сообщение после подключения)
    Config {
        /// Версия протокола
        protocol_v: u16,
        /// Провайдер: deepgram
        provider: String,
        /// Язык распознавания (ISO 639-1)
        language: String,
        /// Частота дискретизации в Hz
        sample_rate: u32,
        /// Количество каналов (1 = моно)
        channels: u8,
        /// Кодировка: pcm_s16le
        encoding: String,
        /// Ключевые термины для улучшения распознавания
        #[serde(skip_serializing_if = "Option::is_none")]
        keyterms: Option<Vec<String>>,
    },

    /// Клиент закрывает сессию
    Close,

    /// Форсирует финализацию буфера на стороне провайдера (без закрытия соединения).
    ///
    /// Используется в keep-alive режиме: при остановке записи мы хотим, чтобы провайдер
    /// дослал финальные результаты для уже отправленного аудио, но WebSocket остался живым
    /// для быстрого старта следующей записи.
    Finalize,
}

/// Сообщения от бэкенда к клиенту
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ServerMessage {
    /// Сессия готова к приёму аудио
    Ready { session_id: String },

    /// Подтверждение приёма аудио чанка
    Ack { seq: u64 },

    /// Промежуточный результат (может измениться)
    Partial {
        text: String,
        #[serde(default)]
        confidence: Option<f32>,
        #[serde(default)]
        is_segment_final: Option<bool>,
        #[serde(default)]
        start_ms: Option<u64>,
        #[serde(default)]
        duration_ms: Option<u64>,
    },

    /// Финальный результат (не изменится)
    Final {
        text: String,
        #[serde(default)]
        confidence: Option<f32>,
        #[serde(default)]
        start_ms: Option<u64>,
        /// Длительность обработанного аудио в мс
        #[serde(default)]
        duration_ms: u64,
    },

    /// Обновление usage (для отображения на клиенте)
    UsageUpdate {
        seconds_used: f32,
        seconds_remaining_plan: f32,
        #[serde(default)]
        seconds_remaining_bonus: Option<f32>,
        #[serde(default)]
        seconds_remaining_total: Option<f32>,
    },

    /// Сессия успешно возобновлена
    Resumed {
        session_id: String,
        last_seq_acked: u64,
    },

    /// Ошибка
    Error { code: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_config_message() {
        let msg = ClientMessage::Config {
            protocol_v: 1,
            provider: "deepgram".to_string(),
            language: "ru".to_string(),
            sample_rate: 16000,
            channels: 1,
            encoding: "pcm_s16le".to_string(),
            keyterms: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"config""#));
        assert!(json.contains(r#""provider":"deepgram""#));
    }

    #[test]
    fn test_deserialize_ready_message() {
        let json = r#"{"type":"ready","session_id":"abc-123"}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::Ready { session_id } => {
                assert_eq!(session_id, "abc-123");
            }
            _ => panic!("Expected Ready message"),
        }
    }

    #[test]
    fn test_deserialize_partial_message() {
        let json = r#"{"type":"partial","text":"привет","confidence":0.85}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::Partial {
                text,
                confidence,
                is_segment_final,
                start_ms,
                duration_ms,
            } => {
                assert_eq!(text, "привет");
                assert_eq!(confidence, Some(0.85));
                assert_eq!(is_segment_final, None);
                assert_eq!(start_ms, None);
                assert_eq!(duration_ms, None);
            }
            _ => panic!("Expected Partial message"),
        }
    }

    #[test]
    fn test_deserialize_v2_segment_final_partial_message() {
        let json = r#"{"type":"partial","text":"первый кусок","confidence":0.9,"is_segment_final":true,"start_ms":120,"duration_ms":980}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::Partial {
                text,
                confidence,
                is_segment_final,
                start_ms,
                duration_ms,
            } => {
                assert_eq!(text, "первый кусок");
                assert_eq!(confidence, Some(0.9));
                assert_eq!(is_segment_final, Some(true));
                assert_eq!(start_ms, Some(120));
                assert_eq!(duration_ms, Some(980));
            }
            _ => panic!("Expected Partial message"),
        }
    }

    #[test]
    fn test_deserialize_final_without_duration_defaults_to_zero() {
        let json = r#"{"type":"final","text":"готово","confidence":0.9,"start_ms":120}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::Final {
                text,
                confidence,
                start_ms,
                duration_ms,
            } => {
                assert_eq!(text, "готово");
                assert_eq!(confidence, Some(0.9));
                assert_eq!(start_ms, Some(120));
                assert_eq!(duration_ms, 0);
            }
            _ => panic!("Expected Final message"),
        }
    }

    #[test]
    fn test_deserialize_usage_update() {
        let json = r#"{"type":"usage_update","seconds_used":10.5,"seconds_remaining_plan":989.5}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::UsageUpdate {
                seconds_used,
                seconds_remaining_plan,
                ..
            } => {
                assert!((seconds_used - 10.5).abs() < 0.01);
                assert!((seconds_remaining_plan - 989.5).abs() < 0.01);
            }
            _ => panic!("Expected UsageUpdate message"),
        }
    }

    #[test]
    fn test_deserialize_error_message() {
        let json = r#"{"type":"error","code":"LIMIT_EXCEEDED","message":"Usage limit reached"}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::Error { code, message } => {
                assert_eq!(code, "LIMIT_EXCEEDED");
                assert_eq!(message, "Usage limit reached");
            }
            _ => panic!("Expected Error message"),
        }
    }
}
