use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalizeReason {
    Drained,
    NoAudio,
    Deadline,
    ProviderError,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TailEvidence {
    NoAudio,
    SegmentObserved,
    Unconfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRelease {
    Released,
    Reusable,
    Unconfirmed,
}

/// Recognition evidence is independent of local transport cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFinalizeReport {
    pub reason: FinalizeReason,
    pub tail_evidence: TailEvidence,
    pub provider_release: ProviderRelease,
    pub last_delivery_seq: u64,
    pub stable_snapshot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Cumulative wire PCM bytes for this run. Backend ACK proves receipt, not ASR processing.
/// Source PCM may use another sample rate; compare only within the same representation.
pub struct AudioDeliveryProgress {
    pub sent_bytes: u64,
    pub acked_bytes: u64,
}
