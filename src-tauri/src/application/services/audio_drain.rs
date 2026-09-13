use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDrainReason {
    Drained,
    Deadline,
    Cancelled,
    ProcessorError,
}

/// Counts source PCM bytes. Transport ACK bytes may use a different sample rate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AudioDrainReport {
    pub accepted_bytes: u64,
    pub read_bytes: u64,
    pub submitted_bytes: u64,
    pub acknowledged_bytes: Option<u64>,
    /// Subset of submitted bytes still lacking a transport ACK, when observable.
    pub unacknowledged_bytes: Option<u64>,
    pub remaining_bytes: u64,
    pub unknown_bytes: u64,
    pub reason: AudioDrainReason,
}

impl AudioDrainReport {
    /// Merge a completed episode exactly once when attaching the next episode.
    pub(super) fn accumulate(&mut self, previous: &Self) {
        self.accepted_bytes = self.accepted_bytes.saturating_add(previous.accepted_bytes);
        self.read_bytes = self.read_bytes.saturating_add(previous.read_bytes);
        self.submitted_bytes = self
            .submitted_bytes
            .saturating_add(previous.submitted_bytes);
        self.remaining_bytes = self
            .remaining_bytes
            .saturating_add(previous.remaining_bytes);
        self.unknown_bytes = self.unknown_bytes.saturating_add(previous.unknown_bytes);
        self.acknowledged_bytes = self
            .acknowledged_bytes
            .zip(previous.acknowledged_bytes)
            .map(|(a, b)| a.saturating_add(b));
        self.unacknowledged_bytes = self
            .unacknowledged_bytes
            .zip(previous.unacknowledged_bytes)
            .map(|(a, b)| a.saturating_add(b));
        if previous.reason != AudioDrainReason::Drained {
            self.reason = previous.reason;
        }
    }

    pub fn is_incomplete(&self) -> bool {
        self.reason != AudioDrainReason::Drained
            || self.remaining_bytes != 0
            || self.unknown_bytes != 0
            || self.unacknowledged_bytes.is_some_and(|bytes| bytes != 0)
    }
}

/// One instance follows one prepared receiver into its sole processor.
pub(super) struct AudioAccounting {
    source_bytes_per_second: AtomicU64,
    accepted: AtomicU64,
    read: AtomicU64,
    submitted: AtomicU64,
    acknowledged: AtomicU64,
    track_acknowledgements: AtomicBool,
    unknown: AtomicU64,
    pub outstanding: Arc<AtomicUsize>,
    pub sealed: AtomicBool,
    pub seal_notify: Notify,
    failure: Mutex<Option<crate::domain::SttError>>,
}

impl AudioAccounting {
    pub fn new(outstanding: Arc<AtomicUsize>) -> Self {
        Self {
            source_bytes_per_second: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            read: AtomicU64::new(0),
            submitted: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
            track_acknowledgements: AtomicBool::new(false),
            unknown: AtomicU64::new(0),
            outstanding,
            sealed: AtomicBool::new(false),
            seal_notify: Notify::new(),
            failure: Mutex::new(None),
        }
    }

    /// The first source format belongs to this run, even after capture moves to B.
    pub fn observe_source_format(&self, sample_rate: u32, channels: u16) {
        let bytes_per_second = u64::from(sample_rate) * u64::from(channels.max(1)) * 2;
        let _ = self.source_bytes_per_second.compare_exchange(
            0,
            bytes_per_second,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn remaining_source_duration(&self, maximum: std::time::Duration) -> std::time::Duration {
        let remaining = self.report(AudioDrainReason::Drained).remaining_bytes;
        if remaining == 0 {
            return std::time::Duration::ZERO;
        }
        let rate = self.source_bytes_per_second.load(Ordering::Acquire);
        if rate == 0 {
            return maximum;
        }
        std::time::Duration::from_nanos(remaining.saturating_mul(1_000_000_000).div_ceil(rate))
            .min(maximum)
    }

    pub fn accepted(&self, bytes: usize) {
        self.accepted.fetch_add(bytes as u64, Ordering::AcqRel);
    }

    pub fn read(&self, bytes: usize) {
        self.read.fetch_add(bytes as u64, Ordering::AcqRel);
    }

    pub fn restore_read(&self, bytes: usize) {
        self.read.fetch_sub(bytes as u64, Ordering::AcqRel);
    }

    pub fn track_acknowledgements(&self) {
        self.track_acknowledgements.store(true, Ordering::Release);
    }

    /// Only used when transport and source counters share the PCM representation.
    pub fn acknowledge(&self, cumulative_bytes: u64) {
        if !self.track_acknowledgements.load(Ordering::Acquire) {
            return;
        }
        let next = cumulative_bytes.min(self.submitted.load(Ordering::Acquire));
        let previous = self.acknowledged.fetch_max(next, Ordering::AcqRel);
        if next > previous {
            self.outstanding
                .fetch_sub((next - previous) as usize, Ordering::AcqRel);
        }
    }

    pub fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
        self.seal_notify.notify_one();
    }

    pub fn fail(&self, error: crate::domain::SttError) {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(error);
    }

    pub fn failure(&self) -> Option<crate::domain::SttError> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn sending(self: &Arc<Self>, bytes: usize) -> AudioSendLease {
        AudioSendLease {
            accounting: self.clone(),
            bytes,
            completed: false,
        }
    }

    pub fn report(&self, reason: AudioDrainReason) -> AudioDrainReport {
        let accepted = self.accepted.load(Ordering::Acquire);
        let submitted = self.submitted.load(Ordering::Acquire);
        let unknown = self.unknown.load(Ordering::Acquire);
        let remaining = accepted.saturating_sub(submitted).saturating_sub(unknown);
        let tracking = self.track_acknowledgements.load(Ordering::Acquire);
        let acknowledged = self.acknowledged.load(Ordering::Acquire);
        AudioDrainReport {
            accepted_bytes: accepted,
            read_bytes: self.read.load(Ordering::Acquire),
            submitted_bytes: submitted,
            acknowledged_bytes: tracking
                .then_some(acknowledged)
                .or_else(|| (accepted == 0).then_some(0)),
            unacknowledged_bytes: tracking
                .then_some(submitted.saturating_sub(acknowledged))
                .or_else(|| (accepted == 0).then_some(0)),
            remaining_bytes: remaining,
            unknown_bytes: unknown,
            reason: if reason == AudioDrainReason::Drained && (remaining != 0 || unknown != 0) {
                AudioDrainReason::ProcessorError
            } else {
                reason
            },
        }
    }
}

/// Cancelling a send future cannot silently reclassify its bytes as delivered.
pub(super) struct AudioSendLease {
    accounting: Arc<AudioAccounting>,
    bytes: usize,
    completed: bool,
}

impl AudioSendLease {
    /// Strong transport evidence: no send was polled and the same PCM is retained.
    pub fn not_started(mut self) {
        self.completed = true;
        // Retained PCM still owns its budget even for a non-ACK-tracked source.
        self.bytes = 0;
    }

