//! Shared native callback routing, independent of the Tauri event executor.
use crate::application::TranscriptionService;
use crate::domain::{ErrorCallback, SttError};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) enum RuntimeFailureSource {
    Provider(u64),
    Capture(u64),
}
impl RuntimeFailureSource {
    pub(crate) fn is_capture(self) -> bool {
        matches!(self, Self::Capture(_))
    }
}

pub(crate) fn capture_error_callback(
    physical_run: u64,
    dispatch: impl Fn(RuntimeFailureSource, SttError) + Send + Sync + 'static,
) -> ErrorCallback {
    Arc::new(move |error| dispatch(RuntimeFailureSource::Capture(physical_run), error))
}

pub(crate) async fn resolve_runtime_failure(
    service: &TranscriptionService,
    source: RuntimeFailureSource,
    reason: &str,
) -> Option<u64> {
    match source {
        RuntimeFailureSource::Capture(physical) => {
            // Do not apply the active logical-session gate to a physical B.
            // Cleanup resolves ownership atomically before UI/reducer dispatch.
            service
                .cleanup_capture_runtime_failure(physical, reason)
                .await
        }
        RuntimeFailureSource::Provider(logical) => Some(logical),
    }
}

/// Called after cleanup by the production dispatcher; never infer negotiation
/// from the client offer or from the reducer's asynchronously installed route.
pub(crate) async fn runtime_failure_event(
    service: &TranscriptionService,
    logical: u64,
    error: crate::presentation::recording_intent_coordinator::ErrorCode,
) -> crate::presentation::recording_intent_coordinator::CoordinatorEvent {
    use crate::presentation::recording_intent_coordinator::{CoordinatorEvent, RunId};
    let run_id = RunId::new(logical);
    if service.runtime_failure_requires_terminal(logical).await {
        CoordinatorEvent::NegotiatedRuntimeFailed { run_id, error }
    } else {
        CoordinatorEvent::RuntimeFailed { run_id, error }
    }
}

use crate::domain::Transcription;
#[derive(Debug, Default)]
pub(crate) struct RunTranscriptDelivery {
    pub(crate) stable_snapshot: String,
    pub(crate) last_delivery_seq: Option<u64>,
    pub(crate) delivery_error: Option<String>,
    pub(crate) terminal: bool,
}

impl RunTranscriptDelivery {
    pub(crate) fn accept_stable(&mut self, transcription: &Transcription) -> bool {
        if self.terminal {
            return false;
        }
        if let Some(seq) = transcription.delivery_seq {
            if self.last_delivery_seq.is_some_and(|last| seq <= last) {
                return false;
            }
            self.last_delivery_seq = Some(seq);
        }
        if !transcription.text.trim().is_empty() {
            if !self.stable_snapshot.is_empty() {
                self.stable_snapshot.push(' ');
            }
            self.stable_snapshot.push_str(transcription.text.trim());
        }
        true
    }
}

impl RunTranscriptDelivery {
    pub(crate) fn close(&mut self) -> Option<String> {
        if self.terminal {
            return None;
        }
        self.terminal = true;
        Some(self.stable_snapshot.clone())
    }
}

use crate::domain::ContinuationDeliveryOwnership;
use crate::presentation::recording_intent_coordinator::{CoordinatorState, RunId};

/// Caller holds coordinator -> delivery locks. Native release may run later:
/// the delivery high-water fence is committed before either lock is released.
pub(crate) fn finish_registration(
    coordinator: &mut CoordinatorState,
    delivery: &mut ContinuationDeliveryOwnership,
    run: RunId,
) -> bool {
    let retire = coordinator.finish_native_candidate(run, delivery.contains(&run.get()));
    if retire {
        assert!(delivery.retire_unowned(run.get()));
    }
    retire
}
pub(crate) fn retire_abandoned(
    coordinator: &mut CoordinatorState,
    delivery: &mut ContinuationDeliveryOwnership,
) -> Vec<RunId> {
    let retired = coordinator.retire_native_candidates(|id| delivery.contains(&id.get()));
    for id in &retired {
        assert!(delivery.retire_unowned(id.get()));
    }
    retired
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::presentation::recording_intent_coordinator::*;
    use std::sync::Mutex;

    fn candidate(id: u64) -> CoordinatorState {
        let mut state = CoordinatorState::default();
        let run = RunContext {
            run_id: RunId::new(id),
            revision: IntentRevision::new(id),
            source: IntentSource::Frontend,
            policy: RuntimePolicySnapshot::default(),
        };
        state.capture = CaptureState::Recording { run };
        assert!(state.register_native_candidate(run.run_id));
        state
    }

    #[tokio::test]
    async fn retirement_claim_fences_ready_before_native_release_without_terminal() {
        let delivery = Arc::new(Mutex::new(ContinuationDeliveryOwnership::default()));
        // More than the capacity/history window: a late Ready cannot revive even
        // the oldest retired registration, and no tombstone set grows with runs.
        for id in 1..=12 {
            let mut state = candidate(id);
            state.capture = CaptureState::Idle;
            let decided = Arc::new(tokio::sync::Barrier::new(2));
            let release = Arc::new(tokio::sync::Barrier::new(2));
            let task = {
                let delivery = delivery.clone();
                let decided = decided.clone();
                let release = release.clone();
                tokio::spawn(async move {
                    let retired = retire_abandoned(&mut state, &mut delivery.lock().unwrap());
                    assert_eq!(retired, vec![RunId::new(id)]);
                    decided.wait().await;
                    release.wait().await;
                    retired // adapter may now issue these native release calls
                })
            };
            decided.wait().await;
            assert!(!delivery.lock().unwrap().accept(id, true));
            assert!(!delivery.lock().unwrap().accept(1, true));
            release.wait().await;
            assert_eq!(task.await.unwrap(), vec![RunId::new(id)]);
        }
        assert!(delivery.lock().unwrap().accept(13, true));
    }

    #[test]
    fn accepted_native_owners_survive_retirement_and_terminal_until_ack_at_capacity() {
        let mut delivery = ContinuationDeliveryOwnership::default();
        for id in 1..=8 {
            let mut state = candidate(id);
            assert!(delivery.accept(id, true)); // Ready wins the delivery mutex
            state.capture = CaptureState::Idle;
            assert!(retire_abandoned(&mut state, &mut delivery).is_empty());
            assert!(!finish_registration(
                &mut state,
                &mut delivery,
                RunId::new(id)
            ));
            delivery.terminal(id);
            assert!(!finish_registration(
                &mut state,
                &mut delivery,
                RunId::new(id)
            ));
            assert!(delivery.contains(&id));
        }
        assert!(!delivery.accept(9, true));
        assert!(delivery.authorize(1, 3, true));
        assert!(!delivery.settle(1, 2));
        assert!(delivery.settle(1, 3));
        assert!(delivery.accept(9, true));
        assert!(!delivery.accept(1, true));
        for id in 2..=8 {
            assert!(delivery.contains(&id));
        }
    }
}
