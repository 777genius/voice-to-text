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
        /// Streaming provider behind our backend: deepgram or elevenlabs
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
        /// Optional protocol capabilities. Unknown values are ignored by older backends.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },

    Pause {
        request_id: String,
        control_seq: u64,
        logical_run_id: String,
    },
    Continue {
        request_id: String,
        control_seq: u64,
        provider_session_id: String,
        pause_epoch: u64,
    },
    PauseRestore {
        request_id: String,
        control_seq: u64,
        provider_session_id: String,
        pause_epoch: u64,
        continue_request_id: String,
    },
    ControlStatus {
        query_id: String,
        operation_request_id: String,
        provider_session_id: String,
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
    Ready {
        session_id: String,
        #[serde(default)]
        accepted_capabilities: Vec<String>,
    },

    PauseAccepted {
        #[serde(flatten)]
        result: crate::domain::ContinuationControlResult,
        continue_window_ms: u64,
    },
    PauseRejected {
        #[serde(flatten)]
        result: crate::domain::ContinuationControlResult,
    },
    ContinueResult {
        #[serde(flatten)]
        result: crate::domain::ContinuationControlResult,
    },
    PauseRestoreResult {
        #[serde(flatten)]
        result: crate::domain::ContinuationControlResult,
    },
    ControlStatusResult {
        #[serde(flatten)]
        result: crate::domain::ContinuationStatusResult,
    },

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

    Stable {
        text: String,
        #[serde(default)]
        confidence: Option<f32>,
        delivery_seq: u64,
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
    Error {
        code: String,
        message: String,
        #[serde(flatten)]
        not_started: Option<crate::domain::ContinuationNotStarted>,
    },

    /// Backend завершил bounded-drain после Finalize.
    FinalizeComplete {
        status: String,
        saw_result: bool,
        #[serde(default)]
        outcome: Option<crate::domain::models::ProviderFinalizeReport>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r4_not_started_error_preserves_typed_correlation_and_cumulative_range() {
        for end in [7, 8] {
            let message = serde_json::json!({"type":"error", "code":"CONTINUE_AUDIO_NOT_STARTED", "message":"unsent",
                "provider_session_id":"same", "pause_epoch":1, "continue_request_id":"c1", "first_unsent_seq":7, "last_unsent_seq":end});
            match serde_json::from_value::<ServerMessage>(message).unwrap() {
                ServerMessage::Error {
                    not_started: Some(value),
                    ..
                } => {
                    assert_eq!(value.provider_session_id, "same");
                    assert_eq!(value.continue_request_id.as_deref(), Some("c1"));
                    assert_eq!((value.first_unsent_seq, value.last_unsent_seq), (7, end));
                }
                _ => panic!("typed NotStarted disposition lost"),
            }
        }
    }

    #[test]
    fn continuation_controls_match_backend_json() {
        let pause = serde_json::to_value(ClientMessage::Pause {
            request_id: "p1".into(),
            control_seq: 1,
            logical_run_id: "run1".into(),
        })
        .unwrap();
        assert_eq!(
            pause,
            serde_json::json!({"type":"pause","request_id":"p1","control_seq":1,"logical_run_id":"run1"})
        );
        let status = serde_json::to_value(ClientMessage::ControlStatus {
            query_id: "q1".into(),
            operation_request_id: "p1".into(),
            provider_session_id: "session".into(),
        })
        .unwrap();
        assert_eq!(
            status,
            serde_json::json!({"type":"control_status","query_id":"q1","operation_request_id":"p1","provider_session_id":"session"})
        );
        assert!(status.get("pause_epoch").is_none());
        assert!(status.get("control_seq").is_none());
        let restore = serde_json::to_value(ClientMessage::PauseRestore {
            request_id: "r1".into(),
            control_seq: 3,
            provider_session_id: "session".into(),
            pause_epoch: 1,
            continue_request_id: "c1".into(),
        })
        .unwrap();
        assert_eq!(
            restore,
            serde_json::json!({"type":"pause_restore","request_id":"r1","control_seq":3,"provider_session_id":"session","pause_epoch":1,"continue_request_id":"c1"})
        );
    }

    #[test]
    fn status_keeps_historical_acceptance_separate_from_current_eligibility() {
        let msg: ServerMessage = serde_json::from_str(r#"{"type":"control_status_result","query_id":"q1","operation_request_id":"c1","provider_session_id":"s","pause_epoch":1,"original_decision":"accepted","current_phase":"finalizing","eligible_now":false}"#).unwrap();
        match msg {
            ServerMessage::ControlStatusResult { result } => {
                assert_eq!(
                    result.original_decision,
                    Some(crate::domain::ControlDecision::Accepted)
                );
                assert!(!result.eligible_now);
                assert_eq!(
                    result.current_phase,
                    crate::domain::ContinuationPhase::Finalizing
                );
            }
            _ => panic!("expected status"),
        }
        let unknown: ServerMessage = serde_json::from_str(r#"{"type":"control_status_result","query_id":"q1","operation_request_id":"p1","provider_session_id":"s","pause_epoch":null,"original_decision":null,"current_phase":"active","eligible_now":false}"#).unwrap();
        assert!(
            matches!(unknown, ServerMessage::ControlStatusResult { result } if result.pause_epoch.is_none() && result.original_decision.is_none())
        );
    }

    #[test]
    fn pause_and_continue_responses_require_current_eligibility() {
        let response = r#"{"type":"pause_accepted","request_id":"p1","provider_session_id":"s","pause_epoch":1,"decision":"accepted","current_phase":"paused_reclaimable","eligible_now":true,"reason":null,"continue_window_ms":2000}"#;
        let parsed: ServerMessage = serde_json::from_str(response).unwrap();
        assert!(
            matches!(parsed, ServerMessage::PauseAccepted { result, continue_window_ms: 2000 } if result.pause_epoch == Some(1) && result.eligible_now)
        );
        let missing = response.replace(",\"eligible_now\":true", "");
        assert!(serde_json::from_str::<ServerMessage>(&missing).is_err());
        let missing_nullable = response.replace(",\"reason\":null", "");
        assert!(serde_json::from_str::<ServerMessage>(&missing_nullable).is_err());
        let invalid = response.replace("paused_reclaimable", "unknown_future_phase");
        assert!(serde_json::from_str::<ServerMessage>(&invalid).is_err());
    }

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
            capabilities: vec!["finalize_ack".to_string()],
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"config""#));
        assert!(json.contains(r#""provider":"deepgram""#));
        assert!(json.contains(r#""capabilities":["finalize_ack"]"#));
    }

    #[test]
    fn test_deserialize_ready_message() {
        let json = r#"{"type":"ready","session_id":"abc-123"}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::Ready { session_id, .. } => {
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
            ServerMessage::Error { code, message, .. } => {
                assert_eq!(code, "LIMIT_EXCEEDED");
                assert_eq!(message, "Usage limit reached");
            }
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_deserialize_finalize_complete_message() {
        let json = r#"{"type":"finalize_complete","status":"flushed","saw_result":true}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        match msg {
            ServerMessage::FinalizeComplete {
                status, saw_result, ..
            } => {
                assert_eq!(status, "flushed");
                assert!(saw_result);
            }
            _ => panic!("Expected FinalizeComplete message"),
        }
    }
    #[test]
    fn modern_ready_and_stable_preserve_capability_and_identity_without_timing() {
        let msg: ServerMessage = serde_json::from_str(
            r#"{"type":"ready","session_id":"x","accepted_capabilities":["finalize_outcome_v1"]}"#,
        )
        .unwrap();
        assert!(
            matches!(msg, ServerMessage::Ready { accepted_capabilities, .. } if accepted_capabilities == ["finalize_outcome_v1"])
        );
        let msg: ServerMessage =
            serde_json::from_str(r#"{"type":"stable","text":"да","delivery_seq":2}"#).unwrap();
        assert!(matches!(
            msg,
            ServerMessage::Stable {
                delivery_seq: 2,
                confidence: None,
                ..
            }
        ));
    }
}
