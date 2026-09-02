//! Pure desired-state reducer for recording capture, finalization, and panel intent.
//!
//! The reducer owns ordering only. Callers execute returned effects after releasing
//! their coordinator lock and feed completions back as new events. This module must
//! remain synchronous and free of Tauri, audio, provider, filesystem, and runtime
//! dependencies.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

const DEFAULT_TRACE_CAPACITY: usize = 256;
const MAX_TRACE_CAPACITY: usize = 4_096;
const COMPLETED_EFFECT_RETENTION: usize = 4_096;
const MAX_PANEL_ATTEMPTS: u8 = 2;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

id_type!(IntentRevision);
id_type!(RunId);
id_type!(EffectId);
id_type!(GestureId);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ErrorCode(pub u16);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DesiredRecording {
    #[default]
    Off,
    On {
        revision: IntentRevision,
        source: IntentSource,
    },
}

impl DesiredRecording {
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On { .. })
    }

    pub const fn revision(self) -> Option<IntentRevision> {
        match self {
            Self::Off => None,
            Self::On { revision, .. } => Some(revision),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PanelGoal {
    Shown,
    Hidden,
    #[default]
    Preserve,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum IntentSource {
    #[default]
    Frontend,
    CarbonHotkey,
    DoubleSpaceHotkey,
    HoldHotkey,
    Vad,
    Runtime,
    System,
    Shutdown,
}

impl IntentSource {
    const fn is_hotkey(self) -> bool {
        matches!(
            self,
            Self::CarbonHotkey | Self::DoubleSpaceHotkey | Self::HoldHotkey
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordingMode {
    #[default]
    Toggle,
    Hold,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaptureMode {
    #[default]
    Dictation,
    LiveTranslation,
}

/// Immutable policy captured by every new run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimePolicySnapshot {
    pub version: u64,
    pub capture_mode: CaptureMode,
    pub recording_mode: RecordingMode,
    pub show_panel_on_start: bool,
    pub show_mini_panel: bool,
    pub hide_panel_on_hotkey_stop: bool,
    pub hide_panel_on_force_off: bool,
    /// Total stop attempts, including the first attempt.
    pub max_stop_attempts: u8,
    /// Total finalize attempts, including the first attempt.
    pub max_finalize_attempts: u8,
}

impl Default for RuntimePolicySnapshot {
    fn default() -> Self {
        Self {
            version: 0,
            capture_mode: CaptureMode::Dictation,
            recording_mode: RecordingMode::Toggle,
            show_panel_on_start: true,
            show_mini_panel: true,
            hide_panel_on_hotkey_stop: true,
            hide_panel_on_force_off: true,
            max_stop_attempts: 3,
            max_finalize_attempts: 2,
        }
    }
}

impl RuntimePolicySnapshot {
    fn normalized(mut self) -> Self {
        self.max_stop_attempts = self.max_stop_attempts.max(1);
        self.max_finalize_attempts = self.max_finalize_attempts.max(1);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunContext {
    pub run_id: RunId,
    pub revision: IntentRevision,
    pub source: IntentSource,
    pub policy: RuntimePolicySnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    User,
    Hotkey,
    HoldReleased,
    VadTimeout,
    RuntimeFailure,
    SystemSleep,
    PermissionRevoked,
    Shutdown,
    CompensatingStaleStart,
}

impl StopReason {
    const fn source(self) -> IntentSource {
        match self {
            Self::VadTimeout => IntentSource::Vad,
            Self::RuntimeFailure => IntentSource::Runtime,
            Self::SystemSleep | Self::PermissionRevoked => IntentSource::System,
            Self::Shutdown => IntentSource::Shutdown,
            Self::Hotkey | Self::HoldReleased => IntentSource::CarbonHotkey,
            Self::User | Self::CompensatingStaleStart => IntentSource::Frontend,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState {
    Idle,
    Starting {
        run: RunContext,
        effect_id: EffectId,
        cancel_requested: bool,
    },
    Recording {
        run: RunContext,
    },
    Stopping {
        run: RunContext,
        effect_id: EffectId,
        attempts: u8,
        reason: StopReason,
    },
    StopUncertain {
        run: RunContext,
        active_effect: Option<EffectId>,
        attempts: u8,
        reason: StopReason,
        error: ErrorCode,
    },
}

impl Default for CaptureState {
    fn default() -> Self {
        Self::Idle
    }
}

impl CaptureState {
    pub const fn run(self) -> Option<RunContext> {
        match self {
            Self::Idle => None,
            Self::Starting { run, .. }
            | Self::Recording { run }
            | Self::Stopping { run, .. }
            | Self::StopUncertain { run, .. } => Some(run),
        }
    }

    pub const fn phase(self) -> CapturePhase {
        match self {
            Self::Idle => CapturePhase::Idle,
            Self::Starting { .. } => CapturePhase::Starting,
            Self::Recording { .. } => CapturePhase::Recording,
            Self::Stopping { .. } => CapturePhase::Stopping,
            Self::StopUncertain { .. } => CapturePhase::StopUncertain,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CapturePhase {
    #[default]
    Idle,
    Starting,
    Recording,
    Stopping,
    StopUncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessingState {
    Finalizing {
        effect_id: EffectId,
        attempts: u8,
    },
    CompensatingStop {
        effect_id: EffectId,
        attempts: u8,
    },
    StopUncertain {
        effect_id: EffectId,
        attempts: u8,
        error: ErrorCode,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessingJob {
    pub run_id: RunId,
    pub state: ProcessingState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PanelState {
    #[default]
    Hidden,
    Shown {
        window_epoch: u64,
    },
    Showing {
        effect_id: EffectId,
        revision: IntentRevision,
    },
    Hiding {
        effect_id: EffectId,
        revision: IntentRevision,
        previous_epoch: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntentKind {
    Toggle,
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingIntent {
    pub kind: IntentKind,
    pub source: IntentSource,
    pub gesture_id: Option<GestureId>,
    /// Optional compare-and-set fence for callers stopping a previously
    /// observed run. A mismatched run is a stale no-op.
    pub expected_run_id: Option<RunId>,
}

impl RecordingIntent {
    pub const fn toggle(source: IntentSource, gesture_id: GestureId) -> Self {
        Self {
            kind: IntentKind::Toggle,
            source,
            gesture_id: Some(gesture_id),
            expected_run_id: None,
        }
    }

    pub const fn start(source: IntentSource, gesture_id: Option<GestureId>) -> Self {
        Self {
            kind: IntentKind::Start,
            source,
            gesture_id,
            expected_run_id: None,
        }
    }

    pub const fn stop(source: IntentSource, gesture_id: Option<GestureId>) -> Self {
        Self {
            kind: IntentKind::Stop,
            source,
            gesture_id,
            expected_run_id: None,
        }
    }

    pub const fn stop_expected(
        source: IntentSource,
        gesture_id: Option<GestureId>,
        expected_run_id: Option<RunId>,
    ) -> Self {
        Self {
            kind: IntentKind::Stop,
            source,
            gesture_id,
            expected_run_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartOutcome {
    Succeeded,
    Failed(ErrorCode),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStopOutcome {
    Inactive,
    FailedButInactive(ErrorCode),
    StillActive(ErrorCode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizeOutcome {
    Committed,
    NoTranscript,
    Failed(ErrorCode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowOutcome {
    Applied {
        window_epoch: u64,
    },
    /// The AppKit epoch changed before the operation committed. Reconcile the
    /// current goal against the reported native lifetime instead of assuming
    /// the requested visibility was applied.
    Superseded {
        window_epoch: u64,
    },
    Failed(ErrorCode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorEvent {
    InputTrace {
        source: IntentSource,
        gesture_id: Option<GestureId>,
        phase: InputTracePhase,
    },
    Intent(RecordingIntent),
    StartFinished {
        effect_id: EffectId,
        run_id: RunId,
        outcome: StartOutcome,
    },
    CaptureStopped {
        effect_id: EffectId,
        run_id: RunId,
        outcome: CaptureStopOutcome,
    },
    FinalizeFinished {
        effect_id: EffectId,
        run_id: RunId,
        outcome: FinalizeOutcome,
    },
    RuntimeFailed {
        run_id: RunId,
        error: ErrorCode,
    },
    WindowFinished {
        effect_id: EffectId,
        outcome: WindowOutcome,
    },
    PolicyUpdated(RuntimePolicySnapshot),
    ForceOff(StopReason),
    ShutdownRequested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputTracePhase {
    OsReceived,
    GestureAccepted,
    GestureRejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorEffect {
    StartRecording {
        effect_id: EffectId,
        run: RunContext,
    },
    /// Best-effort cancellation of the identified start. Its terminal result is
    /// still reported through `StartFinished` for the same effect ID.
    CancelStart {
        effect_id: EffectId,
        run_id: RunId,
    },
    StopRecording {
        effect_id: EffectId,
        run_id: RunId,
        reason: StopReason,
        attempt: u8,
    },
    FinalizeRecording {
        effect_id: EffectId,
        run_id: RunId,
        attempt: u8,
    },
    /// Releases the adapter-owned sender that keeps a run's transcript delivery
    /// task alive after a terminal failure where no further flush is possible.
    ReleaseTranscriptBarrier {
        run_id: RunId,
    },
    ShowPanel {
        effect_id: EffectId,
        revision: IntentRevision,
        source: IntentSource,
        policy: RuntimePolicySnapshot,
    },
    HidePanel {
        effect_id: EffectId,
        revision: IntentRevision,
        window_epoch: u64,
        reason: StopReason,
    },
    EmitProjection(RecordingStatusProjection),
    ShutdownReady,
}

impl CoordinatorEffect {
    pub const fn effect_id(self) -> Option<EffectId> {
        match self {
            Self::StartRecording { effect_id, .. }
            | Self::CancelStart { effect_id, .. }
            | Self::StopRecording { effect_id, .. }
            | Self::FinalizeRecording { effect_id, .. }
            | Self::ShowPanel { effect_id, .. }
            | Self::HidePanel { effect_id, .. } => Some(effect_id),
            Self::ReleaseTranscriptBarrier { .. }
            | Self::EmitProjection(_)
            | Self::ShutdownReady => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionStatus {
    Idle,
    Starting,
    Recording,
    Processing,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingStatusProjection {
    pub status: ProjectionStatus,
    pub desired_recording: DesiredRecording,
    pub panel_goal: PanelGoal,
    pub current_run: Option<RunId>,
    pub status_run: Option<RunId>,
    pub processing_jobs: usize,
    pub pending_start: bool,
    pub stopped_via_hotkey: bool,
    pub shutdown_requested: bool,
    pub fault: Option<ProjectionFault>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionFault {
    StartFailed,
    RuntimeFailed,
    StopUncertain,
    FinalizeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorFault {
    StartFailed {
        revision: IntentRevision,
        error: ErrorCode,
    },
    RuntimeFailed {
        run_id: RunId,
        error: ErrorCode,
    },
    StopUncertain {
        run_id: RunId,
        error: ErrorCode,
    },
    FinalizeFailed {
        run_id: RunId,
        error: ErrorCode,
    },
}

impl CoordinatorFault {
    const fn projection(self) -> ProjectionFault {
        match self {
            Self::StartFailed { .. } => ProjectionFault::StartFailed,
            Self::RuntimeFailed { .. } => ProjectionFault::RuntimeFailed,
            Self::StopUncertain { .. } => ProjectionFault::StopUncertain,
            Self::FinalizeFailed { .. } => ProjectionFault::FinalizeFailed,
        }
    }

    const fn blocks_capture_start(self) -> bool {
        matches!(self, Self::StopUncertain { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopRole {
    ActiveCapture,
    Compensating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingEffect {
    Start {
        run: RunContext,
    },
    Stop {
        run_id: RunId,
        role: StopRole,
        attempt: u8,
        reason: StopReason,
    },
    Finalize {
        run_id: RunId,
        attempt: u8,
    },
    ShowPanel {
        revision: IntentRevision,
        attempt: u8,
    },
    HidePanel {
        revision: IntentRevision,
        previous_epoch: u64,
        attempt: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PanelFailure {
    goal: PanelGoal,
    revision: IntentRevision,
    attempts: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletedEffect {
    Start {
        run_id: RunId,
        outcome: StartOutcome,
    },
    Stop {
        run_id: RunId,
        outcome: CaptureStopOutcome,
    },
    Finalize {
        run_id: RunId,
        outcome: FinalizeOutcome,
    },
    Window {
        outcome: WindowOutcome,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CoordinatorIds {
    intent: u64,
    run: u64,
    effect: u64,
}

impl CoordinatorIds {
    fn intent_revision(&mut self) -> IntentRevision {
        self.intent = self
            .intent
            .checked_add(1)
            .expect("intent revision exhausted");
        IntentRevision::new(self.intent)
    }

    fn run_id(&mut self) -> RunId {
        self.run = self.run.checked_add(1).expect("recording run ID exhausted");
        RunId::new(self.run)
    }

    fn effect_id(&mut self) -> EffectId {
        self.effect = self.effect.checked_add(1).expect("effect ID exhausted");
        EffectId::new(self.effect)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TracePhase {
    OsReceived,
    GestureAccepted,
    GestureRejected,
    IntentApplied,
    IntentRejected,
    ForceOff,
    Shutdown,
    PolicyUpdated,
    StartCompleted,
    CaptureStopped,
    FinalizeCompleted,
    WindowCompleted,
    RuntimeFailed,
    DuplicateCompletion,
    StaleCompletion,
    StartEnqueued,
    CaptureStopEnqueued,
    FinalizeStarted,
    WindowEnqueued,
    EffectEnqueued,
    CompensatingStop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceOutcome {
    StartSucceeded,
    StartFailed,
    StartCancelled,
    CaptureInactive,
    CaptureFailedButInactive,
    CaptureStillActive,
    FinalizeCommitted,
    FinalizeNoTranscript,
    FinalizeFailed,
    WindowApplied,
    WindowSuperseded,
    WindowFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceEntry {
    pub event_sequence: u64,
    pub monotonic_ns: u64,
    pub phase: TracePhase,
    pub source: Option<IntentSource>,
    pub gesture_id: Option<GestureId>,
    pub intent_revision: Option<IntentRevision>,
    pub run_id: Option<RunId>,
    pub effect_id: Option<EffectId>,
    pub window_epoch: Option<u64>,
    pub desired_before: DesiredRecording,
    pub desired_after: DesiredRecording,
    pub capture_before: CapturePhase,
    pub capture_after: CapturePhase,
    pub reason: Option<StopReason>,
    pub outcome: Option<TraceOutcome>,
    pub error: Option<ErrorCode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventTraceContext {
    phase: TracePhase,
    source: Option<IntentSource>,
    gesture_id: Option<GestureId>,
    run_id: Option<RunId>,
    effect_id: Option<EffectId>,
    window_epoch: Option<u64>,
    reason: Option<StopReason>,
    outcome: Option<TraceOutcome>,
    error: Option<ErrorCode>,
}

impl EventTraceContext {
    fn from_event(event: CoordinatorEvent) -> Self {
        match event {
            CoordinatorEvent::InputTrace {
                source,
                gesture_id,
                phase,
            } => Self {
                phase: match phase {
                    InputTracePhase::OsReceived => TracePhase::OsReceived,
                    InputTracePhase::GestureAccepted => TracePhase::GestureAccepted,
                    InputTracePhase::GestureRejected => TracePhase::GestureRejected,
                },
                source: Some(source),
                gesture_id,
                run_id: None,
                effect_id: None,
                window_epoch: None,
                reason: None,
                outcome: None,
                error: None,
            },
            CoordinatorEvent::Intent(intent) => Self {
                phase: TracePhase::IntentApplied,
                source: Some(intent.source),
                gesture_id: intent.gesture_id,
                run_id: None,
                effect_id: None,
                window_epoch: None,
                reason: None,
                outcome: None,
                error: None,
            },
            CoordinatorEvent::StartFinished {
                effect_id,
                run_id,
                outcome,
            } => Self {
                phase: TracePhase::StartCompleted,
                source: None,
                gesture_id: None,
                run_id: Some(run_id),
                effect_id: Some(effect_id),
                window_epoch: None,
                reason: None,
                outcome: Some(match outcome {
                    StartOutcome::Succeeded => TraceOutcome::StartSucceeded,
                    StartOutcome::Failed(_) => TraceOutcome::StartFailed,
                    StartOutcome::Cancelled => TraceOutcome::StartCancelled,
                }),
                error: match outcome {
                    StartOutcome::Failed(error) => Some(error),
                    StartOutcome::Succeeded | StartOutcome::Cancelled => None,
                },
            },
            CoordinatorEvent::CaptureStopped {
                effect_id,
                run_id,
                outcome,
            } => Self {
                phase: TracePhase::CaptureStopped,
                source: None,
                gesture_id: None,
                run_id: Some(run_id),
                effect_id: Some(effect_id),
                window_epoch: None,
                reason: None,
                outcome: Some(match outcome {
                    CaptureStopOutcome::Inactive => TraceOutcome::CaptureInactive,
                    CaptureStopOutcome::FailedButInactive(_) => {
                        TraceOutcome::CaptureFailedButInactive
                    }
                    CaptureStopOutcome::StillActive(_) => TraceOutcome::CaptureStillActive,
                }),
                error: match outcome {
                    CaptureStopOutcome::Inactive => None,
                    CaptureStopOutcome::FailedButInactive(error)
                    | CaptureStopOutcome::StillActive(error) => Some(error),
                },
            },
            CoordinatorEvent::FinalizeFinished {
                effect_id,
                run_id,
                outcome,
            } => Self {
                phase: TracePhase::FinalizeCompleted,
                source: None,
                gesture_id: None,
                run_id: Some(run_id),
                effect_id: Some(effect_id),
                window_epoch: None,
                reason: None,
                outcome: Some(match outcome {
                    FinalizeOutcome::Committed => TraceOutcome::FinalizeCommitted,
                    FinalizeOutcome::NoTranscript => TraceOutcome::FinalizeNoTranscript,
                    FinalizeOutcome::Failed(_) => TraceOutcome::FinalizeFailed,
                }),
                error: match outcome {
                    FinalizeOutcome::Failed(error) => Some(error),
                    FinalizeOutcome::Committed | FinalizeOutcome::NoTranscript => None,
                },
            },
            CoordinatorEvent::RuntimeFailed { run_id, error } => Self {
                phase: TracePhase::RuntimeFailed,
                source: Some(IntentSource::Runtime),
                gesture_id: None,
                run_id: Some(run_id),
                effect_id: None,
                window_epoch: None,
                reason: Some(StopReason::RuntimeFailure),
                outcome: None,
                error: Some(error),
            },
            CoordinatorEvent::WindowFinished { effect_id, outcome } => Self {
                phase: TracePhase::WindowCompleted,
                source: None,
                gesture_id: None,
                run_id: None,
                effect_id: Some(effect_id),
                window_epoch: match outcome {
                    WindowOutcome::Applied { window_epoch }
                    | WindowOutcome::Superseded { window_epoch } => Some(window_epoch),
                    WindowOutcome::Failed(_) => None,
                },
                reason: None,
                outcome: Some(match outcome {
                    WindowOutcome::Applied { .. } => TraceOutcome::WindowApplied,
                    WindowOutcome::Superseded { .. } => TraceOutcome::WindowSuperseded,
                    WindowOutcome::Failed(_) => TraceOutcome::WindowFailed,
                }),
                error: match outcome {
                    WindowOutcome::Failed(error) => Some(error),
                    WindowOutcome::Applied { .. } | WindowOutcome::Superseded { .. } => None,
                },
            },
            CoordinatorEvent::PolicyUpdated(_) => Self {
                phase: TracePhase::PolicyUpdated,
                source: None,
                gesture_id: None,
                run_id: None,
                effect_id: None,
                window_epoch: None,
                reason: None,
                outcome: None,
                error: None,
            },
            CoordinatorEvent::ForceOff(reason) => Self {
                phase: TracePhase::ForceOff,
                source: Some(reason.source()),
                gesture_id: None,
                run_id: None,
                effect_id: None,
                window_epoch: None,
                reason: Some(reason),
                outcome: None,
                error: None,
            },
            CoordinatorEvent::ShutdownRequested => Self {
                phase: TracePhase::Shutdown,
                source: Some(IntentSource::Shutdown),
                gesture_id: None,
                run_id: None,
                effect_id: None,
                window_epoch: None,
                reason: Some(StopReason::Shutdown),
                outcome: None,
                error: None,
            },
        }
    }
}

/// All mutable state required by the pure reducer.
#[derive(Debug)]
pub struct CoordinatorState {
    pub desired_recording: DesiredRecording,
    pub desired_panel: PanelGoal,
    pub capture: CaptureState,
    pub processing_jobs: BTreeMap<RunId, ProcessingJob>,
    pub current_policy: RuntimePolicySnapshot,
    pub panel: PanelState,
    pub fault: Option<CoordinatorFault>,
    pub shutdown_requested: bool,
    desired_stop_reason: StopReason,
    ids: CoordinatorIds,
    latest_gesture_by_source: BTreeMap<IntentSource, GestureId>,
    active_hold_gesture: Option<GestureId>,
    in_flight: BTreeMap<EffectId, PendingEffect>,
    completed_effects: BTreeMap<EffectId, CompletedEffect>,
    completed_effect_order: VecDeque<EffectId>,
    mismatched_start_cleanups: BTreeSet<(EffectId, RunId)>,
    blocked_start_revision: Option<IntentRevision>,
    panel_failure: Option<PanelFailure>,
    terminal_status_run: Option<RunId>,
    last_projection: Option<RecordingStatusProjection>,
    shutdown_ready_emitted: bool,
    event_sequence: u64,
    last_monotonic_ns: u64,
    trace_capacity: usize,
    trace: VecDeque<TraceEntry>,
}

impl Default for CoordinatorState {
    fn default() -> Self {
        Self::new(RuntimePolicySnapshot::default())
    }
}

impl CoordinatorState {
    pub fn new(policy: RuntimePolicySnapshot) -> Self {
        Self::with_trace_capacity(policy, DEFAULT_TRACE_CAPACITY)
    }

    pub fn with_trace_capacity(policy: RuntimePolicySnapshot, trace_capacity: usize) -> Self {
        Self {
            desired_recording: DesiredRecording::Off,
            desired_panel: PanelGoal::Preserve,
            capture: CaptureState::Idle,
            processing_jobs: BTreeMap::new(),
            current_policy: policy.normalized(),
            panel: PanelState::Hidden,
            fault: None,
            shutdown_requested: false,
            desired_stop_reason: StopReason::User,
            ids: CoordinatorIds::default(),
            latest_gesture_by_source: BTreeMap::new(),
            active_hold_gesture: None,
            in_flight: BTreeMap::new(),
            completed_effects: BTreeMap::new(),
            completed_effect_order: VecDeque::new(),
            mismatched_start_cleanups: BTreeSet::new(),
            blocked_start_revision: None,
            panel_failure: None,
            terminal_status_run: None,
            last_projection: None,
            shutdown_ready_emitted: false,
            event_sequence: 0,
            last_monotonic_ns: 0,
            trace_capacity: trace_capacity.min(MAX_TRACE_CAPACITY),
            trace: VecDeque::new(),
        }
    }

    pub fn trace(&self) -> impl ExactSizeIterator<Item = &TraceEntry> {
        self.trace.iter()
    }

    pub fn projection(&self) -> RecordingStatusProjection {
        let status = match self.capture {
            CaptureState::StopUncertain { .. } => ProjectionStatus::Error,
            CaptureState::Starting {
                cancel_requested: true,
                ..
            } => ProjectionStatus::Processing,
            CaptureState::Starting { .. } => ProjectionStatus::Starting,
            CaptureState::Recording { .. } => ProjectionStatus::Recording,
            CaptureState::Stopping { .. } => ProjectionStatus::Processing,
            CaptureState::Idle if self.fault.is_some() => ProjectionStatus::Error,
            CaptureState::Idle if !self.processing_jobs.is_empty() => ProjectionStatus::Processing,
            CaptureState::Idle => ProjectionStatus::Idle,
        };
        let pending_start = self.desired_recording.is_on()
            && !self
                .fault
                .is_some_and(CoordinatorFault::blocks_capture_start)
            && !matches!(
                self.capture,
                CaptureState::Starting { .. } | CaptureState::Recording { .. }
            );
        RecordingStatusProjection {
            status,
            desired_recording: self.desired_recording,
            panel_goal: self.desired_panel,
            current_run: self.capture.run().map(|run| run.run_id),
            status_run: self
                .capture
                .run()
                .map(|run| run.run_id)
                .or_else(|| self.processing_jobs.keys().next_back().copied())
                .or(self.terminal_status_run),
            processing_jobs: self.processing_jobs.len(),
            pending_start,
            stopped_via_hotkey: matches!(
                self.desired_stop_reason,
                StopReason::Hotkey | StopReason::HoldReleased
            ),
            shutdown_requested: self.shutdown_requested,
            fault: self.fault.map(CoordinatorFault::projection),
        }
    }

    /// Verifies reducer-owned invariants without inspecting external resources.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.shutdown_requested && self.desired_recording.is_on() {
            return Err("shutdown cannot retain desired recording On");
        }
        if let Some(run) = self.capture.run() {
            if self.processing_jobs.contains_key(&run.run_id) {
                return Err("current capture run also exists as a processing job");
            }
        }

        match self.capture {
            CaptureState::Starting { run, effect_id, .. } => {
                if self.in_flight.get(&effect_id) != Some(&PendingEffect::Start { run }) {
                    return Err("Starting state lacks its start effect");
                }
            }
            CaptureState::Stopping {
                run,
                effect_id,
                attempts,
                reason,
            } => {
                if self.in_flight.get(&effect_id)
                    != Some(&PendingEffect::Stop {
                        run_id: run.run_id,
                        role: StopRole::ActiveCapture,
                        attempt: attempts,
                        reason,
                    })
                {
                    return Err("Stopping state lacks its stop effect");
                }
            }
            CaptureState::StopUncertain {
                run,
                active_effect: Some(effect_id),
                attempts,
                reason,
                ..
            } => {
                if self.in_flight.get(&effect_id)
                    != Some(&PendingEffect::Stop {
                        run_id: run.run_id,
                        role: StopRole::ActiveCapture,
                        attempt: attempts,
                        reason,
                    })
                {
                    return Err("StopUncertain retry lacks its stop effect");
                }
            }
            CaptureState::Idle
            | CaptureState::Recording { .. }
            | CaptureState::StopUncertain {
                active_effect: None,
                ..
            } => {}
        }

        for (run_id, job) in &self.processing_jobs {
            if *run_id != job.run_id {
                return Err("processing job key does not match run ID");
            }
            let expected = match job.state {
                ProcessingState::Finalizing {
                    effect_id,
                    attempts,
                } => Some((
                    effect_id,
                    PendingEffect::Finalize {
                        run_id: *run_id,
                        attempt: attempts,
                    },
                )),
                ProcessingState::CompensatingStop {
                    effect_id,
                    attempts,
                } => Some((
                    effect_id,
                    PendingEffect::Stop {
                        run_id: *run_id,
                        role: StopRole::Compensating,
                        attempt: attempts,
                        reason: StopReason::CompensatingStaleStart,
                    },
                )),
                ProcessingState::StopUncertain {
                    effect_id,
                    attempts,
                    ..
                } => Some((
                    effect_id,
                    PendingEffect::Stop {
                        run_id: *run_id,
                        role: StopRole::Compensating,
                        attempt: attempts,
                        reason: StopReason::CompensatingStaleStart,
                    },
                )),
            };
            if let Some((effect_id, pending)) = expected {
                if self.in_flight.get(&effect_id) != Some(&pending) {
                    return Err("processing job lacks its effect");
                }
            }
        }

        for effect_id in self.in_flight.keys() {
            if self.completed_effects.contains_key(effect_id) {
                return Err("effect is both in flight and completed");
            }
        }
        Ok(())
    }

    fn next_revision(&mut self) -> IntentRevision {
        self.ids.intent_revision()
    }

    fn next_effect(&mut self) -> EffectId {
        self.ids.effect_id()
    }

    fn push_trace(&mut self, entry: TraceEntry) {
        if self.trace_capacity == 0 {
            return;
        }
        while self.trace.len() >= self.trace_capacity {
            self.trace.pop_front();
        }
        self.trace.push_back(entry);
    }

    fn remember_completion(&mut self, effect_id: EffectId, completion: CompletedEffect) {
        self.completed_effects.insert(effect_id, completion);
        self.completed_effect_order.push_back(effect_id);
        while self.completed_effect_order.len() > COMPLETED_EFFECT_RETENTION {
            if let Some(expired) = self.completed_effect_order.pop_front() {
                self.completed_effects.remove(&expired);
            }
        }
    }
}

/// Reduces one event using a deterministic logical timestamp.
pub fn reduce(state: &mut CoordinatorState, event: CoordinatorEvent) -> Vec<CoordinatorEffect> {
    let logical_ns = state.last_monotonic_ns.saturating_add(1);
    reduce_at(state, event, logical_ns)
}

/// Reduces one event and records the caller-provided monotonic timestamp.
pub fn reduce_at(
    state: &mut CoordinatorState,
    event: CoordinatorEvent,
    monotonic_ns: u64,
) -> Vec<CoordinatorEffect> {
    state.event_sequence = state
        .event_sequence
        .checked_add(1)
        .expect("coordinator event sequence exhausted");
    state.last_monotonic_ns = monotonic_ns.max(state.last_monotonic_ns);

    let before_desired = state.desired_recording;
    let before_capture = state.capture.phase();
    let context = EventTraceContext::from_event(event);
    let mut phase = context.phase;
    let mut effects = Vec::new();

    apply_event(state, event, &mut effects, &mut phase);
    reconcile_capture(state, &mut effects);
    reconcile_panel(state, &mut effects);
    reconcile_projection(state, &mut effects);
    reconcile_shutdown(state, &mut effects);

    let revision = state.desired_recording.revision();
    state.push_trace(TraceEntry {
        event_sequence: state.event_sequence,
        monotonic_ns: state.last_monotonic_ns,
        phase,
        source: context.source,
        gesture_id: context.gesture_id,
        intent_revision: revision,
        run_id: context.run_id,
        effect_id: context.effect_id,
        window_epoch: context.window_epoch,
        desired_before: before_desired,
        desired_after: state.desired_recording,
        capture_before: before_capture,
        capture_after: state.capture.phase(),
        reason: context.reason,
        outcome: context.outcome,
        error: context.error,
    });
    trace_enqueued_effects(state, &effects);

    debug_assert!(state.validate().is_ok());
    effects
}

fn apply_event(
    state: &mut CoordinatorState,
    event: CoordinatorEvent,
    effects: &mut Vec<CoordinatorEffect>,
    phase: &mut TracePhase,
) {
    match event {
        CoordinatorEvent::InputTrace { .. } => {}
        CoordinatorEvent::Intent(intent) => apply_intent(state, intent, phase),
        CoordinatorEvent::StartFinished {
            effect_id,
            run_id,
            outcome,
        } => apply_start_finished(state, effect_id, run_id, outcome, effects, phase),
        CoordinatorEvent::CaptureStopped {
            effect_id,
            run_id,
            outcome,
        } => apply_capture_stopped(state, effect_id, run_id, outcome, effects, phase),
        CoordinatorEvent::FinalizeFinished {
            effect_id,
            run_id,
            outcome,
        } => apply_finalize_finished(state, effect_id, run_id, outcome, effects, phase),
        CoordinatorEvent::RuntimeFailed { run_id, error } => {
            apply_runtime_failed(state, run_id, error)
        }
        CoordinatorEvent::WindowFinished { effect_id, outcome } => {
            apply_window_finished(state, effect_id, outcome, phase)
        }
        CoordinatorEvent::PolicyUpdated(policy) => {
            state.current_policy = policy.normalized();
        }
        CoordinatorEvent::ForceOff(reason) => force_off(state, reason),
        CoordinatorEvent::ShutdownRequested => {
            state.shutdown_requested = true;
            state.shutdown_ready_emitted = false;
            force_off(state, StopReason::Shutdown);
        }
    }
}

fn apply_intent(state: &mut CoordinatorState, intent: RecordingIntent, phase: &mut TracePhase) {
    if state.shutdown_requested {
        *phase = TracePhase::IntentRejected;
        return;
    }
    if intent.kind == IntentKind::Stop
        && intent.expected_run_id.is_some()
        && state.capture.run().map(|run| run.run_id) != intent.expected_run_id
    {
        *phase = TracePhase::IntentRejected;
        return;
    }
    if let Some(gesture_id) = intent.gesture_id {
        let matching_hold_release = intent.source == IntentSource::HoldHotkey
            && intent.kind == IntentKind::Stop
            && state.active_hold_gesture == Some(gesture_id);
        if intent.source == IntentSource::HoldHotkey
            && intent.kind == IntentKind::Stop
            && !matching_hold_release
        {
            *phase = TracePhase::IntentRejected;
            return;
        }
        let previous = state.latest_gesture_by_source.get(&intent.source).copied();
        if !matching_hold_release && previous.is_some_and(|seen| gesture_id <= seen) {
            *phase = TracePhase::IntentRejected;
            return;
        }
        if matching_hold_release {
            state.active_hold_gesture = None;
        } else {
            state
                .latest_gesture_by_source
                .insert(intent.source, gesture_id);
            if intent.source == IntentSource::HoldHotkey && intent.kind == IntentKind::Start {
                state.active_hold_gesture = Some(gesture_id);
            }
        }
    }

    let wants_on = match intent.kind {
        IntentKind::Toggle => !state.desired_recording.is_on(),
        IntentKind::Start => true,
        IntentKind::Stop => false,
    };
    let revision = state.next_revision();
    if !state
        .fault
        .is_some_and(CoordinatorFault::blocks_capture_start)
    {
        state.fault = None;
        state.blocked_start_revision = None;
    }
    state.panel_failure = None;
    if wants_on {
        if state.current_policy.show_panel_on_start
            && matches!(state.panel, PanelState::Shown { .. })
        {
            // Every accepted transition to On is also a foreground/show request.
            // Re-issuing the idempotent show repairs native visibility after an
            // out-of-band UI auto-hide and advances the epoch for this intent.
            state.panel = PanelState::Hidden;
        }
        state.desired_recording = DesiredRecording::On {
            revision,
            source: intent.source,
        };
        state.desired_panel = if state.current_policy.show_panel_on_start {
            PanelGoal::Shown
        } else {
            PanelGoal::Preserve
        };
    } else {
        state.active_hold_gesture = None;
        state.desired_recording = DesiredRecording::Off;
        state.desired_stop_reason = stop_reason_for_source(intent.source);
        state.desired_panel = panel_goal_for_off(state.current_policy, intent.source);
    }
}

fn force_off(state: &mut CoordinatorState, reason: StopReason) {
    let _revision = state.next_revision();
    state.desired_recording = DesiredRecording::Off;
    state.active_hold_gesture = None;
    state.desired_stop_reason = reason;
    state.blocked_start_revision = None;
    state.panel_failure = None;
    state.desired_panel =
        if reason == StopReason::Shutdown || state.current_policy.hide_panel_on_force_off {
            PanelGoal::Hidden
        } else {
            PanelGoal::Preserve
        };
}

fn panel_goal_for_off(policy: RuntimePolicySnapshot, source: IntentSource) -> PanelGoal {
    if source.is_hotkey() && policy.hide_panel_on_hotkey_stop {
        PanelGoal::Hidden
    } else {
        PanelGoal::Preserve
    }
}

fn stop_reason_for_source(source: IntentSource) -> StopReason {
    match source {
        IntentSource::CarbonHotkey | IntentSource::DoubleSpaceHotkey => StopReason::Hotkey,
        IntentSource::HoldHotkey => StopReason::HoldReleased,
        IntentSource::Vad => StopReason::VadTimeout,
        IntentSource::Runtime => StopReason::RuntimeFailure,
        IntentSource::System => StopReason::SystemSleep,
        IntentSource::Shutdown => StopReason::Shutdown,
        IntentSource::Frontend => StopReason::User,
    }
}

fn apply_runtime_failed(state: &mut CoordinatorState, run_id: RunId, error: ErrorCode) {
    if state.capture.run().map(|run| run.run_id) != Some(run_id) {
        return;
    }
    set_recoverable_fault(state, CoordinatorFault::RuntimeFailed { run_id, error });
    force_off(state, StopReason::RuntimeFailure);
}

fn clear_recoverable_fault(state: &mut CoordinatorState) {
    if !state
        .fault
        .is_some_and(CoordinatorFault::blocks_capture_start)
    {
        state.fault = None;
    }
}

fn set_recoverable_fault(state: &mut CoordinatorState, fault: CoordinatorFault) {
    if !state
        .fault
        .is_some_and(CoordinatorFault::blocks_capture_start)
    {
        state.fault = Some(fault);
    }
}

fn apply_start_finished(
    state: &mut CoordinatorState,
    effect_id: EffectId,
    run_id: RunId,
    outcome: StartOutcome,
    effects: &mut Vec<CoordinatorEffect>,
    phase: &mut TracePhase,
) {
    if state.completed_effects.contains_key(&effect_id) {
        *phase = TracePhase::DuplicateCompletion;
        return;
    }

    let pending = state.in_flight.get(&effect_id).copied();
    let expected_run = match pending {
        Some(PendingEffect::Start { run }) if run.run_id == run_id => Some(run),
        _ => None,
    };
    let Some(run) = expected_run else {
        *phase = TracePhase::StaleCompletion;
        if outcome == StartOutcome::Succeeded {
            ensure_compensating_stop(state, effect_id, run_id, effects);
            *phase = TracePhase::CompensatingStop;
        }
        return;
    };

    state.in_flight.remove(&effect_id);
    state.remember_completion(effect_id, CompletedEffect::Start { run_id, outcome });

    let is_current = matches!(
        state.capture,
        CaptureState::Starting {
            run: current,
            effect_id: current_effect,
            ..
        } if current.run_id == run_id && current_effect == effect_id
    );
    if !is_current {
        *phase = TracePhase::StaleCompletion;
        if outcome == StartOutcome::Succeeded {
            ensure_compensating_stop(state, effect_id, run_id, effects);
            *phase = TracePhase::CompensatingStop;
        }
        return;
    }

    match outcome {
        StartOutcome::Succeeded => {
            state.capture = CaptureState::Recording { run };
            clear_recoverable_fault(state);
        }
        StartOutcome::Cancelled => {
            state.capture = CaptureState::Idle;
            state.terminal_status_run = Some(run_id);
        }
        StartOutcome::Failed(error) => {
            state.capture = CaptureState::Idle;
            state.terminal_status_run = Some(run_id);
            if state.desired_recording.revision() == Some(run.revision) {
                state.blocked_start_revision = Some(run.revision);
                set_recoverable_fault(
                    state,
                    CoordinatorFault::StartFailed {
                        revision: run.revision,
                        error,
                    },
                );
            }
        }
    }
}

fn ensure_compensating_stop(
    state: &mut CoordinatorState,
    originating_effect: EffectId,
    run_id: RunId,
    effects: &mut Vec<CoordinatorEffect>,
) {
    if state.capture.run().map(|run| run.run_id) == Some(run_id)
        || state.processing_jobs.contains_key(&run_id)
        || !state
            .mismatched_start_cleanups
            .insert((originating_effect, run_id))
    {
        return;
    }
    let effect_id = state.next_effect();
    let attempt = 1;
    state.processing_jobs.insert(
        run_id,
        ProcessingJob {
            run_id,
            state: ProcessingState::CompensatingStop {
                effect_id,
                attempts: attempt,
            },
        },
    );
    register_effect(
        state,
        effect_id,
        PendingEffect::Stop {
            run_id,
            role: StopRole::Compensating,
            attempt,
            reason: StopReason::CompensatingStaleStart,
        },
    );
    effects.push(CoordinatorEffect::StopRecording {
        effect_id,
        run_id,
        reason: StopReason::CompensatingStaleStart,
        attempt,
    });
}

fn apply_capture_stopped(
    state: &mut CoordinatorState,
    effect_id: EffectId,
    run_id: RunId,
    outcome: CaptureStopOutcome,
    effects: &mut Vec<CoordinatorEffect>,
    phase: &mut TracePhase,
) {
    if state.completed_effects.contains_key(&effect_id) {
        *phase = TracePhase::DuplicateCompletion;
        return;
    }
    let Some(pending) = state.in_flight.get(&effect_id).copied() else {
        *phase = TracePhase::StaleCompletion;
        return;
    };
    let PendingEffect::Stop {
        run_id: expected_run,
        role,
        attempt,
        reason,
    } = pending
    else {
        *phase = TracePhase::StaleCompletion;
        return;
    };
    if expected_run != run_id {
        *phase = TracePhase::StaleCompletion;
        return;
    }

    state.in_flight.remove(&effect_id);
    state.remember_completion(effect_id, CompletedEffect::Stop { run_id, outcome });
    match role {
        StopRole::ActiveCapture => {
            apply_active_capture_stop(state, run_id, outcome, attempt, reason, effects, phase)
        }
        StopRole::Compensating => apply_compensating_stop(state, run_id, outcome, attempt, effects),
    }
}

fn apply_active_capture_stop(
    state: &mut CoordinatorState,
    run_id: RunId,
    outcome: CaptureStopOutcome,
    attempt: u8,
    reason: StopReason,
    effects: &mut Vec<CoordinatorEffect>,
    phase: &mut TracePhase,
) {
    let run = match state.capture {
        CaptureState::Stopping { run, .. } | CaptureState::StopUncertain { run, .. }
            if run.run_id == run_id =>
        {
            run
        }
        _ => {
            *phase = TracePhase::StaleCompletion;
            return;
        }
    };
    match outcome {
        CaptureStopOutcome::Inactive | CaptureStopOutcome::FailedButInactive(_) => {
            state.capture = CaptureState::Idle;
            clear_recoverable_fault(state);
            begin_finalize(state, run_id, 1, effects);
        }
        CaptureStopOutcome::StillActive(error) => {
            let next_attempt = attempt.saturating_add(1);
            if next_attempt <= run.policy.max_stop_attempts {
                let next_effect = state.next_effect();
                state.capture = CaptureState::StopUncertain {
                    run,
                    active_effect: Some(next_effect),
                    attempts: next_attempt,
                    reason,
                    error,
                };
                register_effect(
                    state,
                    next_effect,
                    PendingEffect::Stop {
                        run_id,
                        role: StopRole::ActiveCapture,
                        attempt: next_attempt,
                        reason,
                    },
                );
                effects.push(CoordinatorEffect::StopRecording {
                    effect_id: next_effect,
                    run_id,
                    reason,
                    attempt: next_attempt,
                });
            } else {
                state.capture = CaptureState::StopUncertain {
                    run,
                    active_effect: None,
                    attempts: attempt,
                    reason,
                    error,
                };
                state.fault = Some(CoordinatorFault::StopUncertain { run_id, error });
                effects.push(CoordinatorEffect::ReleaseTranscriptBarrier { run_id });
            }
        }
    }
}

fn apply_compensating_stop(
    state: &mut CoordinatorState,
    run_id: RunId,
    outcome: CaptureStopOutcome,
    attempt: u8,
    effects: &mut Vec<CoordinatorEffect>,
) {
    match outcome {
        CaptureStopOutcome::Inactive | CaptureStopOutcome::FailedButInactive(_) => {
            state.processing_jobs.remove(&run_id);
            begin_finalize(state, run_id, 1, effects);
        }
        CaptureStopOutcome::StillActive(error) => {
            let next_attempt = attempt.saturating_add(1);
            if next_attempt <= state.current_policy.max_stop_attempts {
                let next_effect = state.next_effect();
                state.processing_jobs.insert(
                    run_id,
                    ProcessingJob {
                        run_id,
                        state: ProcessingState::StopUncertain {
                            effect_id: next_effect,
                            attempts: next_attempt,
                            error,
                        },
                    },
                );
                register_effect(
                    state,
                    next_effect,
                    PendingEffect::Stop {
                        run_id,
                        role: StopRole::Compensating,
                        attempt: next_attempt,
                        reason: StopReason::CompensatingStaleStart,
                    },
                );
                effects.push(CoordinatorEffect::StopRecording {
                    effect_id: next_effect,
                    run_id,
                    reason: StopReason::CompensatingStaleStart,
                    attempt: next_attempt,
                });
            } else {
                state.processing_jobs.remove(&run_id);
                state.terminal_status_run = Some(run_id);
                state.fault = Some(CoordinatorFault::StopUncertain { run_id, error });
                effects.push(CoordinatorEffect::ReleaseTranscriptBarrier { run_id });
            }
        }
    }
}

fn begin_finalize(
    state: &mut CoordinatorState,
    run_id: RunId,
    attempt: u8,
    effects: &mut Vec<CoordinatorEffect>,
) {
    if state.processing_jobs.contains_key(&run_id) {
        return;
    }
    let effect_id = state.next_effect();
    state.processing_jobs.insert(
        run_id,
        ProcessingJob {
            run_id,
            state: ProcessingState::Finalizing {
                effect_id,
                attempts: attempt,
            },
        },
    );
    register_effect(
        state,
        effect_id,
        PendingEffect::Finalize { run_id, attempt },
    );
    effects.push(CoordinatorEffect::FinalizeRecording {
        effect_id,
        run_id,
        attempt,
    });
}

fn apply_finalize_finished(
    state: &mut CoordinatorState,
    effect_id: EffectId,
    run_id: RunId,
    outcome: FinalizeOutcome,
    effects: &mut Vec<CoordinatorEffect>,
    phase: &mut TracePhase,
) {
    if state.completed_effects.contains_key(&effect_id) {
        *phase = TracePhase::DuplicateCompletion;
        return;
    }
    let Some(PendingEffect::Finalize {
        run_id: expected_run,
        attempt,
    }) = state.in_flight.get(&effect_id).copied()
    else {
        *phase = TracePhase::StaleCompletion;
        return;
    };
    if expected_run != run_id
        || !matches!(
            state.processing_jobs.get(&run_id),
            Some(ProcessingJob {
                state: ProcessingState::Finalizing { effect_id: current, .. },
                ..
            }) if *current == effect_id
        )
    {
        *phase = TracePhase::StaleCompletion;
        return;
    }
    state.in_flight.remove(&effect_id);
    state.remember_completion(effect_id, CompletedEffect::Finalize { run_id, outcome });

    match outcome {
        FinalizeOutcome::Committed | FinalizeOutcome::NoTranscript => {
            state.processing_jobs.remove(&run_id);
            state.terminal_status_run = Some(run_id);
        }
        FinalizeOutcome::Failed(error) => {
            let next_attempt = attempt.saturating_add(1);
            if next_attempt <= state.current_policy.max_finalize_attempts {
                state.processing_jobs.remove(&run_id);
                begin_finalize(state, run_id, next_attempt, effects);
            } else {
                state.processing_jobs.remove(&run_id);
                state.terminal_status_run = Some(run_id);
                set_recoverable_fault(state, CoordinatorFault::FinalizeFailed { run_id, error });
                state.blocked_start_revision = state.desired_recording.revision();
                effects.push(CoordinatorEffect::ReleaseTranscriptBarrier { run_id });
            }
        }
    }
}

fn apply_window_finished(
    state: &mut CoordinatorState,
    effect_id: EffectId,
    outcome: WindowOutcome,
    phase: &mut TracePhase,
) {
    if state.completed_effects.contains_key(&effect_id) {
        *phase = TracePhase::DuplicateCompletion;
        return;
    }
    let Some(pending) = state.in_flight.get(&effect_id).copied() else {
        *phase = TracePhase::StaleCompletion;
        return;
    };
    let panel_matches = matches!(
        (state.panel, pending),
        (
            PanelState::Showing { effect_id: current, .. },
            PendingEffect::ShowPanel { .. }
        ) if current == effect_id
    ) || matches!(
        (state.panel, pending),
        (
            PanelState::Hiding { effect_id: current, .. },
            PendingEffect::HidePanel { .. }
        ) if current == effect_id
    );
    if !panel_matches {
        *phase = TracePhase::StaleCompletion;
        return;
    }

    state.in_flight.remove(&effect_id);
    state.remember_completion(effect_id, CompletedEffect::Window { outcome });
    match (pending, outcome) {
        (PendingEffect::ShowPanel { .. }, WindowOutcome::Applied { window_epoch }) => {
            state.panel = PanelState::Shown { window_epoch };
            state.panel_failure = None;
        }
        (PendingEffect::ShowPanel { revision, attempt }, WindowOutcome::Failed(_)) => {
            state.panel = PanelState::Hidden;
            state.panel_failure = Some(PanelFailure {
                goal: PanelGoal::Shown,
                revision,
                attempts: attempt,
            });
        }
        (PendingEffect::HidePanel { .. }, WindowOutcome::Applied { .. }) => {
            state.panel = PanelState::Hidden;
            state.panel_failure = None;
        }
        (PendingEffect::ShowPanel { .. }, WindowOutcome::Superseded { window_epoch }) => {
            state.panel = PanelState::Shown { window_epoch };
            state.panel_failure = None;
        }
        (
            PendingEffect::HidePanel {
                revision,
                previous_epoch,
                attempt,
            },
            WindowOutcome::Failed(_),
        ) => {
            state.panel = PanelState::Shown {
                window_epoch: previous_epoch,
            };
            state.panel_failure = Some(PanelFailure {
                goal: PanelGoal::Hidden,
                revision,
                attempts: attempt,
            });
        }
        (
            PendingEffect::HidePanel {
                revision, attempt, ..
            },
            WindowOutcome::Superseded { window_epoch },
        ) => {
            state.panel = PanelState::Shown { window_epoch };
            state.panel_failure = Some(PanelFailure {
                goal: PanelGoal::Hidden,
                revision,
                attempts: attempt,
            });
        }
        _ => {
            *phase = TracePhase::StaleCompletion;
        }
    }
}

fn reconcile_capture(state: &mut CoordinatorState, effects: &mut Vec<CoordinatorEffect>) {
    if state.shutdown_requested {
        state.desired_recording = DesiredRecording::Off;
    }
    match (state.desired_recording, state.capture) {
        (DesiredRecording::On { revision, source }, CaptureState::Idle)
            if state.blocked_start_revision != Some(revision)
                && state.processing_jobs.is_empty()
                && !state
                    .fault
                    .is_some_and(CoordinatorFault::blocks_capture_start) =>
        {
            let run = RunContext {
                run_id: state.ids.run_id(),
                revision,
                source,
                policy: state.current_policy,
            };
            let effect_id = state.next_effect();
            state.terminal_status_run = None;
            state.capture = CaptureState::Starting {
                run,
                effect_id,
                cancel_requested: false,
            };
            register_effect(state, effect_id, PendingEffect::Start { run });
            effects.push(CoordinatorEffect::StartRecording { effect_id, run });
        }
        (
            DesiredRecording::Off,
            CaptureState::Starting {
                run,
                effect_id,
                cancel_requested: false,
            },
        ) => {
            state.capture = CaptureState::Starting {
                run,
                effect_id,
                cancel_requested: true,
            };
            effects.push(CoordinatorEffect::CancelStart {
                effect_id,
                run_id: run.run_id,
            });
        }
        (DesiredRecording::Off, CaptureState::Recording { run }) => {
            start_active_stop(state, run, state.desired_stop_reason, effects);
        }
        _ => {}
    }
}

fn start_active_stop(
    state: &mut CoordinatorState,
    run: RunContext,
    reason: StopReason,
    effects: &mut Vec<CoordinatorEffect>,
) {
    let effect_id = state.next_effect();
    let attempt = 1;
    state.capture = CaptureState::Stopping {
        run,
        effect_id,
        attempts: attempt,
        reason,
    };
    register_effect(
        state,
        effect_id,
        PendingEffect::Stop {
            run_id: run.run_id,
            role: StopRole::ActiveCapture,
            attempt,
            reason,
        },
    );
    effects.push(CoordinatorEffect::StopRecording {
        effect_id,
        run_id: run.run_id,
        reason,
        attempt,
    });
}

fn reconcile_panel(state: &mut CoordinatorState, effects: &mut Vec<CoordinatorEffect>) {
    match (state.desired_panel, state.panel) {
        (PanelGoal::Shown, PanelState::Hidden) => {
            let revision = state
                .desired_recording
                .revision()
                .unwrap_or(IntentRevision::new(state.ids.intent));
            let source = match state.desired_recording {
                DesiredRecording::On { source, .. } => source,
                DesiredRecording::Off => IntentSource::Frontend,
            };
            let Some(attempt) = next_panel_attempt(state, PanelGoal::Shown, revision) else {
                return;
            };
            let effect_id = state.next_effect();
            state.panel = PanelState::Showing {
                effect_id,
                revision,
            };
            register_effect(
                state,
                effect_id,
                PendingEffect::ShowPanel { revision, attempt },
            );
            effects.push(CoordinatorEffect::ShowPanel {
                effect_id,
                revision,
                source,
                policy: state.current_policy,
            });
        }
        (PanelGoal::Hidden, PanelState::Shown { window_epoch }) => {
            let revision = IntentRevision::new(state.ids.intent);
            let Some(attempt) = next_panel_attempt(state, PanelGoal::Hidden, revision) else {
                return;
            };
            let effect_id = state.next_effect();
            state.panel = PanelState::Hiding {
                effect_id,
                revision,
                previous_epoch: window_epoch,
            };
            register_effect(
                state,
                effect_id,
                PendingEffect::HidePanel {
                    revision,
                    previous_epoch: window_epoch,
                    attempt,
                },
            );
            effects.push(CoordinatorEffect::HidePanel {
                effect_id,
                revision,
                window_epoch,
                reason: if state.shutdown_requested {
                    StopReason::Shutdown
                } else {
                    StopReason::Hotkey
                },
            });
        }
        _ => {}
    }
}

fn next_panel_attempt(
    state: &CoordinatorState,
    goal: PanelGoal,
    revision: IntentRevision,
) -> Option<u8> {
    match state.panel_failure {
        Some(failure) if failure.goal == goal && failure.revision == revision => {
            let next = failure.attempts.saturating_add(1);
            (next <= MAX_PANEL_ATTEMPTS).then_some(next)
        }
        _ => Some(1),
    }
}

fn reconcile_projection(state: &mut CoordinatorState, effects: &mut Vec<CoordinatorEffect>) {
    let projection = state.projection();
    if state.last_projection != Some(projection) {
        state.last_projection = Some(projection);
        effects.push(CoordinatorEffect::EmitProjection(projection));
    }
}

fn reconcile_shutdown(state: &mut CoordinatorState, effects: &mut Vec<CoordinatorEffect>) {
    let ready = state.shutdown_requested
        && matches!(
            state.capture,
            CaptureState::Idle
                | CaptureState::StopUncertain {
                    active_effect: None,
                    ..
                }
        )
        && state.processing_jobs.is_empty();
    if ready && !state.shutdown_ready_emitted {
        state.shutdown_ready_emitted = true;
        effects.push(CoordinatorEffect::ShutdownReady);
    } else if !ready {
        state.shutdown_ready_emitted = false;
    }
}

fn register_effect(state: &mut CoordinatorState, effect_id: EffectId, pending: PendingEffect) {
    let previous = state.in_flight.insert(effect_id, pending);
    debug_assert!(previous.is_none());
    debug_assert!(!state.completed_effects.contains_key(&effect_id));
}

fn trace_enqueued_effects(state: &mut CoordinatorState, effects: &[CoordinatorEffect]) {
    let desired = state.desired_recording;
    let capture = state.capture.phase();
    let event_sequence = state.event_sequence;
    let monotonic_ns = state.last_monotonic_ns;
    for effect in effects {
        let (phase, run_id, window_epoch, reason) = match *effect {
            CoordinatorEffect::StartRecording { run, .. } => {
                (TracePhase::StartEnqueued, Some(run.run_id), None, None)
            }
            CoordinatorEffect::CancelStart { run_id, .. } => {
                (TracePhase::EffectEnqueued, Some(run_id), None, None)
            }
            CoordinatorEffect::FinalizeRecording { run_id, .. } => {
                (TracePhase::FinalizeStarted, Some(run_id), None, None)
            }
            CoordinatorEffect::StopRecording { run_id, reason, .. } => (
                if reason == StopReason::CompensatingStaleStart {
                    TracePhase::CompensatingStop
                } else {
                    TracePhase::CaptureStopEnqueued
                },
                Some(run_id),
                None,
                Some(reason),
            ),
            CoordinatorEffect::ShowPanel { .. } => (TracePhase::WindowEnqueued, None, None, None),
            CoordinatorEffect::HidePanel {
                window_epoch,
                reason,
                ..
            } => (
                TracePhase::WindowEnqueued,
                None,
                Some(window_epoch),
                Some(reason),
            ),
            CoordinatorEffect::ReleaseTranscriptBarrier { .. }
            | CoordinatorEffect::EmitProjection(_)
            | CoordinatorEffect::ShutdownReady => (TracePhase::EffectEnqueued, None, None, None),
        };
        if effect.effect_id().is_none() {
            continue;
        }
        state.push_trace(TraceEntry {
            event_sequence,
            monotonic_ns,
            phase,
            source: None,
            gesture_id: None,
            intent_revision: desired.revision(),
            run_id,
            effect_id: effect.effect_id(),
            window_epoch,
            desired_before: desired,
            desired_after: desired,
            capture_before: capture,
            capture_after: capture,
            reason,
            outcome: None,
            error: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(kind: IntentKind, source: IntentSource, gesture: u64) -> CoordinatorEvent {
        CoordinatorEvent::Intent(RecordingIntent {
            kind,
            source,
            gesture_id: Some(GestureId::new(gesture)),
            expected_run_id: None,
        })
    }

    fn find_start(effects: &[CoordinatorEffect]) -> (EffectId, RunContext) {
        effects
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::StartRecording { effect_id, run } => Some((*effect_id, *run)),
                _ => None,
            })
            .expect("expected StartRecording")
    }

    fn find_stop(effects: &[CoordinatorEffect]) -> (EffectId, RunId) {
        effects
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::StopRecording {
                    effect_id, run_id, ..
                } => Some((*effect_id, *run_id)),
                _ => None,
            })
            .expect("expected StopRecording")
    }

    fn find_finalize(effects: &[CoordinatorEffect]) -> (EffectId, RunId) {
        effects
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::FinalizeRecording {
                    effect_id, run_id, ..
                } => Some((*effect_id, *run_id)),
                _ => None,
            })
            .expect("expected FinalizeRecording")
    }

    fn complete_start(
        state: &mut CoordinatorState,
        effect_id: EffectId,
        run_id: RunId,
    ) -> Vec<CoordinatorEffect> {
        reduce(
            state,
            CoordinatorEvent::StartFinished {
                effect_id,
                run_id,
                outcome: StartOutcome::Succeeded,
            },
        )
    }

    #[test]
    fn idle_to_on_dispatches_panel_and_start_without_ordering_them() {
        let mut state = CoordinatorState::default();
        let effects = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 1),
        );
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::ShowPanel { .. })));
        assert!(matches!(state.capture, CaptureState::Starting { .. }));
        assert_eq!(state.desired_panel, PanelGoal::Shown);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn two_distinct_gestures_have_no_time_based_debounce() {
        let mut state = CoordinatorState::default();
        reduce_at(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 1),
            1,
        );
        reduce_at(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 2),
            20_000_001,
        );
        assert_eq!(state.desired_recording, DesiredRecording::Off);
    }

    #[test]
    fn matching_hold_release_is_accepted_once_and_stale_release_is_rejected() {
        let mut state = CoordinatorState::default();
        let started = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::HoldHotkey, 1),
        );
        let (start_id, run) = find_start(&started);
        complete_start(&mut state, start_id, run.run_id);

        let stopped = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::HoldHotkey, 1),
        );
        assert_eq!(state.desired_recording, DesiredRecording::Off);
        assert!(stopped
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StopRecording { .. })));

        let duplicate = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::HoldHotkey, 1),
        );
        assert!(duplicate.is_empty());
        assert_eq!(
            state.trace().last().unwrap().phase,
            TracePhase::IntentRejected
        );

        reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::HoldHotkey, 2),
        );
        reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::HoldHotkey, 1),
        );
        assert!(state.desired_recording.is_on());
        assert_eq!(
            state.trace().last().unwrap().phase,
            TracePhase::IntentRejected
        );
    }

    #[test]
    fn duplicate_gesture_is_rejected_without_mutation() {
        let mut state = CoordinatorState::default();
        reduce(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 7),
        );
        let desired = state.desired_recording;
        let effects = reduce(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 7),
        );
        assert_eq!(state.desired_recording, desired);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::StartRecording { .. } | CoordinatorEffect::StopRecording { .. }
        )));
        assert_eq!(
            state.trace().last().unwrap().phase,
            TracePhase::IntentRejected
        );
    }

    #[test]
    fn explicit_start_while_recording_only_reissues_the_panel_effect() {
        let mut state = CoordinatorState::default();
        let initial = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        let (start_id, run) = find_start(&initial);
        let show_id = initial
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::ShowPanel { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .expect("expected initial ShowPanel");
        complete_start(&mut state, start_id, run.run_id);
        reduce(
            &mut state,
            CoordinatorEvent::WindowFinished {
                effect_id: show_id,
                outcome: WindowOutcome::Applied { window_epoch: 7 },
            },
        );

        let duplicate = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );

        assert_eq!(
            state.capture.run().map(|active| active.run_id),
            Some(run.run_id)
        );
        assert!(!duplicate
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        assert!(duplicate
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::ShowPanel { .. })));
    }

    #[test]
    fn toggle_restart_reissues_show_after_out_of_band_auto_hide() {
        let mut policy = RuntimePolicySnapshot::default();
        policy.hide_panel_on_hotkey_stop = false;
        let mut state = CoordinatorState::new(policy);

        let initial = reduce(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 1),
        );
        let (start_id, run) = find_start(&initial);
        let show_id = initial
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::ShowPanel { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .expect("expected initial ShowPanel");
        complete_start(&mut state, start_id, run.run_id);
        reduce(
            &mut state,
            CoordinatorEvent::WindowFinished {
                effect_id: show_id,
                outcome: WindowOutcome::Applied { window_epoch: 7 },
            },
        );

        let stop = reduce(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 2),
        );
        assert!(!stop
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::HidePanel { .. })));
        let (stop_id, _) = find_stop(&stop);
        let finalizing = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        let (finalize_id, _) = find_finalize(&finalizing);
        reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalize_id,
                run_id: run.run_id,
                outcome: FinalizeOutcome::NoTranscript,
            },
        );

        // The frontend may now hide the native window without mutating this
        // pure reducer, so its last completed panel state is intentionally stale.
        assert!(matches!(state.panel, PanelState::Shown { window_epoch: 7 }));
        let restarted = reduce(
            &mut state,
            intent(IntentKind::Toggle, IntentSource::CarbonHotkey, 3),
        );

        assert!(restarted
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        assert!(restarted
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::ShowPanel { .. })));
    }

    #[test]
    fn expected_run_fence_rejects_a_stale_direct_stop() {
        let mut state = recording_state();
        let active_run = state.capture.run().unwrap().run_id;

        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop_expected(
                IntentSource::Frontend,
                None,
                Some(RunId::new(active_run.get() + 1)),
            )),
        );

        assert!(state.desired_recording.is_on());
        assert_eq!(state.capture.run().map(|run| run.run_id), Some(active_run));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StopRecording { .. })));
        assert_eq!(
            state.trace().last().unwrap().phase,
            TracePhase::IntentRejected
        );
    }

    #[test]
    fn late_start_after_off_is_stopped_exactly_once() {
        let mut state = CoordinatorState::default();
        let start_effects = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 1),
        );
        let (start_id, run) = find_start(&start_effects);
        let off_effects = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 2),
        );
        assert!(off_effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::CancelStart { effect_id, .. } if *effect_id == start_id
        )));

        let completion = complete_start(&mut state, start_id, run.run_id);
        let stop = find_stop(&completion);
        let duplicate = complete_start(&mut state, start_id, run.run_id);
        assert_eq!(stop.1, run.run_id);
        assert!(!duplicate
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StopRecording { .. })));
    }

    #[test]
    fn retained_on_waits_for_old_transcript_barrier_before_starting_new_run() {
        let mut state = CoordinatorState::default();
        let initial = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 1),
        );
        let (start_id, run) = find_start(&initial);
        complete_start(&mut state, start_id, run.run_id);
        let stop_request = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 2),
        );
        let (stop_id, _) = find_stop(&stop_request);
        reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 3),
        );

        let effects = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::FinalizeRecording { run_id, .. } if *run_id == run.run_id
        )));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        assert!(state.processing_jobs.contains_key(&run.run_id));

        let (finalize_id, _) = find_finalize(&effects);
        let finalized = reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalize_id,
                run_id: run.run_id,
                outcome: FinalizeOutcome::Committed,
            },
        );
        let (_, replacement) = find_start(&finalized);
        assert_ne!(replacement.run_id, run.run_id);
        assert!(!state.processing_jobs.contains_key(&run.run_id));
    }

    #[test]
    fn pending_start_has_no_three_five_or_sixty_second_deadline() {
        let mut state = recording_state();
        reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 2),
        );
        reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 3),
        );
        let policy = state.current_policy;

        for elapsed_ns in [3_000_000_000, 5_000_000_000, 60_000_000_000] {
            let effects = reduce_at(
                &mut state,
                CoordinatorEvent::PolicyUpdated(policy),
                elapsed_ns,
            );
            assert!(state.desired_recording.is_on());
            assert!(matches!(state.capture, CaptureState::Stopping { .. }));
            assert!(state.projection().pending_start);
            assert!(!effects
                .iter()
                .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        }
    }

    #[test]
    fn on_then_off_while_stopping_does_not_restart() {
        let mut state = recording_state();
        let run = state.capture.run().unwrap();
        let stop_effects = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 2),
        );
        let (stop_id, _) = find_stop(&stop_effects);
        reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 3),
        );
        reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 4),
        );
        let completed = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        assert!(!completed
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        assert_eq!(state.desired_recording, DesiredRecording::Off);
    }

    #[test]
    fn stop_failed_but_inactive_releases_capture_slot() {
        let mut state = recording_state();
        let run = state.capture.run().unwrap();
        let stop_effects = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::Frontend, 2),
        );
        let (stop_id, _) = find_stop(&stop_effects);
        let effects = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::FailedButInactive(ErrorCode(10)),
            },
        );
        assert!(matches!(state.capture, CaptureState::Idle));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::FinalizeRecording { run_id, .. } if *run_id == run.run_id
        )));
    }

    #[test]
    fn still_active_enters_bounded_stop_uncertain_retries() {
        let mut policy = RuntimePolicySnapshot::default();
        policy.max_stop_attempts = 2;
        let mut state = CoordinatorState::new(policy);
        let initial = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 1),
        );
        let (start_id, run) = find_start(&initial);
        complete_start(&mut state, start_id, run.run_id);
        let stop_effects = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::Frontend, 2),
        );
        let (first_stop, _) = find_stop(&stop_effects);
        let retry = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: first_stop,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::StillActive(ErrorCode(20)),
            },
        );
        let (second_stop, _) = find_stop(&retry);
        let exhausted = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: second_stop,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::StillActive(ErrorCode(21)),
            },
        );
        assert!(matches!(
            state.capture,
            CaptureState::StopUncertain {
                active_effect: None,
                attempts: 2,
                ..
            }
        ));
        assert!(!exhausted
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StopRecording { .. })));
        assert!(exhausted.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::ReleaseTranscriptBarrier { run_id }
                if *run_id == run.run_id
        )));
        assert_eq!(
            state.projection().fault,
            Some(ProjectionFault::StopUncertain)
        );
        assert!(!state.projection().pending_start);
    }

    #[test]
    fn terminal_compensating_stop_is_removed_but_blocks_future_capture() {
        let mut policy = RuntimePolicySnapshot::default();
        policy.max_stop_attempts = 1;
        let mut state = CoordinatorState::new(policy);
        let cleanup = reduce(
            &mut state,
            CoordinatorEvent::StartFinished {
                effect_id: EffectId::new(999),
                run_id: RunId::new(77),
                outcome: StartOutcome::Succeeded,
            },
        );
        let (stop_id, run_id) = find_stop(&cleanup);
        let terminal = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id,
                outcome: CaptureStopOutcome::StillActive(ErrorCode(31)),
            },
        );

        assert!(state.processing_jobs.is_empty());
        assert_eq!(
            state.projection().fault,
            Some(ProjectionFault::StopUncertain)
        );
        assert!(terminal.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::ReleaseTranscriptBarrier { run_id: released }
                if *released == run_id
        )));

        let requested = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        assert!(!requested
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
    }

    #[test]
    fn compensating_stop_safety_fault_survives_unrelated_capture_cleanup() {
        let mut policy = RuntimePolicySnapshot::default();
        policy.max_stop_attempts = 1;
        let mut state = CoordinatorState::new(policy);
        let initial = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 1),
        );
        let (start_id, current) = find_start(&initial);
        complete_start(&mut state, start_id, current.run_id);

        let stale_cleanup = reduce(
            &mut state,
            CoordinatorEvent::StartFinished {
                effect_id: EffectId::new(999),
                run_id: RunId::new(77),
                outcome: StartOutcome::Succeeded,
            },
        );
        let (stale_stop_id, stale_run) = find_stop(&stale_cleanup);
        reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stale_stop_id,
                run_id: stale_run,
                outcome: CaptureStopOutcome::StillActive(ErrorCode(32)),
            },
        );
        assert_eq!(
            state.projection().fault,
            Some(ProjectionFault::StopUncertain)
        );

        let current_stop = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::Frontend, 2),
        );
        let (current_stop_id, _) = find_stop(&current_stop);
        reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: current_stop_id,
                run_id: current.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );

        assert_eq!(
            state.projection().fault,
            Some(ProjectionFault::StopUncertain)
        );
    }

    #[test]
    fn terminal_finalize_failure_releases_job_and_requires_new_revision() {
        let mut policy = RuntimePolicySnapshot::default();
        policy.max_finalize_attempts = 1;
        let mut state = CoordinatorState::new(policy);
        let initial = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 1),
        );
        let (start_id, run) = find_start(&initial);
        complete_start(&mut state, start_id, run.run_id);
        let stopping = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::Frontend, 2),
        );
        let (stop_id, _) = find_stop(&stopping);
        reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 3),
        );
        let stopped = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        let (finalize_id, _) = find_finalize(&stopped);
        let terminal = reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalize_id,
                run_id: run.run_id,
                outcome: FinalizeOutcome::Failed(ErrorCode(41)),
            },
        );

        assert!(state.processing_jobs.is_empty());
        assert_eq!(
            state.projection().fault,
            Some(ProjectionFault::FinalizeFailed)
        );
        assert!(!terminal
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
        assert!(terminal.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::ReleaseTranscriptBarrier { run_id }
                if *run_id == run.run_id
        )));

        let retry = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        assert!(retry
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
    }

    #[test]
    fn stale_start_success_uses_run_scoped_compensating_stop() {
        let mut state = CoordinatorState::default();
        let effects = reduce(
            &mut state,
            CoordinatorEvent::StartFinished {
                effect_id: EffectId::new(999),
                run_id: RunId::new(77),
                outcome: StartOutcome::Succeeded,
            },
        );
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::StopRecording {
                run_id,
                reason: StopReason::CompensatingStaleStart,
                ..
            } if *run_id == RunId::new(77)
        )));
        assert!(matches!(state.capture, CaptureState::Idle));
    }

    #[test]
    fn stale_finalize_never_mutates_new_capture_or_panel_goal() {
        let mut state = recording_state();
        state.desired_panel = PanelGoal::Shown;
        let capture = state.capture;
        let effects = reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: EffectId::new(500),
                run_id: RunId::new(400),
                outcome: FinalizeOutcome::Committed,
            },
        );
        assert_eq!(state.capture, capture);
        assert_eq!(state.desired_panel, PanelGoal::Shown);
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
    }

    #[test]
    fn vad_force_off_cannot_restart_recording() {
        let mut state = recording_state();
        let effects = reduce(
            &mut state,
            CoordinatorEvent::ForceOff(StopReason::VadTimeout),
        );
        assert_eq!(state.desired_recording, DesiredRecording::Off);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::StopRecording {
                reason: StopReason::VadTimeout,
                ..
            }
        )));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StartRecording { .. })));
    }

    #[test]
    fn stale_vad_stop_cannot_turn_off_a_newer_run() {
        let mut state = recording_state();
        let active_run = state.capture.run().unwrap().run_id;
        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop_expected(
                IntentSource::Vad,
                None,
                Some(RunId::new(active_run.get().saturating_sub(1))),
            )),
        );

        assert!(state.desired_recording.is_on());
        assert_eq!(state.capture.run().map(|run| run.run_id), Some(active_run));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::StopRecording { .. })));
        assert_eq!(
            state.trace().last().unwrap().phase,
            TracePhase::IntentRejected
        );
    }

    #[test]
    fn reversed_panel_completion_reconciles_latest_goal() {
        let mut state = CoordinatorState::default();
        let on = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 1),
        );
        let show_id = on
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::ShowPanel { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 2),
        );
        let after_show = reduce(
            &mut state,
            CoordinatorEvent::WindowFinished {
                effect_id: show_id,
                outcome: WindowOutcome::Applied { window_epoch: 9 },
            },
        );
        assert!(after_show
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::HidePanel { .. })));
    }

    #[test]
    fn persistent_panel_failure_retries_once_without_recursive_loop() {
        let mut state = CoordinatorState::default();
        let initial = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::CarbonHotkey, 1),
        );
        let first_show = initial
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::ShowPanel { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let retry = reduce(
            &mut state,
            CoordinatorEvent::WindowFinished {
                effect_id: first_show,
                outcome: WindowOutcome::Failed(ErrorCode(50)),
            },
        );
        let second_show = retry
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::ShowPanel { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .expect("one bounded retry");
        let exhausted = reduce(
            &mut state,
            CoordinatorEvent::WindowFinished {
                effect_id: second_show,
                outcome: WindowOutcome::Failed(ErrorCode(51)),
            },
        );
        assert!(!exhausted
            .iter()
            .any(|effect| matches!(effect, CoordinatorEffect::ShowPanel { .. })));
        assert!(matches!(state.panel, PanelState::Hidden));
    }

    #[test]
    fn duplicate_completion_is_idempotent() {
        let mut state = CoordinatorState::default();
        let initial = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 1),
        );
        let (start_id, run) = find_start(&initial);
        complete_start(&mut state, start_id, run.run_id);
        let before = state.capture;
        let duplicate = complete_start(&mut state, start_id, run.run_id);
        assert_eq!(state.capture, before);
        assert!(!duplicate.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::StartRecording { .. } | CoordinatorEffect::StopRecording { .. }
        )));
    }

    #[test]
    fn terminal_idle_projection_keeps_the_completed_run_identity() {
        let mut state = recording_state();
        let run_id = state.capture.run().unwrap().run_id;
        let stop_effects = reduce(
            &mut state,
            intent(IntentKind::Stop, IntentSource::CarbonHotkey, 2),
        );
        let (stop_id, _) = find_stop(&stop_effects);
        let stopped = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        let finalize_id = stopped
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();

        let finalized = reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalize_id,
                run_id,
                outcome: FinalizeOutcome::Committed,
            },
        );
        assert!(finalized.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::EmitProjection(RecordingStatusProjection {
                status: ProjectionStatus::Idle,
                status_run: Some(status_run),
                stopped_via_hotkey: true,
                ..
            }) if *status_run == run_id
        )));
    }

    #[test]
    fn shutdown_forces_off_and_emits_ready_only_after_terminal_cleanup() {
        let mut idle = CoordinatorState::default();
        let effects = reduce(&mut idle, CoordinatorEvent::ShutdownRequested);
        assert!(effects.contains(&CoordinatorEffect::ShutdownReady));
        assert_eq!(idle.desired_panel, PanelGoal::Hidden);

        let mut active = recording_state();
        let effects = reduce(&mut active, CoordinatorEvent::ShutdownRequested);
        assert!(!effects.contains(&CoordinatorEffect::ShutdownReady));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            CoordinatorEffect::StopRecording {
                reason: StopReason::Shutdown,
                ..
            }
        )));
    }

    #[test]
    fn shutdown_completes_after_terminal_stop_uncertain() {
        let mut policy = RuntimePolicySnapshot::default();
        policy.max_stop_attempts = 1;
        let mut state = CoordinatorState::new(policy);
        let start_effects = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 1),
        );
        let (start_id, run) = find_start(&start_effects);
        complete_start(&mut state, start_id, run.run_id);

        let shutdown_effects = reduce(&mut state, CoordinatorEvent::ShutdownRequested);
        let (stop_id, _) = find_stop(&shutdown_effects);
        let terminal_effects = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop_id,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::StillActive(ErrorCode(90)),
            },
        );

        assert!(terminal_effects.contains(&CoordinatorEffect::ShutdownReady));
        assert!(terminal_effects
            .contains(&CoordinatorEffect::ReleaseTranscriptBarrier { run_id: run.run_id }));
    }

    #[test]
    fn trace_is_bounded_and_contains_no_payload_fields() {
        let mut state = CoordinatorState::with_trace_capacity(RuntimePolicySnapshot::default(), 4);
        for gesture in 1..=20 {
            reduce(
                &mut state,
                intent(IntentKind::Toggle, IntentSource::CarbonHotkey, gesture),
            );
        }
        assert_eq!(state.trace().len(), 4);
        assert!(state.trace().all(|entry| entry.monotonic_ns > 0));
    }

    fn recording_state() -> CoordinatorState {
        let mut state = CoordinatorState::default();
        let effects = reduce(
            &mut state,
            intent(IntentKind::Start, IntentSource::Frontend, 1),
        );
        let (effect_id, run) = find_start(&effects);
        complete_start(&mut state, effect_id, run.run_id);
        state
    }

    #[derive(Clone, Copy)]
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn choose(&mut self, upper: usize) -> usize {
            (self.next() as usize) % upper
        }
    }

    #[test]
    fn ten_thousand_seeded_sequences_preserve_invariants() {
        for seed in 1..=10_000_u64 {
            let mut rng = Lcg(seed ^ 0x7A61_D33D_5EED);
            let mut state = CoordinatorState::default();
            let mut observed_effects = Vec::new();
            let mut next_gesture = 1_u64;
            let length = 6 + rng.choose(3);

            for _ in 0..length {
                let event = if !observed_effects.is_empty() && rng.choose(3) == 0 {
                    completion_for(&mut rng, &observed_effects)
                } else {
                    let event = match rng.choose(8) {
                        0 => intent(IntentKind::Toggle, IntentSource::CarbonHotkey, next_gesture),
                        1 => intent(
                            IntentKind::Start,
                            IntentSource::DoubleSpaceHotkey,
                            next_gesture,
                        ),
                        2 => intent(IntentKind::Stop, IntentSource::HoldHotkey, next_gesture),
                        3 => CoordinatorEvent::ForceOff(StopReason::VadTimeout),
                        4 => CoordinatorEvent::ForceOff(StopReason::SystemSleep),
                        5 => CoordinatorEvent::PolicyUpdated(RuntimePolicySnapshot {
                            version: rng.next(),
                            max_stop_attempts: (rng.choose(3) + 1) as u8,
                            max_finalize_attempts: (rng.choose(3) + 1) as u8,
                            ..RuntimePolicySnapshot::default()
                        }),
                        6 => CoordinatorEvent::RuntimeFailed {
                            run_id: RunId::new(rng.next() % 8),
                            error: ErrorCode((rng.next() % 32) as u16),
                        },
                        _ => CoordinatorEvent::StartFinished {
                            effect_id: EffectId::new(10_000 + rng.next() % 64),
                            run_id: RunId::new(10_000 + rng.next() % 64),
                            outcome: StartOutcome::Succeeded,
                        },
                    };
                    next_gesture += 1;
                    event
                };
                let effects = reduce_at(&mut state, event, rng.next());
                observed_effects.extend(effects.iter().copied().filter(|effect| {
                    !matches!(
                        effect,
                        CoordinatorEffect::ReleaseTranscriptBarrier { .. }
                            | CoordinatorEffect::EmitProjection(_)
                            | CoordinatorEffect::ShutdownReady
                    )
                }));
                assert!(state.validate().is_ok(), "seed={seed}, event={event:?}");
                assert!(state
                    .processing_jobs
                    .keys()
                    .all(|run_id| { state.capture.run().map(|run| run.run_id) != Some(*run_id) }));
            }
        }
    }

    fn completion_for(rng: &mut Lcg, effects: &[CoordinatorEffect]) -> CoordinatorEvent {
        match effects[rng.choose(effects.len())] {
            CoordinatorEffect::StartRecording { effect_id, run } => {
                CoordinatorEvent::StartFinished {
                    effect_id,
                    run_id: run.run_id,
                    outcome: match rng.choose(3) {
                        0 => StartOutcome::Succeeded,
                        1 => StartOutcome::Failed(ErrorCode(1)),
                        _ => StartOutcome::Cancelled,
                    },
                }
            }
            CoordinatorEffect::StopRecording {
                effect_id, run_id, ..
            } => CoordinatorEvent::CaptureStopped {
                effect_id,
                run_id,
                outcome: match rng.choose(3) {
                    0 => CaptureStopOutcome::Inactive,
                    1 => CaptureStopOutcome::FailedButInactive(ErrorCode(2)),
                    _ => CaptureStopOutcome::StillActive(ErrorCode(3)),
                },
            },
            CoordinatorEffect::FinalizeRecording {
                effect_id, run_id, ..
            } => CoordinatorEvent::FinalizeFinished {
                effect_id,
                run_id,
                outcome: match rng.choose(3) {
                    0 => FinalizeOutcome::Committed,
                    1 => FinalizeOutcome::NoTranscript,
                    _ => FinalizeOutcome::Failed(ErrorCode(4)),
                },
            },
            CoordinatorEffect::ShowPanel { effect_id, .. }
            | CoordinatorEffect::HidePanel { effect_id, .. } => CoordinatorEvent::WindowFinished {
                effect_id,
                outcome: if rng.choose(3) == 0 {
                    WindowOutcome::Failed(ErrorCode(5))
                } else {
                    WindowOutcome::Applied {
                        window_epoch: rng.next(),
                    }
                },
            },
            CoordinatorEffect::CancelStart { effect_id, run_id } => {
                CoordinatorEvent::StartFinished {
                    effect_id,
                    run_id,
                    outcome: StartOutcome::Cancelled,
                }
            }
            CoordinatorEffect::ReleaseTranscriptBarrier { .. }
            | CoordinatorEffect::EmitProjection(_)
            | CoordinatorEffect::ShutdownReady => {
                unreachable!("non-completing effects are not retained")
            }
        }
    }
}
