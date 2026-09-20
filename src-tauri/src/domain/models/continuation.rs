//! Typed EL continuation wire evidence; acceptance alone is not an audio permit.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationPhase {
    Active,
    Pausing,
    PausedReclaimable,
    ActiveAwaitingAudio,
    Finalizing,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlDecision {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlRejection {
    Expired,
    Terminal,
    WrongEpoch,
    Ownership,
    Unsupported,
    Unavailable,
    Stale,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationControlResult {
    pub request_id: String,
    pub provider_session_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub pause_epoch: Option<u64>,
    pub decision: ControlDecision,
    pub current_phase: ContinuationPhase,
    pub eligible_now: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub reason: Option<ControlRejection>,
    /// Present only on the matching PauseAccepted response. Status-only
    /// recovery has no advertised duration and keeps the conservative window.
    #[serde(skip)]
    pub continue_window_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationStatusResult {
    pub query_id: String,
    pub operation_request_id: String,
    pub provider_session_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub pause_epoch: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub original_decision: Option<ControlDecision>,
    pub current_phase: ContinuationPhase,
    pub eligible_now: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationSession {
    pub connection_generation: u64,
    pub provider_session_id: String,
}

/// A caller must retain this identity even if the operation response is lost.
#[derive(Debug, Clone)]
pub enum ContinuationOperation {
    Pause {
        logical_run_id: u64,
    },
    Continue {
        pause_epoch: u64,
    },
    Restore {
        pause_epoch: u64,
        continue_request_id: String,
    },
}

/// Only the transport can prove that no first-B send was polled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationFirstWrite {
    Written,
    NotStarted,
}

/// Shared evidence lives across cancellation/timeout of the writer future.
/// Only the transport sets attempted, immediately before its first sink poll.
#[derive(Clone)]
pub struct ContinuationWriteFence {
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub attempted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub deadline: tokio::time::Instant,
}

impl ContinuationWriteFence {
    pub fn new(
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            cancelled,
            attempted: Default::default(),
            deadline,
        }
    }
    pub fn revoked(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            || tokio::time::Instant::now() >= self.deadline
    }
}

// The negotiated wire requires explicit null, rather than silently defaulting a
// missing field in a malformed result to unknown evidence.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Immutable native policy belonging to one captured episode. Format is the
/// normalized PCM format exposed by the selected native capture adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationCompatibility {
    pub effective_device: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub recording_mode: super::RecordingMode,
    pub auto_paste: bool,
    pub auto_copy: bool,
    pub microphone_sensitivity: u8,
    pub manual_stop_only: bool,
    pub vad_timeout_ms: u64,
}
impl ContinuationCompatibility {
    pub fn from_config(
        config: &super::AppConfig,
        audio: &super::AudioConfig,
        effective_device: Option<String>,
    ) -> Self {
        Self {
            effective_device,
            sample_rate: audio.sample_rate,
            channels: audio.channels,
            recording_mode: config.recording_mode,
            auto_paste: config.auto_paste_text,
            auto_copy: config.auto_copy_to_clipboard,
            microphone_sensitivity: config.microphone_sensitivity,
            manual_stop_only: config.keep_recording_until_manual_stop,
            vad_timeout_ms: config.vad_silence_timeout_ms,
        }
    }
}

#[derive(Default)]
pub struct ContinuationDeliveryOwnership {
    runs: std::collections::BTreeMap<u64, ContinuationDeliveryOwner>,
    high_water: u64,
}
struct ContinuationDeliveryOwner {
    auto_copy: bool,
    terminal_pending: bool,
    last_sequence: u64,
}
impl ContinuationDeliveryOwnership {
    /// No eviction: a full owner set refuses new automatic delivery.
    pub fn accept(&mut self, run: u64, auto_copy: bool) -> bool {
        if let Some(owner) = self.runs.get(&run) {
            return !owner.terminal_pending;
        }
        if run == 0 || run <= self.high_water || self.runs.len() >= 8 {
            return false;
        }
        self.high_water = run;
        self.runs.insert(
            run,
            ContinuationDeliveryOwner {
                auto_copy,
                terminal_pending: false,
                last_sequence: 0,
            },
        );
        true
    }
    pub fn contains(&self, run: &u64) -> bool {
        self.runs.contains_key(run)
    }
    pub fn terminal(&mut self, run: u64) {
        // Monotonic run IDs fence all retired history in constant space.
        self.high_water = self.high_water.max(run);
        if let Some(owner) = self.runs.get_mut(&run) {
            owner.terminal_pending = true;
        }
    }
    /// Claim native retirement under the same mutex as Ready acceptance.
    /// Once claimed, a queued native release cannot race a new delivery owner.
    pub fn retire_unowned(&mut self, run: u64) -> bool {
        if self.contains(&run) {
            return false;
        }
        self.high_water = self.high_water.max(run);
        true
    }
    pub fn authorize(&mut self, run: u64, seq: u64, nonempty_copy: bool) -> bool {
        let Some(owner) = self.runs.get_mut(&run) else {
            return false;
        };
        if seq == 0 || (nonempty_copy && (!owner.auto_copy || !owner.terminal_pending)) {
            return false;
        }
        owner.last_sequence = owner.last_sequence.max(seq);
        true
    }
    pub fn settle(&mut self, run: u64, seq: u64) -> bool {
        if !self
            .runs
            .get(&run)
            .is_some_and(|owner| owner.terminal_pending && seq >= owner.last_sequence)
        {
            return false;
        }
        self.runs.remove(&run);
        true
    }
}

/// Server disposition for positively unattempted provider egress. These source
/// frames were already attempted on the desktop socket, so this is NOT replay permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationNotStarted {
    pub provider_session_id: String,
    pub pause_epoch: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub continue_request_id: Option<String>,
    pub first_unsent_seq: u64,
    pub last_unsent_seq: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContinuationNotStartedEvidence {
    pub disposition: ContinuationNotStarted,
    pub known_unsent_bytes: u64,
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn terminal_delivery_ownership_survives_terminal_until_ack_then_rejects_late_ipc() {
        let mut owners = ContinuationDeliveryOwnership::default();
        assert!(owners.accept(1, true));
        assert!(owners.authorize(1, 1, false));
        assert!(!owners.authorize(1, 2, true));
        assert!(!owners.settle(1, 1));
        owners.terminal(1);
        assert!(owners.authorize(1, 2, true));
        assert!(!owners.settle(1, 1));
        assert!(owners.settle(1, 2));
        assert!(!owners.authorize(1, 3, false));
        assert!(!owners.accept(1, true));
    }
    #[test]
    fn immutable_auto_copy_false_still_allows_empty_terminal_context_gate() {
        let mut owners = ContinuationDeliveryOwnership::default();
        assert!(owners.accept(1, false));
        owners.terminal(1);
        assert!(!owners.accept(1, true)); // terminal cannot be accepted again
        assert!(!owners.authorize(1, 1, true));
        assert!(owners.authorize(1, 1, false));
        assert!(owners.settle(1, 1));
    }
    #[test]
    fn eight_live_terminal_owners_are_never_evicted_by_historical_acceptance() {
        let mut owners = ContinuationDeliveryOwnership::default();
        for run in 1..=8 {
            assert!(owners.accept(run, true));
            owners.terminal(run);
        }
        assert!(!owners.accept(9, true));
        assert!(owners.authorize(1, 1, false));
        assert!(owners.settle(1, 1));
        assert!(owners.accept(9, true));
    }
}