    pub fn submitted(mut self) {
        self.accounting
            .submitted
            .fetch_add(self.bytes as u64, Ordering::AcqRel);
        self.completed = true;
    }
}

impl Drop for AudioSendLease {
    fn drop(&mut self) {
        if !self.completed {
            self.accounting
                .unknown
                .fetch_add(self.bytes as u64, Ordering::AcqRel);
        }
        if !self.completed
            || !self
                .accounting
                .track_acknowledgements
                .load(Ordering::Acquire)
        {
            self.accounting
                .outstanding
                .fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_not_started_retains_source_budget_until_the_single_later_submission() {
        let accounting = Arc::new(AudioAccounting::new(Arc::new(AtomicUsize::new(320))));
        accounting.accepted(320);
        accounting.read(320);
        accounting.sending(320).not_started();
        accounting.restore_read(320);
        let report = accounting.report(AudioDrainReason::Drained);
        assert_eq!(
            (
                report.read_bytes,
                report.submitted_bytes,
                report.unknown_bytes
            ),
            (0, 0, 0)
        );
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 320);
        accounting.read(320);
        accounting.sending(320).submitted();
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 0);
        assert_eq!(
            accounting.report(AudioDrainReason::Drained).submitted_bytes,
            320
        );
    }

    #[test]
    fn backend_budget_includes_submitted_audio_until_a_new_ack() {
        let accounting = Arc::new(AudioAccounting::new(Arc::new(AtomicUsize::new(960))));
        accounting.accepted(960);
        accounting.track_acknowledgements();
        accounting.read(640);
        accounting.sending(320).submitted();
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 960);
        assert_eq!(
            accounting
                .report(AudioDrainReason::Drained)
                .unacknowledged_bytes,
            Some(320)
        );
        accounting.acknowledge(320);
        accounting.acknowledge(320);
        accounting.acknowledge(0);
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 640);
        drop(accounting.sending(320));
        let report = accounting.report(AudioDrainReason::Drained);
        assert_eq!(report.acknowledged_bytes, Some(320));
        assert_eq!(report.unacknowledged_bytes, Some(0));
        assert_eq!(report.unknown_bytes, 320);
        assert_eq!(report.remaining_bytes, 320);
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 320);
    }

    #[test]
    fn dequeue_keeps_responsibility_and_cancelled_send_is_unknown() {
        let accounting = Arc::new(AudioAccounting::new(Arc::new(AtomicUsize::new(960))));
        accounting.accepted(960);
        accounting.read(640);
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 960);
        accounting.sending(320).submitted();
        drop(accounting.sending(320));
        let report = accounting.report(AudioDrainReason::Drained);
        assert_eq!(
            (
                report.submitted_bytes,
                report.unknown_bytes,
                report.remaining_bytes
            ),
            (320, 320, 320)
        );
        assert_eq!(report.reason, AudioDrainReason::ProcessorError);
        assert_eq!(report.acknowledged_bytes, None);
        assert_eq!(accounting.outstanding.load(Ordering::Acquire), 320);
    }

    #[test]
    fn empty_receiver_or_successful_send_does_not_prove_asr_ack() {
        let accounting = Arc::new(AudioAccounting::new(Arc::new(AtomicUsize::new(320))));
        accounting.accepted(320);
        accounting.read(320);
        accounting.sending(320).submitted();
        let report = accounting.report(AudioDrainReason::Drained);
        assert!(!report.is_incomplete());
        assert_eq!(report.acknowledged_bytes, None);
        assert_eq!(
            accounting.report(AudioDrainReason::Deadline).reason,
            AudioDrainReason::Deadline
        );
    }
}
