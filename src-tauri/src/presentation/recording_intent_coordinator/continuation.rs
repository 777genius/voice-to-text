//! Negotiated EL route within the recording reducer. Capture identity always names
//! the physical episode; this route owns the single logical provider/delivery job.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContinuationKey {
    pub logical_run_id: RunId,
    pub connection_generation: u64,
    pub pause_epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalPhase {
    Active,
    Pausing,
    PausedReclaimable,
    ContinuePending,
    Draining,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContinuationRoute {
    pub key: ContinuationKey,
    pub episode: RunId,
    pub phase: LogicalPhase,
    /// Reducer clock at the user's original Stop, never the Pause ACK time.
    pub stopped_at_ns: Option<u64>,
    pub(super) terminal_observed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinueAttachOutcome {
    Attached { context_revision: u64 },
    Unsent,
    Cancelled,
    AttemptedFailure(ErrorCode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuationEvent {
    /// Emitted only from the actual provider's immutable Ready negotiation.
    Negotiated {
        logical_run_id: RunId,
        connection_generation: u64,
    },
    PauseFinished {
        effect_id: EffectId,
        key: ContinuationKey,
        pause_epoch: Option<u64>,
    },
    ContinueFinished {
        effect_id: EffectId,
        key: ContinuationKey,
        run_id: RunId,
        generation: u64,
        outcome: ContinueAttachOutcome,
    },
    PendingCaptureStopped {
        effect_id: EffectId,
        run_id: RunId,
        generation: u64,
        outcome: CaptureStopOutcome,
    },
    WindowElapsed {
        key: ContinuationKey,
    },
    TerminalObserved {
        logical_run_id: RunId,
        connection_generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuationEffect {
    Pause {
        effect_id: EffectId,
        key: ContinuationKey,
        stopped_at_ns: u64,
    },
    Continue {
        effect_id: EffectId,
        key: ContinuationKey,
        run: RunContext,
        generation: u64,
    },
    SealPending {
        effect_id: EffectId,
        run_id: RunId,
        generation: u64,
        cancel: bool,
    },
    ObserveTerminal {
        logical_run_id: RunId,
        connection_generation: u64,
    },
    WaitForWindow {
        key: ContinuationKey,
        stopped_at_ns: u64,
    },
}
impl ContinuationEffect {
    pub const fn effect_id(self) -> Option<EffectId> {
        match self {
            Self::Pause { effect_id, .. }
            | Self::Continue { effect_id, .. }
            | Self::SealPending { effect_id, .. } => Some(effect_id),
            Self::ObserveTerminal { .. } | Self::WaitForWindow { .. } => None,
        }
    }
}
impl ContinuationEvent {
    pub(super) fn trace_context(self) -> EventTraceContext {
        let (run_id, effect_id) = match self {
            Self::Negotiated { logical_run_id, .. }
            | Self::TerminalObserved { logical_run_id, .. } => (logical_run_id, None),
            Self::PauseFinished { key, effect_id, .. } => (key.logical_run_id, Some(effect_id)),
            Self::ContinueFinished {
                run_id, effect_id, ..
            }
            | Self::PendingCaptureStopped {
                run_id, effect_id, ..
            } => (run_id, Some(effect_id)),
            Self::WindowElapsed { key } => (key.logical_run_id, None),
        };
        EventTraceContext {
            phase: TracePhase::EffectEnqueued,
            source: None,
            gesture_id: None,
            run_id: Some(run_id),
            effect_id,
            window_epoch: None,
            reason: None,
            outcome: None,
            error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PendingDisposition {
    Live,
    Seal,
    Cancel,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PendingEpisode {
    pub(super) run_id: RunId,
    pub(super) disposition: PendingDisposition,
    sealed: bool,
    seal_effect: Option<EffectId>,
    cancel_signalled: bool,
    physical_retry: Option<(u64, bool)>,
    stopped_at_ns: Option<u64>,
}
impl PendingEpisode {
    pub(super) fn cancel(&mut self) {
        if self.disposition != PendingDisposition::Cancel {
            // Sealing preserves the receiver; cancellation must still dispose it.
            self.disposition = PendingDisposition::Cancel;
            self.sealed = false;
        }
    }

    pub(super) fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            disposition: PendingDisposition::Live,
            sealed: false,
            seal_effect: None,
            cancel_signalled: false,
            physical_retry: None,
            stopped_at_ns: None,
        }
    }
}

pub(super) fn pending_capture_retry(
    state: &CoordinatorState,
    effect: EffectId,
    run: RunId,
) -> Option<(u64, bool)> {
    let owns = match state.capture {
        CaptureState::Stopping {
            effect_id,
            run: owner,
            ..
        } => effect_id == effect && owner.run_id == run,
        CaptureState::StopUncertain {
            active_effect: Some(effect_id),
            run: owner,
            ..
        } => effect_id == effect && owner.run_id == run,
        _ => false,
    };
    owns.then(|| {
        state
            .pending_episode
            .filter(|p| p.run_id == run)?
            .physical_retry
    })
    .flatten()
}

pub(super) fn complete_pending_capture_retry(
    state: &mut CoordinatorState,
    run: RunContext,
) -> bool {
    let Some(mut pending) = state.pending_episode.filter(|p| p.run_id == run.run_id) else {
        return false;
    };
    let Some((generation, cancelled)) = pending.physical_retry.take() else {
        return false;
    };
    if cancelled {
        state.capture = CaptureState::Idle;
        state.pending_episode = None;
    } else {
        // Physical release is not consumption of the sealed receiver. B still
        // owns its buffered bytes and must attach or cold-route after A settles.
        pending.sealed = pending.disposition != PendingDisposition::Cancel;
        state.capture = CaptureState::Buffering { run, generation };
        state.pending_episode = Some(pending);
    }
    true
}

// Logical settlement can add a finalization obligation, but can never
// allocate a second physical Stop or renew an exhausted retry budget.
fn transfer_stop_finalization(state: &mut CoordinatorState) -> bool {
    let owner = match &mut state.capture {
        CaptureState::Stopping {
            effect_id,
            finalize_after,
            ..
        } => {
            *finalize_after = true;
            Some(*effect_id)
        }
        CaptureState::StopUncertain {
            active_effect,
            finalize_after,
            ..
        } => {
            *finalize_after = true;
            *active_effect
        }
        _ => return false,
    };
    if let Some(PendingEffect::Stop { finalize_after, .. }) =
        owner.and_then(|id| state.in_flight.get_mut(&id))
    {
        *finalize_after = true;
    }
    true
}

pub(super) fn settle_after_physical_release(
    state: &mut CoordinatorState,
    run: RunContext,
    effects: &mut Vec<CoordinatorEffect>,
) {
    if transfer_stop_finalization(state) {
        state.pending_episode = None;
    } else if state
        .pending_episode
        .is_some_and(|p| p.run_id == run.run_id && p.seal_effect.is_some())
    {
        // The first Seal has not proved release yet. Observe its result too.
        state.capture = CaptureState::Recording { run };
    } else {
        state.pending_episode = None;
        start_active_stop(state, run, StopReason::RuntimeFailure, true, effects);
    }
}

pub(super) fn note_continuation_stop(state: &mut CoordinatorState, toggle: bool) {
    if let Some(pending) = &mut state.pending_episode {
        // Repeated Stop is idempotent. Once cancelled, a late Stop cannot resurrect B.
        if pending.disposition == PendingDisposition::Live {
            pending.stopped_at_ns = Some(state.last_monotonic_ns);
            pending.disposition =
                if toggle && matches!(state.capture, CaptureState::Preparing { .. }) {
                    PendingDisposition::Cancel
                } else {
                    PendingDisposition::Seal
                };
        }
    } else if let Some(route) = &mut state.continuation {
        if route.phase == LogicalPhase::Active && route.stopped_at_ns.is_none() {
            route.stopped_at_ns = Some(state.last_monotonic_ns);
        }
    }
}

pub(super) fn pause_instead_of_finalize(
    state: &mut CoordinatorState,
    episode: RunId,
    effects: &mut Vec<CoordinatorEffect>,
) -> bool {
    let Some(mut route) = state.continuation else {
        return false;
    };
    if episode != route.episode {
        return false;
    }
    if route.phase != LogicalPhase::Active
        || matches!(
            state.desired_stop_reason,
            StopReason::RuntimeFailure
                | StopReason::SystemSleep
                | StopReason::PermissionRevoked
                | StopReason::Shutdown
        )
    {
        route.phase = LogicalPhase::Finalizing;
        state.continuation = Some(route);
        begin_terminal_finalize(state, route.key.logical_run_id, 1, effects);
        return true;
    }
    let stopped_at_ns = *route.stopped_at_ns.get_or_insert(state.last_monotonic_ns);
    route.phase = LogicalPhase::Pausing;
    state.continuation = Some(route);
    let effect_id = state.next_effect();
    state.processing_jobs.insert(
        route.key.logical_run_id,
        ProcessingJob {
            run_id: route.key.logical_run_id,
            state: ProcessingState::Continuation,
        },
    );
    register_effect(state, effect_id, PendingEffect::Pause { key: route.key });
    effects.push(CoordinatorEffect::Continuation(ContinuationEffect::Pause {
        effect_id,
        key: route.key,
        stopped_at_ns,
    }));
    true
}

pub(super) fn apply_continuation_event(
    state: &mut CoordinatorState,
    event: ContinuationEvent,
    effects: &mut Vec<CoordinatorEffect>,
    phase: &mut TracePhase,
) {
    match event {
        ContinuationEvent::Negotiated {
            logical_run_id,
            connection_generation,
        } => {
            // Ready observed after Stop/finalization cannot reopen an irreversible run.
            if connection_generation == 0
                || state.continuation.is_some()
                || !matches!(state.capture, CaptureState::Recording { run } if run.run_id == logical_run_id)
            {
                *phase = TracePhase::StaleCompletion;
                return;
            }
            state.continuation = Some(ContinuationRoute {
                key: ContinuationKey {
                    logical_run_id,
                    connection_generation,
                    pause_epoch: 0,
                },
                episode: logical_run_id,
                phase: LogicalPhase::Active,
                stopped_at_ns: None,
                terminal_observed: false,
            });
            effects.push(CoordinatorEffect::Continuation(
                ContinuationEffect::ObserveTerminal {
                    logical_run_id,
                    connection_generation,
                },
            ));
        }
        ContinuationEvent::PauseFinished {
            effect_id,
            key,
            pause_epoch,
        } => {
            if state.in_flight.get(&effect_id) != Some(&PendingEffect::Pause { key })
                || !state
                    .continuation
                    .is_some_and(|r| r.key == key && r.phase == LogicalPhase::Pausing)
            {
                *phase = TracePhase::StaleCompletion;
                return;
            }
            state.in_flight.remove(&effect_id);
            state.remember_completion(effect_id, CompletedEffect::Continuation);
            let mut route = state.continuation.unwrap();
            if let Some(epoch) = pause_epoch.filter(|epoch| *epoch > key.pause_epoch) {
                route.key.pause_epoch = epoch;
                route.phase = LogicalPhase::PausedReclaimable;
                effects.push(CoordinatorEffect::Continuation(
                    ContinuationEffect::WaitForWindow {
                        key: route.key,
                        stopped_at_ns: route.stopped_at_ns.unwrap(),
                    },
                ));
            } else {
                route.phase = LogicalPhase::Finalizing;
                begin_terminal_finalize(state, key.logical_run_id, 1, effects);
            }
            state.continuation = Some(route);
        }
        ContinuationEvent::ContinueFinished {
            effect_id,
            key,
            run_id,
            generation,
            outcome,
        } => {
            let Some(PendingEffect::Continue {
                key: expected,
                run,
                generation: expected_generation,
            }) = state.in_flight.get(&effect_id).copied()
            else {
                *phase = TracePhase::StaleCompletion;
                return;
            };
            if expected != key
                || expected_generation != generation
                || run.run_id != run_id
                || !state
                    .continuation
                    .is_some_and(|r| r.key == key && r.phase == LogicalPhase::ContinuePending)
            {
                *phase = TracePhase::StaleCompletion;
                return;
            }
            state.in_flight.remove(&effect_id);
            state.remember_completion(effect_id, CompletedEffect::Continuation);
            let mut route = state.continuation.unwrap();
            match outcome {
                ContinueAttachOutcome::Attached { .. } => {
                    route.episode = run_id;
                    route.phase = LogicalPhase::Active;
                    route.stopped_at_ns = if state.desired_recording.is_on() {
                        None
                    } else {
                        state
                            .pending_episode
                            .and_then(|pending| pending.stopped_at_ns)
                            .or(Some(state.last_monotonic_ns))
                    };
                    // Attachment transfers finalization to the physical Stop
                    // already in flight; it must not replace that owner's identity
                    // or reset its bounded attempt count.
                    if transfer_stop_finalization(state) {
                        state.pending_episode = None;
                    } else {
                        state.capture = CaptureState::Recording { run };
                        // A first Seal can also still own physical release.
                        // Its completion must remain observable after attachment.
                        if !state
                            .pending_episode
                            .is_some_and(|p| p.seal_effect.is_some())
                        {
                            state.pending_episode = None;
                        }
                    }
                }
                ContinueAttachOutcome::Unsent => {
                    route.phase = LogicalPhase::Draining;
                    // An unattempted refusal does not retire a physical Stop retry.
                    // Only the still-owned Continue transition may restore Buffering.
                    if matches!(state.capture, CaptureState::Starting { effect_id: owner, .. } if owner == effect_id)
                    {
                        if state.pending_episode.is_some_and(|p| {
                            p.run_id == run_id
                                && p.sealed
                                && p.disposition == PendingDisposition::Cancel
                        }) {
                            state.capture = CaptureState::Idle;
                            state.pending_episode = None;
                        } else {
                            state.capture = CaptureState::Buffering { run, generation };
                        }
                    }
                }
                ContinueAttachOutcome::Cancelled => {
                    route.phase = LogicalPhase::Draining;
                    // Control cancellation does not prove physical microphone release.
                    // SealPending (or its bounded stop retry) retains ownership until
                    // it confirms inactivity, in either completion order.
                    if state
                        .pending_episode
                        .is_some_and(|p| p.run_id == run_id && !p.sealed)
                    {
                        if matches!(state.capture, CaptureState::Starting { effect_id: owner, .. } if owner == effect_id)
                        {
                            state.capture = CaptureState::Buffering { run, generation };
                        }
                    } else if matches!(state.capture, CaptureState::Starting { effect_id: owner, .. } if owner == effect_id)
                    {
                        state.capture = CaptureState::Idle;
                        state.pending_episode = None;
                    }
                }
                ContinueAttachOutcome::AttemptedFailure(error) => {
                    route.phase = LogicalPhase::Finalizing;
                    route.episode = run_id;
                    state.continuation = Some(route);
                    set_recoverable_fault(
                        state,
                        CoordinatorFault::RuntimeFailed {
                            run_id: key.logical_run_id,
                            error,
                        },
                    );
                    force_off(state, StopReason::RuntimeFailure);
                    if state
                        .capture
                        .run()
                        .is_some_and(|owner| owner.run_id == run_id)
                    {
                        settle_after_physical_release(state, run, effects);
                    } else {
                        begin_terminal_finalize(state, key.logical_run_id, 1, effects);
                    }
                }
            }
            state.continuation = Some(route);
        }
        ContinuationEvent::PendingCaptureStopped {
            effect_id,
            run_id,
            generation,
            outcome,
        } => {
            let Some(PendingEffect::Seal {
                run_id: owner,
                generation: expected,
                cancel,
            }) = state.in_flight.get(&effect_id).copied()
            else {
                *phase = TracePhase::StaleCompletion;
                return;
            };
            if owner != run_id || generation != expected {
                *phase = TracePhase::StaleCompletion;
                return;
            }
            state.in_flight.remove(&effect_id);
            state.remember_completion(effect_id, CompletedEffect::Continuation);
            let Some(mut pending) = state
                .pending_episode
                .filter(|p| p.run_id == run_id && p.seal_effect == Some(effect_id))
            else {
                *phase = TracePhase::StaleCompletion;
                return;
            };
            pending.seal_effect = None;
            let attached = state.continuation.is_some_and(|route| {
                matches!(route.phase, LogicalPhase::Active | LogicalPhase::Finalizing)
                    && route.episode == run_id
            });
            if let CaptureStopOutcome::StillActive(error) = outcome {
                set_recoverable_fault(state, CoordinatorFault::RuntimeFailed { run_id, error });
                // The bounded Stop retry retains the exact prepared generation and
                // seal/cancel decision; a generic Stop would discard unsent PCM.
                pending.physical_retry = Some((generation, cancel));
                state.pending_episode = if attached { None } else { Some(pending) };
                if let Some(run) = state.capture.run() {
                    start_active_stop(state, run, StopReason::RuntimeFailure, attached, effects);
                }
                return;
            }
            if attached {
                state.capture = CaptureState::Idle;
                state.pending_episode = None;
                begin_finalize(state, run_id, 1, effects);
                return;
            }
            pending.sealed = true;
            if !cancel && pending.disposition == PendingDisposition::Cancel {
                pending.sealed = false;
            }
            if cancel && !matches!(state.capture, CaptureState::Starting { .. }) {
                state.capture = CaptureState::Idle;
                state.pending_episode = None;
            } else {
                state.pending_episode = Some(pending);
            }
        }
        ContinuationEvent::WindowElapsed { key } => {
            if let Some(route) = &mut state.continuation {
                if route.key == key && route.phase == LogicalPhase::PausedReclaimable {
                    // Eligibility expiry does not send Finalize or restart provider silence.
                    route.phase = LogicalPhase::Draining;
                } else {
                    *phase = TracePhase::StaleCompletion;
                }
            }
        }
        ContinuationEvent::TerminalObserved {
            logical_run_id,
            connection_generation,
        } => {
            let Some(mut route) = state.continuation.filter(|r| {
                r.key.logical_run_id == logical_run_id
                    && r.key.connection_generation == connection_generation
            }) else {
                *phase = TracePhase::StaleCompletion;
                return;
            };
            // A control/first write owns its bounded result. Its completion handles
            // the cached terminal, so a monitor cannot race it into a replay route.
            if matches!(
                route.phase,
                LogicalPhase::Pausing | LogicalPhase::ContinuePending
            ) {
                route.terminal_observed = true;
                state.continuation = Some(route);
                return;
            }
            route.terminal_observed = false;
            route.phase = LogicalPhase::Finalizing;
            state.continuation = Some(route);
            if state
                .capture
                .run()
                .is_some_and(|run| run.run_id == route.episode)
            {
                force_off(state, StopReason::RuntimeFailure);
                if let Some(run) = state.capture.run() {
                    settle_after_physical_release(state, run, effects);
                }
            } else {
                begin_terminal_finalize(state, logical_run_id, 1, effects);
            }
        }
    }
    if let Some(route) = state.continuation.filter(|r| {
        r.terminal_observed
            && !matches!(
                r.phase,
                LogicalPhase::Pausing | LogicalPhase::ContinuePending | LogicalPhase::Finalizing
            )
    }) {
        apply_continuation_event(
            state,
            ContinuationEvent::TerminalObserved {
                logical_run_id: route.key.logical_run_id,
                connection_generation: route.key.connection_generation,
            },
            effects,
            phase,
        );
    }
}

/// Returns true when continuation owns capture reconciliation. Legacy/DG still
/// pass unchanged through the original reducer below this call.
pub(super) fn reconcile_continuation_capture(
    state: &mut CoordinatorState,
    effects: &mut Vec<CoordinatorEffect>,
) -> bool {
    let tearing_down = !state.desired_recording.is_on()
        && matches!(
            state.desired_stop_reason,
            StopReason::Shutdown
                | StopReason::SystemSleep
                | StopReason::PermissionRevoked
                | StopReason::RuntimeFailure
        );
    if tearing_down {
        if let Some(route) = state.continuation.as_mut() {
            // In-flight controls must finish/compensate before finalization;
            // inactive reclaimable owners have no such operation to wait for.
            if matches!(
                route.phase,
                LogicalPhase::PausedReclaimable | LogicalPhase::Draining
            ) {
                route.phase = LogicalPhase::Finalizing;
                let logical = route.key.logical_run_id;
                begin_terminal_finalize(state, logical, 1, effects);
            }
        }
    }
    let Some(mut pending) = state.pending_episode else {
        return false;
    };
    if state.capture.run().map(|run| run.run_id) != Some(pending.run_id) {
        state.pending_episode = None;
        return false;
    }
    // Before admission a toggle may cancel Prepare. Its token-fenced outcome
    // decides whether capture was admitted; success must instead seal and drain.
    if matches!(state.capture, CaptureState::Preparing { .. })
        && pending.disposition == PendingDisposition::Cancel
    {
        return false;
    }
    // Continue owns its cancellation independently of the physical microphone.
    // Seal/retry may have replaced Starting while that control still awaits admission.
    if pending.disposition == PendingDisposition::Cancel && !pending.cancel_signalled {
        let owner = state
            .in_flight
            .iter()
            .find_map(|(effect_id, effect)| match effect {
                PendingEffect::Continue { run, .. } if run.run_id == pending.run_id => {
                    Some(*effect_id)
                }
                _ => None,
            })
            .or_else(|| match state.capture {
                CaptureState::Starting { effect_id, run, .. } if run.run_id == pending.run_id => {
                    Some(effect_id)
                }
                _ => None,
            });
        if let Some(effect_id) = owner {
            effects.push(CoordinatorEffect::CancelStart {
                effect_id,
                run_id: pending.run_id,
            });
            pending.cancel_signalled = true;
        }
    }
    state.pending_episode = Some(pending);
    // Physical release still belongs to the existing bounded retry owner.
    if matches!(
        state.capture,
        CaptureState::Stopping { .. } | CaptureState::StopUncertain { .. }
    ) {
        return true;
    }
    if pending.disposition != PendingDisposition::Live {
        if !pending.sealed && pending.seal_effect.is_none() {
            if let Some((run_id, generation)) = state.capture_identity() {
                let effect_id = state.next_effect();
                let cancel = pending.disposition == PendingDisposition::Cancel;
                register_effect(
                    state,
                    effect_id,
                    PendingEffect::Seal {
                        run_id,
                        generation,
                        cancel,
                    },
                );
                effects.push(CoordinatorEffect::Continuation(
                    ContinuationEffect::SealPending {
                        effect_id,
                        run_id,
                        generation,
                        cancel,
                    },
                ));
                pending.seal_effect = Some(effect_id);
            }
        }
    }
    state.pending_episode = Some(pending);
    if let CaptureState::Buffering { run, generation } = state.capture {
        if tearing_down {
            return true;
        }
        if pending.disposition == PendingDisposition::Cancel || pending.seal_effect.is_some() {
            return true;
        }
        if let Some(mut route) = state.continuation {
            if route.phase == LogicalPhase::PausedReclaimable {
                let effect_id = state.next_effect();
                register_effect(
                    state,
                    effect_id,
                    PendingEffect::Continue {
                        key: route.key,
                        run,
                        generation,
                    },
                );
                state.capture = CaptureState::Starting {
                    run,
                    effect_id,
                    cancel_requested: false,
                };
                route.phase = LogicalPhase::ContinuePending;
                state.continuation = Some(route);
                effects.push(CoordinatorEffect::Continuation(
                    ContinuationEffect::Continue {
                        effect_id,
                        key: route.key,
                        run,
                        generation,
                    },
                ));
            }
        } else if state.processing_jobs.is_empty() {
            // Strong-unsent cold fallback only after A's explicit released terminal.
            let effect_id = state.next_effect();
            register_effect(state, effect_id, PendingEffect::Start { run });
            state.capture = CaptureState::Starting {
                run,
                effect_id,
                cancel_requested: false,
            };
            effects.push(CoordinatorEffect::StartRecording { effect_id, run });
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(state: &mut CoordinatorState, event: ContinuationEvent) -> Vec<CoordinatorEffect> {
        reduce(state, CoordinatorEvent::Continuation(event))
    }
    fn start(state: &mut CoordinatorState) -> (EffectId, RunContext) {
        let effects = reduce(
            state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        effects
            .into_iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::PrepareCapture { effect_id, run } => Some((effect_id, run)),
                _ => None,
            })
            .unwrap()
    }
    fn prepared(
        state: &mut CoordinatorState,
        effect_id: EffectId,
        run: RunContext,
        generation: u64,
    ) -> Vec<CoordinatorEffect> {
        reduce(
            state,
            CoordinatorEvent::PrepareFinished {
                effect_id,
                run_id: run.run_id,
                outcome: PrepareOutcome::Succeeded { generation },
            },
        )
    }
    fn active() -> (CoordinatorState, RunContext) {
        let mut state = CoordinatorState::default();
        let (effect_id, run) = start(&mut state);
        let effects = prepared(&mut state, effect_id, run, 10);
        let effect_id = effects
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::StartRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        reduce(
            &mut state,
            CoordinatorEvent::StartFinished {
                effect_id,
                run_id: run.run_id,
                outcome: StartOutcome::Succeeded,
            },
        );
        event(
            &mut state,
            ContinuationEvent::Negotiated {
                logical_run_id: run.run_id,
                connection_generation: 7,
            },
        );
        (state, run)
    }
    fn pause(state: &mut CoordinatorState, run: RunContext, epoch: u64) -> ContinuationKey {
        let effects = reduce_at(
            state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
            1_000_000_000 * epoch,
        );
        let stop = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let effects = reduce_at(
            state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: run.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
            1_000_000_000 * epoch + 20_000_000,
        );
        assert!(!effects.iter().any(|e| matches!(
            e,
            CoordinatorEffect::FinalizeRecording { .. }
                | CoordinatorEffect::ReleaseTranscriptBarrier { .. }
        )));
        let (effect_id, key, time) = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::Continuation(ContinuationEffect::Pause {
                    effect_id,
                    key,
                    stopped_at_ns,
                }) => Some((*effect_id, *key, *stopped_at_ns)),
                _ => None,
            })
            .unwrap();
        assert_eq!(time, 1_000_000_000 * epoch);
        event(
            state,
            ContinuationEvent::PauseFinished {
                effect_id,
                key,
                pause_epoch: Some(epoch),
            },
        );
        state.continuation.unwrap().key
    }
    fn continue_effect(
        effects: &[CoordinatorEffect],
    ) -> (EffectId, ContinuationKey, RunContext, u64) {
        effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::Continuation(ContinuationEffect::Continue {
                    effect_id,
                    key,
                    run,
                    generation,
                }) => Some((*effect_id, *key, *run, *generation)),
                _ => None,
            })
            .unwrap()
    }
    #[test]
    fn cancelled_or_unsent_continue_keeps_physical_owner_until_seal_or_retry_confirms_release() {
        for attach_outcome in [
            ContinueAttachOutcome::Cancelled,
            ContinueAttachOutcome::Unsent,
        ] {
            for seal_first in [false, true] {
                for outcome in [
                    CaptureStopOutcome::Inactive,
                    CaptureStopOutcome::FailedButInactive(ErrorCode(90)),
                    CaptureStopOutcome::StillActive(ErrorCode(91)),
                ] {
                    let (mut state, a) = active();
                    pause(&mut state, a, 1);
                    let (prepare, b) = start(&mut state);
                    let effects = prepared(&mut state, prepare, b, 22);
                    let (attach, key, _, generation) = continue_effect(&effects);
                    let effects =
                        reduce(&mut state, CoordinatorEvent::ForceOff(StopReason::Shutdown));
                    let seal = effects
                        .iter()
                        .find_map(|e| match e {
                            CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                                effect_id,
                                run_id,
                                cancel: true,
                                ..
                            }) if *run_id == b.run_id => Some(*effect_id),
                            _ => None,
                        })
                        .unwrap();
                    let completed = ContinuationEvent::ContinueFinished {
                        effect_id: attach,
                        key,
                        run_id: b.run_id,
                        generation,
                        outcome: attach_outcome,
                    };
                    let stopped = ContinuationEvent::PendingCaptureStopped {
                        effect_id: seal,
                        run_id: b.run_id,
                        generation,
                        outcome,
                    };
                    let mut effects = if seal_first {
                        event(&mut state, stopped)
                    } else {
                        event(&mut state, completed)
                    };
                    if !seal_first {
                        assert_eq!(state.capture.run().map(|r| r.run_id), Some(b.run_id));
                        assert_eq!(state.pending_episode.unwrap().seal_effect, Some(seal));
                    }
                    effects.extend(if seal_first {
                        event(&mut state, completed)
                    } else {
                        event(&mut state, stopped)
                    });
                    if matches!(outcome, CaptureStopOutcome::StillActive(_)) {
                        let retry = effects
                            .iter()
                            .find_map(|e| match e {
                                CoordinatorEffect::StopRecording {
                                    effect_id, run_id, ..
                                } if *run_id == b.run_id => Some(*effect_id),
                                _ => None,
                            })
                            .expect("failed Seal must schedule physical stop retry");
                        assert_eq!(state.capture.run().map(|r| r.run_id), Some(b.run_id));
                        assert!(!effects.iter().any(|e| matches!(
                            e,
                            CoordinatorEffect::Continuation(ContinuationEffect::SealPending { .. })
                        )));
                        let queued = reduce(
                            &mut state,
                            CoordinatorEvent::Intent(RecordingIntent::start(
                                IntentSource::Frontend,
                                None,
                            )),
                        );
                        assert!(!queued
                            .iter()
                            .any(|e| matches!(e, CoordinatorEffect::PrepareCapture { .. })));
                        reduce(
                            &mut state,
                            CoordinatorEvent::CaptureStopped {
                                effect_id: retry,
                                run_id: b.run_id,
                                outcome: CaptureStopOutcome::Inactive,
                            },
                        );
                    }
                    assert_ne!(state.capture.run().map(|r| r.run_id), Some(b.run_id));
                    assert!(!state.pending_episode.is_some_and(|p| p.run_id == b.run_id));
                    assert!(state.validate().is_ok());
                }
            }
        }
    }

    #[test]
    fn pending_stop_retry_preserves_sealed_route_and_exact_retry_owner() {
        for cancel in [false, true] {
            let (mut state, a) = active();
            let effects = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
            );
            let stop_a = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            let effects = reduce(
                &mut state,
                CoordinatorEvent::CaptureStopped {
                    effect_id: stop_a,
                    run_id: a.run_id,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            let (pause_id, key) = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::Continuation(ContinuationEffect::Pause {
                        effect_id,
                        key,
                        ..
                    }) => Some((*effect_id, *key)),
                    _ => None,
                })
                .unwrap();
            let (prepare, b) = start(&mut state);
            prepared(&mut state, prepare, b, 22);
            assert!(matches!(state.capture, CaptureState::Buffering { .. }));
            let stop = if cancel {
                CoordinatorEvent::ForceOff(StopReason::Shutdown)
            } else {
                CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None))
            };
            let effects = reduce(&mut state, stop);
            let seal = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                        effect_id,
                        ..
                    }) => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            let effects = event(
                &mut state,
                ContinuationEvent::PendingCaptureStopped {
                    effect_id: seal,
                    run_id: b.run_id,
                    generation: 22,
                    outcome: CaptureStopOutcome::StillActive(ErrorCode(99)),
                },
            );
            let retry = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                state.pending_capture_retry(retry, b.run_id),
                Some((22, cancel))
            );
            assert_eq!(state.pending_capture_retry(seal, b.run_id), None);
            let effects = reduce(
                &mut state,
                CoordinatorEvent::CaptureStopped {
                    effect_id: retry,
                    run_id: b.run_id,
                    outcome: CaptureStopOutcome::StillActive(ErrorCode(100)),
                },
            );
            let retry2 = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording {
                        effect_id,
                        attempt: 2,
                        ..
                    } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            assert_eq!(state.pending_capture_retry(retry, b.run_id), None);
            assert_eq!(
                state.pending_capture_retry(retry2, b.run_id),
                Some((22, cancel))
            );
            assert!(!effects.iter().any(|e| matches!(
                e,
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending { .. })
            )));
            reduce(
                &mut state,
                CoordinatorEvent::CaptureStopped {
                    effect_id: retry2,
                    run_id: b.run_id,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            if cancel {
                assert!(state.pending_episode.is_none());
                assert!(matches!(state.capture, CaptureState::Idle));
            } else {
                assert!(state.pending_episode.unwrap().sealed);
                assert!(
                    matches!(state.capture, CaptureState::Buffering { run, generation: 22 } if run.run_id == b.run_id)
                );
            }
            let effects = event(
                &mut state,
                ContinuationEvent::PauseFinished {
                    effect_id: pause_id,
                    key,
                    pause_epoch: Some(1),
                },
            );
            assert_eq!(effects.iter().any(|e| matches!(e, CoordinatorEffect::Continuation(ContinuationEffect::Continue { run, .. }) if run.run_id == b.run_id)), !cancel);
            assert!(state.validate().is_ok());
        }
    }

    pub(crate) fn pending_continue_with_outstanding_seal() -> (
        CoordinatorState,
        RunContext,
        EffectId,
        ContinuationKey,
        u64,
        EffectId,
    ) {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        let (attach, key, _, generation) = continue_effect(&effects);
        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
        );
        let seal = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                    effect_id,
                    ..
                }) => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        (state, b, attach, key, generation, seal)
    }

    pub(crate) fn pending_continue_with_failed_seal() -> (
        CoordinatorState,
        RunContext,
        EffectId,
        ContinuationKey,
        u64,
        EffectId,
    ) {
        let (mut state, b, attach, key, generation, seal) =
            pending_continue_with_outstanding_seal();
        let effects = event(
            &mut state,
            ContinuationEvent::PendingCaptureStopped {
                effect_id: seal,
                run_id: b.run_id,
                generation,
                outcome: CaptureStopOutcome::StillActive(ErrorCode(99)),
            },
        );
        let retry = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        (state, b, attach, key, generation, retry)
    }

    #[test]
    fn attached_continue_preserves_physical_retry_identity_and_finalization() {
        for failed_retries in [0, 1, 3] {
            let (mut state, b, attach, key, generation, mut retry) =
                pending_continue_with_failed_seal();
            let first_retry = retry;
            for _ in 0..failed_retries {
                let effects = reduce(
                    &mut state,
                    CoordinatorEvent::CaptureStopped {
                        effect_id: retry,
                        run_id: b.run_id,
                        outcome: CaptureStopOutcome::StillActive(ErrorCode(100)),
                    },
                );
                if let Some(next) = effects.iter().find_map(|e| match e {
                    CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                }) {
                    retry = next;
                }
            }
            let before = state.capture;
            let effects = event(
                &mut state,
                ContinuationEvent::ContinueFinished {
                    effect_id: attach,
                    key,
                    run_id: b.run_id,
                    generation,
                    outcome: ContinueAttachOutcome::Attached {
                        context_revision: 7,
                    },
                },
            );
            assert!(!effects
                .iter()
                .any(|e| matches!(e, CoordinatorEffect::StopRecording { .. })));
            let owner = match (before, state.capture) {
                (
                    CaptureState::Stopping {
                        effect_id: old,
                        attempts: n,
                        ..
                    },
                    CaptureState::Stopping {
                        effect_id,
                        attempts,
                        finalize_after: true,
                        ..
                    },
                ) => {
                    assert_eq!((old, n), (effect_id, attempts));
                    Some(effect_id)
                }
                (
                    CaptureState::StopUncertain {
                        active_effect: old,
                        attempts: n,
                        ..
                    },
                    CaptureState::StopUncertain {
                        active_effect,
                        attempts,
                        finalize_after: true,
                        ..
                    },
                ) => {
                    assert_eq!((old, n), (active_effect, attempts));
                    active_effect
                }
                pair => panic!("physical owner replaced: {pair:?}"),
            };
            assert!(state.pending_episode.is_none());
            // Already-completed effects cannot settle the surviving owner.
            if first_retry != retry {
                let before = state.capture;
                reduce(
                    &mut state,
                    CoordinatorEvent::CaptureStopped {
                        effect_id: first_retry,
                        run_id: b.run_id,
                        outcome: CaptureStopOutcome::Inactive,
                    },
                );
                assert_eq!(state.capture, before);
            }
            if let Some(owner) = owner {
                let effects = reduce(
                    &mut state,
                    CoordinatorEvent::CaptureStopped {
                        effect_id: owner,
                        run_id: b.run_id,
                        outcome: CaptureStopOutcome::Inactive,
                    },
                );
                assert!(matches!(state.capture, CaptureState::Idle));
                assert_eq!(
                    effects
                        .iter()
                        .filter(|e| matches!(
                            e,
                            CoordinatorEffect::Continuation(ContinuationEffect::Pause { .. })
                        ))
                        .count(),
                    1
                );
                assert!(matches!(
                    state.continuation.unwrap().phase,
                    LogicalPhase::Pausing
                ));
            } else {
                assert!(matches!(
                    state.capture,
                    CaptureState::StopUncertain {
                        active_effect: None,
                        ..
                    }
                ));
            }
            assert!(state.validate().is_ok());
        }
    }

    #[test]
    fn attachment_before_seal_or_after_retry_preserves_single_finalize() {
        for before_seal in [true, false] {
            let (mut state, a) = active();
            pause(&mut state, a, 1);
            let (prepare, b) = start(&mut state);
            let effects = prepared(&mut state, prepare, b, 22);
            let (attach, key, _, generation) = continue_effect(&effects);
            let effects = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
            );
            let seal = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                        effect_id,
                        ..
                    }) => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            let attached = ContinuationEvent::ContinueFinished {
                effect_id: attach,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Attached {
                    context_revision: 7,
                },
            };
            if before_seal {
                event(&mut state, attached);
            }
            let effects = event(
                &mut state,
                ContinuationEvent::PendingCaptureStopped {
                    effect_id: seal,
                    run_id: b.run_id,
                    generation,
                    outcome: CaptureStopOutcome::StillActive(ErrorCode(99)),
                },
            );
            let retry = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            let mut effects = reduce(
                &mut state,
                CoordinatorEvent::CaptureStopped {
                    effect_id: retry,
                    run_id: b.run_id,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            if !before_seal {
                assert!(matches!(state.capture, CaptureState::Buffering { .. }));
                effects.extend(event(&mut state, attached));
                let stop = effects
                    .iter()
                    .find_map(|e| match e {
                        CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                        _ => None,
                    })
                    .unwrap();
                effects.extend(reduce(
                    &mut state,
                    CoordinatorEvent::CaptureStopped {
                        effect_id: stop,
                        run_id: b.run_id,
                        outcome: CaptureStopOutcome::Inactive,
                    },
                ));
            }
            assert_eq!(
                effects
                    .iter()
                    .filter(|e| matches!(
                        e,
                        CoordinatorEffect::Continuation(ContinuationEffect::Pause { .. })
                    ))
                    .count(),
                1
            );
            assert!(state.pending_episode.is_none());
            assert!(matches!(state.capture, CaptureState::Idle));
            assert!(state.validate().is_ok());
        }
    }

    #[test]
    fn late_teardown_disposes_already_sealed_b_in_both_completion_orders() {
        for teardown in [
            CoordinatorEvent::ShutdownRequested,
            CoordinatorEvent::ForceOff(StopReason::SystemSleep),
        ] {
            for dispose_first in [false, true] {
                let (mut state, b, attach, key, generation, retry) =
                    pending_continue_with_failed_seal();
                reduce(
                    &mut state,
                    CoordinatorEvent::CaptureStopped {
                        effect_id: retry,
                        run_id: b.run_id,
                        outcome: CaptureStopOutcome::Inactive,
                    },
                );
                assert!(state.pending_episode.unwrap().sealed);
                let effects = reduce(&mut state, teardown);
                let dispose = effects
                    .iter()
                    .find_map(|e| match e {
                        CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                            effect_id,
                            run_id,
                            generation: g,
                            cancel: true,
                        }) if *run_id == b.run_id && *g == generation => Some(*effect_id),
                        _ => None,
                    })
                    .expect("sealed PCM still requires explicit cancellation disposal");
                let finished = ContinuationEvent::ContinueFinished {
                    effect_id: attach,
                    key,
                    run_id: b.run_id,
                    generation,
                    outcome: ContinueAttachOutcome::Cancelled,
                };
                let disposed = ContinuationEvent::PendingCaptureStopped {
                    effect_id: dispose,
                    run_id: b.run_id,
                    generation,
                    outcome: CaptureStopOutcome::Inactive,
                };
                let mut effects = if dispose_first {
                    event(&mut state, disposed)
                } else {
                    event(&mut state, finished)
                };
                effects.extend(if dispose_first {
                    event(&mut state, finished)
                } else {
                    event(&mut state, disposed)
                });
                assert!(state.pending_episode.is_none());
                assert!(matches!(state.capture, CaptureState::Idle));
                for effect in effects {
                    if let CoordinatorEffect::FinalizeRecording {
                        effect_id, run_id, ..
                    } = effect
                    {
                        reduce(
                            &mut state,
                            CoordinatorEvent::FinalizeFinished {
                                effect_id,
                                run_id,
                                outcome: FinalizeOutcome::Committed,
                            },
                        );
                    }
                }
                assert!(state.processing_jobs.is_empty());
                assert!(state.validate().is_ok());
            }
        }
    }

    #[test]
    fn terminal_and_attempted_failure_preserve_active_and_exhausted_stop_owners() {
        for failures in [0, 1, 3] {
            for settlement in 0..7 {
                let (mut state, b, attach, key, generation, mut retry) =
                    pending_continue_with_failed_seal();
                for _ in 0..failures {
                    let effects = reduce(
                        &mut state,
                        CoordinatorEvent::CaptureStopped {
                            effect_id: retry,
                            run_id: b.run_id,
                            outcome: CaptureStopOutcome::StillActive(ErrorCode(100)),
                        },
                    );
                    if let Some(next) = effects.iter().find_map(|e| match e {
                        CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                        _ => None,
                    }) {
                        retry = next;
                    }
                }
                let mut expected = state.capture;
                let owner = match &mut expected {
                    CaptureState::Stopping {
                        effect_id,
                        finalize_after,
                        ..
                    } => {
                        *finalize_after = true;
                        Some(*effect_id)
                    }
                    CaptureState::StopUncertain {
                        active_effect,
                        finalize_after,
                        ..
                    } => {
                        *finalize_after = true;
                        *active_effect
                    }
                    _ => unreachable!(),
                };
                let terminal = ContinuationEvent::TerminalObserved {
                    logical_run_id: key.logical_run_id,
                    connection_generation: key.connection_generation,
                };
                let notification = match settlement {
                    3 | 4 => CoordinatorEvent::RuntimeFailed {
                        run_id: key.logical_run_id,
                        error: ErrorCode(110),
                    },
                    5 | 6 => CoordinatorEvent::NegotiatedRuntimeFailed {
                        run_id: key.logical_run_id,
                        error: ErrorCode(110),
                    },
                    _ => CoordinatorEvent::Continuation(terminal),
                };
                let mut effects = Vec::new();
                if matches!(settlement, 0 | 3 | 5) {
                    effects.extend(reduce(&mut state, notification));
                }
                effects.extend(event(
                    &mut state,
                    ContinuationEvent::ContinueFinished {
                        effect_id: attach,
                        key,
                        run_id: b.run_id,
                        generation,
                        outcome: if settlement == 2 {
                            ContinueAttachOutcome::AttemptedFailure(ErrorCode(101))
                        } else {
                            ContinueAttachOutcome::Attached {
                                context_revision: 7,
                            }
                        },
                    },
                ));
                if matches!(settlement, 1 | 4 | 6) {
                    effects.extend(reduce(&mut state, notification));
                }
                assert_eq!(
                    state.capture, expected,
                    "failures={failures}, settlement={settlement}"
                );
                assert!(!effects.iter().any(|e| matches!(
                    e,
                    CoordinatorEffect::StopRecording { .. }
                        | CoordinatorEffect::FinalizeRecording { .. }
                )));
                if let Some(owner) = owner {
                    let effects = reduce(
                        &mut state,
                        CoordinatorEvent::CaptureStopped {
                            effect_id: owner,
                            run_id: b.run_id,
                            outcome: CaptureStopOutcome::Inactive,
                        },
                    );
                    assert_eq!(
                        effects
                            .iter()
                            .filter(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. }))
                            .count(),
                        1
                    );
                    assert!(matches!(state.capture, CaptureState::Idle));
                } else {
                    assert!(matches!(
                        state.capture,
                        CaptureState::StopUncertain {
                            active_effect: None,
                            ..
                        }
                    ));
                }
                assert!(state.validate().is_ok());
            }
        }
    }

    #[test]
    fn runtime_failure_preserves_outstanding_first_seal_before_and_after_attachment() {
        for negotiated in [false, true] {
            for attached_first in [false, true] {
                for stop_fails in [false, true] {
                    let (mut state, a) = active();
                    pause(&mut state, a, 1);
                    let (prepare, b) = start(&mut state);
                    let effects = prepared(&mut state, prepare, b, 22);
                    let (attach, key, _, generation) = continue_effect(&effects);
                    let effects = reduce(
                        &mut state,
                        CoordinatorEvent::Intent(RecordingIntent::stop(
                            IntentSource::Frontend,
                            None,
                        )),
                    );
                    let seal = effects
                        .iter()
                        .find_map(|e| match e {
                            CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                                effect_id,
                                ..
                            }) => Some(*effect_id),
                            _ => None,
                        })
                        .unwrap();
                    let attached =
                        CoordinatorEvent::Continuation(ContinuationEvent::ContinueFinished {
                            effect_id: attach,
                            key,
                            run_id: b.run_id,
                            generation,
                            outcome: ContinueAttachOutcome::Attached {
                                context_revision: 7,
                            },
                        });
                    let failure = if negotiated {
                        CoordinatorEvent::NegotiatedRuntimeFailed {
                            run_id: key.logical_run_id,
                            error: ErrorCode(110),
                        }
                    } else {
                        CoordinatorEvent::RuntimeFailed {
                            run_id: key.logical_run_id,
                            error: ErrorCode(110),
                        }
                    };
                    let mut effects =
                        reduce(&mut state, if attached_first { attached } else { failure });
                    effects.extend(reduce(
                        &mut state,
                        if attached_first { failure } else { attached },
                    ));
                    assert!(!effects.iter().any(|e| matches!(
                        e,
                        CoordinatorEffect::StopRecording { .. }
                            | CoordinatorEffect::FinalizeRecording { .. }
                    )));
                    assert_eq!(state.pending_episode.unwrap().seal_effect, Some(seal));
                    assert!(
                        matches!(state.capture, CaptureState::Recording { run } if run.run_id == b.run_id)
                    );
                    let mut effects = event(
                        &mut state,
                        ContinuationEvent::PendingCaptureStopped {
                            effect_id: seal,
                            run_id: b.run_id,
                            generation,
                            outcome: if stop_fails {
                                CaptureStopOutcome::StillActive(ErrorCode(99))
                            } else {
                                CaptureStopOutcome::Inactive
                            },
                        },
                    );
                    if stop_fails {
                        assert!(!effects
                            .iter()
                            .any(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. })));
                        let retry = effects
                            .iter()
                            .find_map(|e| match e {
                                CoordinatorEffect::StopRecording {
                                    effect_id,
                                    attempt: 1,
                                    ..
                                } => Some(*effect_id),
                                _ => None,
                            })
                            .unwrap();
                        effects = reduce(
                            &mut state,
                            CoordinatorEvent::CaptureStopped {
                                effect_id: retry,
                                run_id: b.run_id,
                                outcome: CaptureStopOutcome::Inactive,
                            },
                        );
                    }
                    assert_eq!(
                        effects
                            .iter()
                            .filter(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. }))
                            .count(),
                        1
                    );
                    assert!(matches!(state.capture, CaptureState::Idle));
                    assert!(state.pending_episode.is_none());
                    assert!(state.validate().is_ok());
                }
            }
        }
    }

    #[test]
    fn teardown_cancels_inflight_continue_without_replacing_physical_retry() {
        for teardown in [
            CoordinatorEvent::ShutdownRequested,
            CoordinatorEvent::ForceOff(StopReason::SystemSleep),
        ] {
            for uncertain in [false, true] {
                let (mut state, b, attach, _, _, retry) = pending_continue_with_failed_seal();
                if uncertain {
                    reduce(
                        &mut state,
                        CoordinatorEvent::CaptureStopped {
                            effect_id: retry,
                            run_id: b.run_id,
                            outcome: CaptureStopOutcome::StillActive(ErrorCode(100)),
                        },
                    );
                }
                let owner = state.capture;
                let effects = reduce(&mut state, teardown);
                assert_eq!(state.capture, owner);
                assert_eq!(effects.iter().filter(|e| matches!(e,
                    CoordinatorEffect::CancelStart { effect_id, run_id } if *effect_id == attach && *run_id == b.run_id)).count(), 1);
                assert!(!effects.iter().any(|e| matches!(
                    e,
                    CoordinatorEffect::StopRecording { .. }
                        | CoordinatorEffect::Continuation(ContinuationEffect::SealPending { .. })
                )));
                let repeated = reduce(&mut state, teardown);
                assert!(!repeated
                    .iter()
                    .any(|e| matches!(e, CoordinatorEffect::CancelStart { .. })));
                assert_eq!(state.capture, owner);
                assert!(state.validate().is_ok());
            }
        }
    }

    #[test]
    fn actual_native_and_vad_stop_builder_seals_current_pending_capture_immediately() {
        for source in [IntentSource::Frontend, IntentSource::Vad] {
            let (mut state, a) = active();
            pause(&mut state, a, 1);
            let (prepare, b) = start(&mut state);
            prepared(&mut state, prepare, b, 22);
            let stop = state.current_capture_stop(source).unwrap();
            let effects = reduce(&mut state, stop);
            assert!(effects.iter().any(|effect| matches!(effect,
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending { run_id, generation: 22, cancel: false, .. }) if *run_id == b.run_id)));
            assert!(!effects
                .iter()
                .any(|effect| matches!(effect, CoordinatorEffect::CancelStart { .. })));
        }
    }

    #[test]
    fn reserved_native_registration_retires_on_supersession_before_prepare() {
        let (mut state, a) = active();
        assert!(state.register_native_candidate(a.run_id));
        reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
        );
        // A still holds the physical microphone. Each reopen reserves a native
        // target, then loses its intent before any PrepareCapture can run.
        for _ in 0..12 {
            state.panel = PanelState::Hidden;
            let effects = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
            );
            let reserved = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::ShowPanel {
                        run_id: Some(id), ..
                    } => Some(*id),
                    _ => None,
                })
                .unwrap();
            assert!(state.register_native_candidate(reserved));
            assert!(state.retire_native_candidates(|_| false).is_empty());
            reduce(
                &mut state,
                CoordinatorEvent::ForceOff(StopReason::SystemSleep),
            );
            assert_eq!(state.retire_native_candidates(|_| false), vec![reserved]);
            assert!(!state.register_native_candidate(reserved));
            assert!(state.native_registration_candidates.contains(&a.run_id));
        }
        assert_eq!(state.native_registration_candidates.len(), 1);
    }

    #[test]
    fn cancelled_unsent_native_b_retires_while_a_and_terminal_delivery_survive() {
        let (mut state, a) = active();
        assert!(state.register_native_candidate(a.run_id));
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        assert!(state.register_native_candidate(b.run_id));
        prepared(&mut state, prepare, b, 22);
        reduce(&mut state, CoordinatorEvent::ForceOff(StopReason::Shutdown));
        assert_eq!(
            state.retire_native_candidates(|id| id == a.run_id),
            vec![b.run_id]
        );
        assert!(!state.register_native_candidate(b.run_id));
        // A's registration also survives finalization while its delivery ACK is pending.
        assert!(!state.finish_native_candidate(a.run_id, true));
        assert!(state.native_registration_candidates.contains(&a.run_id));
    }

    #[test]
    fn nonnegotiated_runs_retire_without_terminal_ack_across_twelve_cycles() {
        let mut state = CoordinatorState::default();
        for generation in 1..=12 {
            let (prepare, run) = start(&mut state);
            assert!(state.register_native_candidate(run.run_id));
            let effects = prepared(&mut state, prepare, run, generation);
            let connect = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StartRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            reduce(
                &mut state,
                CoordinatorEvent::StartFinished {
                    effect_id: connect,
                    run_id: run.run_id,
                    outcome: StartOutcome::Succeeded,
                },
            );
            assert!(state.retire_native_candidates(|_| false).is_empty());
            let effects = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
            );
            let stop = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            let effects = reduce(
                &mut state,
                CoordinatorEvent::CaptureStopped {
                    effect_id: stop,
                    run_id: run.run_id,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            let finalize = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                    _ => None,
                })
                .unwrap();
            assert!(state.retire_native_candidates(|_| false).is_empty());
            // Same production hook as FinalizeRecording after terminal publication.
            assert!(state.finish_native_candidate(run.run_id, false));
            assert!(!state.finish_native_candidate(run.run_id, false));
            reduce(
                &mut state,
                CoordinatorEvent::FinalizeFinished {
                    effect_id: finalize,
                    run_id: run.run_id,
                    outcome: FinalizeOutcome::NoTranscript,
                },
            );
            assert!(state.native_registration_candidates.is_empty());
        }
    }

    #[test]
    fn logical_error_dispatch_stops_attached_b_and_finalizes_a_once() {
        let (mut state, a) = active();
        assert!(state.register_native_candidate(a.run_id));
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        assert!(state.register_native_candidate(b.run_id));
        let effects = prepared(&mut state, prepare, b, 22);
        let (effect_id, key, _, generation) = continue_effect(&effects);
        event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Attached {
                    context_revision: 1,
                },
            },
        );
        assert_eq!(state.retire_native_candidates(|_| false), vec![b.run_id]);
        let failure = CoordinatorEvent::RuntimeFailed {
            run_id: a.run_id,
            error: ErrorCode(8),
        };
        let effects = reduce(&mut state, failure);
        let stop = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::StopRecording {
                    effect_id, run_id, ..
                } if *run_id == b.run_id => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let duplicate = reduce(&mut state, failure);
        assert!(!duplicate.iter().any(|e| matches!(
            e,
            CoordinatorEffect::FinalizeRecording { .. } | CoordinatorEffect::StopRecording { .. }
        )));
        let effects = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: b.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        assert_eq!(effects.iter().filter(|e| matches!(e, CoordinatorEffect::FinalizeRecording { run_id, .. } if *run_id == a.run_id)).count(), 1);
        let finalize = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        assert!(!state.finish_native_candidate(a.run_id, true));
        reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalize,
                run_id: a.run_id,
                outcome: FinalizeOutcome::NoTranscript,
            },
        );
        assert!(state.continuation.is_none());
        assert!(state
            .retire_native_candidates(|id| id == a.run_id)
            .is_empty());
        // Once the terminal ACK retires delivery ownership, reconciliation can
        // forget the tracking entry (the ACK also releases the native manager).
        assert_eq!(state.retire_native_candidates(|_| false), vec![a.run_id]);
    }

    #[test]
    fn reopen_reserves_target_identity_before_old_microphone_release() {
        let (mut state, a) = active();
        state.panel = PanelState::Hidden;
        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
        );
        let stop = effects
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        state.panel = PanelState::Hidden;
        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        let (reserved, revision) = effects
            .iter()
            .find_map(|effect| match effect {
                CoordinatorEffect::ShowPanel {
                    run_id: Some(run),
                    revision,
                    ..
                } => Some((*run, *revision)),
                _ => None,
            })
            .unwrap();
        assert_ne!(reserved, a.run_id);
        assert!(state.owns_intent_run(reserved, revision));
        let effects = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: a.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        assert!(effects.iter().any(|effect| matches!(effect, CoordinatorEffect::PrepareCapture { run, .. } if run.run_id == reserved)));
    }

    #[test]
    fn start_during_delayed_pause_acceptance_retains_intent_without_early_continue() {
        let (mut state, a) = active();
        let effects = reduce_at(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
            1_000_000_000,
        );
        let stop = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::StopRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let effects = reduce_at(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: a.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
            1_020_000_000,
        );
        let (pause_id, key) = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::Continuation(ContinuationEffect::Pause {
                    effect_id,
                    key,
                    ..
                }) => Some((*effect_id, *key)),
                _ => None,
            })
            .unwrap();
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        assert!(state.desired_recording.is_on());
        assert!(matches!(state.capture, CaptureState::Buffering { run, .. } if run == b));
        assert!(!effects.iter().any(|e| matches!(
            e,
            CoordinatorEffect::StartRecording { .. }
                | CoordinatorEffect::Continuation(ContinuationEffect::Continue { .. })
        )));
        let effects = event(
            &mut state,
            ContinuationEvent::PauseFinished {
                effect_id: pause_id,
                key,
                pause_epoch: Some(1),
            },
        );
        let (_, continued_key, run, _) = continue_effect(&effects);
        assert_eq!(run, b);
        assert_eq!(continued_key.pause_epoch, 1);
        assert_eq!(
            state.continuation.unwrap().stopped_at_ns,
            Some(1_000_000_000)
        );
        assert_eq!(state.processing_jobs.len(), 1);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn repeated_episodes_keep_one_job_and_never_close_logical_delivery_on_pause() {
        let (mut state, mut run) = active();
        let logical = run.run_id;
        let mut old_keys = Vec::new();
        for epoch in 1..=30 {
            let paused_key = pause(&mut state, run, epoch);
            for key in &old_keys {
                let effects = event(&mut state, ContinuationEvent::WindowElapsed { key: *key });
                assert!(
                    effects.is_empty(),
                    "old timer must not affect a later pause"
                );
                assert_eq!(state.continuation.unwrap().key, paused_key);
                assert_eq!(
                    state.continuation.unwrap().phase,
                    LogicalPhase::PausedReclaimable
                );
            }
            old_keys.push(paused_key);
            assert_eq!(state.processing_jobs.len(), 1);
            assert!(state.processing_jobs.contains_key(&logical));
            let (prepare, new_run) = start(&mut state);
            let effects = prepared(&mut state, prepare, new_run, 10 + epoch);
            let (effect_id, key, new_run, generation) = continue_effect(&effects);
            assert_eq!(key.logical_run_id, logical);
            let effects = event(
                &mut state,
                ContinuationEvent::ContinueFinished {
                    effect_id,
                    key,
                    run_id: new_run.run_id,
                    generation,
                    outcome: ContinueAttachOutcome::Attached {
                        context_revision: epoch,
                    },
                },
            );
            assert!(!effects.iter().any(|e| matches!(
                e,
                CoordinatorEffect::StartRecording { .. }
                    | CoordinatorEffect::FinalizeRecording { .. }
            )));
            for key in &old_keys {
                let effects = event(&mut state, ContinuationEvent::WindowElapsed { key: *key });
                assert!(
                    effects.is_empty(),
                    "expired pause timer must not stop attached capture"
                );
                assert!(matches!(state.capture,CaptureState::Recording{run} if run==new_run));
            }
            assert!(matches!(state.capture,CaptureState::Recording{run} if run==new_run));
            assert_eq!(state.processing_jobs.len(), 1);
            assert!(state.validate().is_ok());
            run = new_run;
        }
    }
    #[test]
    fn stop_before_prepare_finishes_seals_and_busy_preserves_same_episode() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::HoldHotkey, None)),
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::CancelPrepare { .. })));
        let effects = prepared(&mut state, prepare, b, 22);
        let seal = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                    effect_id,
                    cancel: false,
                    ..
                }) => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let effects = reduce(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::start(IntentSource::Frontend, None)),
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::PrepareCapture { .. })));
        assert_eq!(state.capture.run().unwrap().run_id, b.run_id);
        let effects = event(
            &mut state,
            ContinuationEvent::PendingCaptureStopped {
                effect_id: seal,
                run_id: b.run_id,
                generation: 22,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        let (_, _, run, generation) = continue_effect(&effects);
        assert_eq!((run.run_id, generation), (b.run_id, 22));
        assert!(!state.desired_recording.is_on());
    }
    #[test]
    fn admitted_pending_toggle_seals_once_for_continue_and_cold_buffer() {
        for cold in [false, true] {
            let (mut state, a) = active();
            pause(&mut state, a, 1);
            if cold {
                state.continuation = None;
            }
            let (prepare, b) = start(&mut state);
            prepared(&mut state, prepare, b, 22);
            let effects = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::toggle(
                    IntentSource::Frontend,
                    GestureId::new(100),
                )),
            );
            let seal = effects
                .iter()
                .find_map(|effect| match effect {
                    CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                        effect_id,
                        run_id,
                        generation: 22,
                        cancel: false,
                    }) if *run_id == b.run_id => Some(*effect_id),
                    _ => None,
                })
                .expect("admitted audio must be sealed");
            assert!(!effects.iter().any(|e| matches!(
                e,
                CoordinatorEffect::CancelStart { .. } | CoordinatorEffect::StopRecording { .. }
            )));
            let repeated = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::HoldHotkey, None)),
            );
            assert!(!repeated.iter().any(|e| matches!(
                e,
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending { .. })
                    | CoordinatorEffect::CancelStart { .. }
            )));
            event(
                &mut state,
                ContinuationEvent::PendingCaptureStopped {
                    effect_id: seal,
                    run_id: b.run_id,
                    generation: 23,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            assert_eq!(state.pending_episode.unwrap().seal_effect, Some(seal));
            event(
                &mut state,
                ContinuationEvent::PendingCaptureStopped {
                    effect_id: seal,
                    run_id: b.run_id,
                    generation: 22,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            assert!(state.pending_episode.unwrap().sealed);
            assert_eq!(
                state.pending_episode.unwrap().disposition,
                PendingDisposition::Seal
            );
            if cold {
                let mut finalization = Vec::new();
                begin_terminal_finalize(&mut state, a.run_id, 1, &mut finalization);
                let finalizer = finalization
                    .iter()
                    .find_map(|e| match e {
                        CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                        _ => None,
                    })
                    .unwrap();
                let effects = reduce(
                    &mut state,
                    CoordinatorEvent::FinalizeFinished {
                        effect_id: finalizer,
                        run_id: a.run_id,
                        outcome: FinalizeOutcome::Committed,
                    },
                );
                let starts: Vec<_> = effects
                    .iter()
                    .filter_map(|e| match e {
                        CoordinatorEffect::StartRecording { effect_id, run } if *run == b => {
                            Some(*effect_id)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(starts.len(), 1);
                let effects = reduce(
                    &mut state,
                    CoordinatorEvent::StartFinished {
                        effect_id: starts[0],
                        run_id: b.run_id,
                        outcome: StartOutcome::Succeeded,
                    },
                );
                assert!(effects.iter().any(|e| matches!(e,
                    CoordinatorEffect::StopRecording { run_id, .. } if *run_id == b.run_id)));
                assert!(matches!(
                    state.capture,
                    CaptureState::Stopping {
                        finalize_after: true,
                        ..
                    }
                ));
            }
            assert!(state.validate().is_ok());
        }
    }

    #[test]
    fn permission_revocation_discards_pending_admission_in_both_stop_orders() {
        for ordinary_first in [false, true] {
            let (mut state, a) = active();
            pause(&mut state, a, 1);
            let (prepare, b) = start(&mut state);
            if ordinary_first {
                reduce(
                    &mut state,
                    CoordinatorEvent::Intent(RecordingIntent::toggle(
                        IntentSource::Frontend,
                        GestureId::new(102),
                    )),
                );
            }
            reduce(
                &mut state,
                CoordinatorEvent::ForceOff(StopReason::PermissionRevoked),
            );
            if !ordinary_first {
                reduce(
                    &mut state,
                    CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::HoldHotkey, None)),
                );
            }
            let effects = prepared(&mut state, prepare, b, 22);
            assert!(!effects.iter().any(|e| matches!(
                e,
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                    cancel: false,
                    ..
                }) | CoordinatorEffect::StartRecording { .. }
            )));
            let stop = effects
                .iter()
                .find_map(|e| match e {
                    CoordinatorEffect::StopRecording {
                        effect_id, run_id, ..
                    } if *run_id == b.run_id => Some(*effect_id),
                    _ => None,
                })
                .expect("revoked admitted capture must be discarded");
            assert!(matches!(
                state.capture,
                CaptureState::Stopping {
                    finalize_after: false,
                    ..
                }
            ));
            reduce(
                &mut state,
                CoordinatorEvent::CaptureStopped {
                    effect_id: stop,
                    run_id: b.run_id,
                    outcome: CaptureStopOutcome::Inactive,
                },
            );
            assert_eq!(state.capture, CaptureState::Idle);
            assert!(state.pending_episode.is_none());
            assert!(state.validate().is_ok());
        }
    }

    #[test]
    fn pending_prepare_cancel_outcome_decides_admission() {
        for admitted in [false, true] {
            let (mut state, a) = active();
            pause(&mut state, a, 1);
            let (prepare, b) = start(&mut state);
            let effects = reduce(
                &mut state,
                CoordinatorEvent::Intent(RecordingIntent::toggle(
                    IntentSource::Frontend,
                    GestureId::new(101),
                )),
            );
            assert!(effects.iter().any(|e| matches!(e,
                CoordinatorEffect::CancelPrepare { effect_id, run_id }
                    if *effect_id == prepare && *run_id == b.run_id)));
            let effects = reduce(
                &mut state,
                CoordinatorEvent::PrepareFinished {
                    effect_id: prepare,
                    run_id: b.run_id,
                    outcome: if admitted {
                        PrepareOutcome::Succeeded { generation: 22 }
                    } else {
                        PrepareOutcome::Cancelled
                    },
                },
            );
            if admitted {
                assert!(effects.iter().any(|e| matches!(e,
                    CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                        run_id, generation: 22, cancel: false, .. }) if *run_id == b.run_id)));
            } else {
                assert_eq!(state.capture, CaptureState::Idle);
                assert!(state.pending_episode.is_none());
            }
            assert!(state.validate().is_ok());
        }
    }

    #[test]
    fn hard_force_off_cancels_unsent_episode_and_fences_late_completion() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        let (effect_id, key, _, generation) = continue_effect(&effects);
        let effects = reduce(&mut state, CoordinatorEvent::ForceOff(StopReason::Shutdown));
        assert!(effects.iter().any(
            |e| matches!(e,CoordinatorEffect::CancelStart{effect_id:owner,..} if *owner==effect_id)
        ));
        assert!(effects.iter().any(|e| matches!(
            e,
            CoordinatorEffect::Continuation(ContinuationEffect::SealPending { cancel: true, .. })
        )));
        event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation: generation + 1,
                outcome: ContinueAttachOutcome::Attached {
                    context_revision: 1,
                },
            },
        );
        assert!(matches!(state.capture, CaptureState::Starting { .. }));
        event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Cancelled,
            },
        );
        assert!(matches!(state.capture, CaptureState::Buffering { run, .. } if run == b));
        let seal = state.pending_episode.unwrap().seal_effect.unwrap();
        assert_eq!(state.processing_jobs.len(), 1);
        let effects = event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Attached {
                    context_revision: 1,
                },
            },
        );
        assert!(!effects.iter().any(|e| matches!(
            e,
            CoordinatorEffect::StopRecording { .. } | CoordinatorEffect::FinalizeRecording { .. }
        )));
        assert_eq!(state.capture.run().map(|r| r.run_id), Some(b.run_id));
        event(
            &mut state,
            ContinuationEvent::PendingCaptureStopped {
                effect_id: seal,
                run_id: b.run_id,
                generation,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        assert_eq!(state.capture, CaptureState::Idle);
        assert!(state.pending_episode.is_none());
    }
    #[test]
    fn unsent_buffer_cold_starts_once_only_after_released_logical_terminal() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        let (effect_id, key, _, generation) = continue_effect(&effects);
        let effects = event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Unsent,
            },
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::StartRecording { .. })));
        let effects = event(
            &mut state,
            ContinuationEvent::TerminalObserved {
                logical_run_id: a.run_id,
                connection_generation: 7,
            },
        );
        let finalizer = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::FinalizeRecording { effect_id, .. } => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let effects = reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalizer,
                run_id: a.run_id,
                outcome: FinalizeOutcome::Committed,
            },
        );
        assert_eq!(effects.iter().filter(|e|matches!(e,CoordinatorEffect::StartRecording{run,..} if run.run_id==b.run_id)).count(),1);
        let effects = reduce(
            &mut state,
            CoordinatorEvent::FinalizeFinished {
                effect_id: finalizer,
                run_id: a.run_id,
                outcome: FinalizeOutcome::Committed,
            },
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::StartRecording { .. })));
    }
    #[test]
    fn late_logical_failure_stops_attached_episode_instead_of_ignoring_old_id() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        let (effect_id, key, _, generation) = continue_effect(&effects);
        event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Attached {
                    context_revision: 7,
                },
            },
        );
        let effects = reduce(
            &mut state,
            CoordinatorEvent::RuntimeFailed {
                run_id: a.run_id,
                error: ErrorCode(9),
            },
        );
        let stop = effects
            .iter()
            .find_map(|e| match e {
                CoordinatorEffect::StopRecording {
                    effect_id, run_id, ..
                } if *run_id == b.run_id => Some(*effect_id),
                _ => None,
            })
            .unwrap();
        let effects = reduce(
            &mut state,
            CoordinatorEvent::CaptureStopped {
                effect_id: stop,
                run_id: b.run_id,
                outcome: CaptureStopOutcome::Inactive,
            },
        );
        assert!(effects.iter().any(
            |e| matches!(e,CoordinatorEffect::FinalizeRecording{run_id,..} if *run_id==a.run_id)
        ));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::PrepareCapture { .. })));
    }
    #[test]
    fn teardown_revokes_pending_continue_and_finalizes_after_compensation() {
        for reason in [StopReason::SystemSleep, StopReason::Shutdown] {
            let (mut state, a) = active();
            pause(&mut state, a, 1);
            let (prepare, b) = start(&mut state);
            let effects = prepared(&mut state, prepare, b, 22);
            let (effect_id, key, _, generation) = continue_effect(&effects);
            let effects = reduce(&mut state, CoordinatorEvent::ForceOff(reason));
            assert!(effects.iter().any(|e| matches!(e,
                CoordinatorEffect::CancelStart { effect_id: owner, .. } if *owner == effect_id)));
            assert!(effects.iter().any(|e| matches!(
                e,
                CoordinatorEffect::Continuation(ContinuationEffect::SealPending {
                    cancel: true,
                    ..
                })
            )));
            let effects = event(
                &mut state,
                ContinuationEvent::ContinueFinished {
                    effect_id,
                    key,
                    run_id: b.run_id,
                    generation,
                    outcome: ContinueAttachOutcome::Cancelled,
                },
            );
            assert!(effects.iter().any(|e| matches!(e,
                CoordinatorEffect::FinalizeRecording { run_id, .. } if *run_id == a.run_id)));
            assert!(!effects.iter().any(|e| matches!(
                e,
                CoordinatorEffect::StartRecording { .. }
                    | CoordinatorEffect::Continuation(ContinuationEffect::Continue { .. })
            )));
        }
    }

    #[test]
    fn sleeping_paused_owner_loses_reclaimability_immediately() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let effects = reduce(
            &mut state,
            CoordinatorEvent::ForceOff(StopReason::SystemSleep),
        );
        assert_eq!(state.continuation.unwrap().phase, LogicalPhase::Finalizing);
        assert!(effects.iter().any(|e| matches!(e,
            CoordinatorEffect::FinalizeRecording { run_id, .. } if *run_id == a.run_id)));
    }

    #[test]
    fn delayed_attach_does_not_restart_sealed_episodes_stop_clock() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        let (effect_id, key, _, generation) = continue_effect(&effects);
        reduce_at(
            &mut state,
            CoordinatorEvent::Intent(RecordingIntent::stop(IntentSource::Frontend, None)),
            2_000_000_000,
        );
        reduce_at(
            &mut state,
            CoordinatorEvent::Continuation(ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Attached {
                    context_revision: 7,
                },
            }),
            4_500_000_000,
        );
        assert_eq!(
            state.continuation.unwrap().stopped_at_ns,
            Some(2_000_000_000)
        );
    }

    #[test]
    fn grace_expiry_preserves_original_provider_drain_and_stale_epoch_cannot_expire_b() {
        let (mut state, a) = active();
        let key = pause(&mut state, a, 1);
        let effects = event(
            &mut state,
            ContinuationEvent::WindowElapsed {
                key: ContinuationKey {
                    pause_epoch: 2,
                    ..key
                },
            },
        );
        assert_eq!(
            state.continuation.unwrap().phase,
            LogicalPhase::PausedReclaimable
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. })));
        let effects = event(&mut state, ContinuationEvent::WindowElapsed { key });
        assert_eq!(state.continuation.unwrap().phase, LogicalPhase::Draining);
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. })));
    }
    #[test]
    fn terminal_during_continue_waits_for_attempt_ownership_before_finalizing() {
        let (mut state, a) = active();
        pause(&mut state, a, 1);
        let (prepare, b) = start(&mut state);
        let effects = prepared(&mut state, prepare, b, 22);
        let (effect_id, key, _, generation) = continue_effect(&effects);
        let effects = event(
            &mut state,
            ContinuationEvent::TerminalObserved {
                logical_run_id: a.run_id,
                connection_generation: 7,
            },
        );
        assert!(!effects
            .iter()
            .any(|e| matches!(e, CoordinatorEffect::FinalizeRecording { .. })));
        let effects = event(
            &mut state,
            ContinuationEvent::ContinueFinished {
                effect_id,
                key,
                run_id: b.run_id,
                generation,
                outcome: ContinueAttachOutcome::Unsent,
            },
        );
        assert!(effects.iter().any(
            |e| matches!(e,CoordinatorEffect::FinalizeRecording{run_id,..} if *run_id==a.run_id)
        ));
        assert!(matches!(state.capture,CaptureState::Buffering{run,..} if run.run_id==b.run_id));
    }
}

impl CoordinatorState {
    pub fn pending_capture_sealed(&self) -> bool {
        self.pending_episode.is_some_and(|pending| {
            pending.sealed || pending.disposition != PendingDisposition::Live
        })
    }
}

#[cfg(test)]
impl CoordinatorState {
    pub(crate) fn with_next_run_id_for_test(run_id: u64) -> Self {
        let mut state = Self::default();
        state.ids.run = run_id.checked_sub(1).unwrap();
        state
    }
}

#[cfg(test)]
pub(crate) use tests::{pending_continue_with_failed_seal, pending_continue_with_outstanding_seal};
