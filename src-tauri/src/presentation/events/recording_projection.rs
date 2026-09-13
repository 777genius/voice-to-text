use crate::domain::RecordingStatus;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingIntentProjectionPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_run_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_episode_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_phase: Option<RecordingContinuationPhase>,
    pub run_id: Option<u64>,
    pub intent_revision: Option<u64>,
    pub status: RecordingStatus,
    pub desired_on: bool,
    pub pending_start: bool,
    pub processing_jobs: usize,
    pub shutdown_requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<RecordingIntentProjectionFault>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault_run_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordingContinuationPhase {
    Active,
    Pausing,
    PausedReclaimable,
    ContinuePending,
    ActiveAwaitingAudio,
    Finalizing,
    Terminal,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecordingCaptureReadinessState {
    Unavailable,
    Buffering,
    Streaming,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingCaptureReadinessReason {
    Idle,
    StartingCapture,
    FinalizingPrevious,
    ConnectingProvider,
    Recording,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingCaptureReadinessPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_run_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_episode_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_ready: Option<bool>,
    pub generation: u64,
    pub run_id: Option<u64>,
    pub revision: Option<u64>,
    pub state: RecordingCaptureReadinessState,
    pub reason: RecordingCaptureReadinessReason,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingIntentProjectionFault {
    StartFailed,
    RuntimeFailed,
    StopUncertain,
    FinalizeFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_projection_uses_documented_camel_case_and_closed_phase_names() {
        let value = serde_json::to_value(RecordingIntentProjectionPayload {
            logical_run_id: Some(101),
            capture_episode_id: Some(102),
            continuation_phase: Some(RecordingContinuationPhase::ContinuePending),
            run_id: Some(102),
            intent_revision: Some(8),
            status: RecordingStatus::Processing,
            desired_on: true,
            pending_start: true,
            processing_jobs: 1,
            shutdown_requested: false,
            fault: None,
            fault_run_id: None,
        })
        .unwrap();
        assert_eq!(value["logicalRunId"], 101);
        assert_eq!(value["captureEpisodeId"], 102);
        assert_eq!(value["runId"], 102);
        assert_eq!(value["continuationPhase"], "continue_pending");
        assert!(value.get("logical_run_id").is_none());
        for (phase, wire) in [
            (RecordingContinuationPhase::Active, "active"),
            (RecordingContinuationPhase::Pausing, "pausing"),
            (
                RecordingContinuationPhase::PausedReclaimable,
                "paused_reclaimable",
            ),
            (
                RecordingContinuationPhase::ActiveAwaitingAudio,
                "active_awaiting_audio",
            ),
            (RecordingContinuationPhase::Finalizing, "finalizing"),
            (RecordingContinuationPhase::Terminal, "terminal"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), wire);
        }
    }
    #[test]
    fn readiness_separates_event_revision_physical_generation_and_transport_permission() {
        let value = serde_json::to_value(RecordingCaptureReadinessPayload {
            generation: 900,
            run_id: Some(102),
            revision: Some(8),
            state: RecordingCaptureReadinessState::Buffering,
            reason: RecordingCaptureReadinessReason::ConnectingProvider,
            capture_generation: Some(7),
            logical_run_id: Some(101),
            capture_episode_id: Some(102),
            capture_ready: Some(false),
            transport_ready: Some(false),
        })
        .unwrap();
        assert_eq!(value["generation"], 900);
        assert_eq!(value["captureGeneration"], 7);
        assert_eq!(value["captureReady"], false);
        assert_eq!(value["transportReady"], false);
        let legacy = serde_json::to_value(RecordingCaptureReadinessPayload {
            generation: 1,
            run_id: None,
            revision: None,
            state: RecordingCaptureReadinessState::Unavailable,
            reason: RecordingCaptureReadinessReason::Idle,
            capture_generation: None,
            logical_run_id: None,
            capture_episode_id: None,
            capture_ready: None,
            transport_ready: None,
        })
        .unwrap();
        assert!(legacy.get("logicalRunId").is_none());
        assert!(legacy.get("transportReady").is_none());
    }
}
