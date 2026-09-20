//! Adapters for the reducer's negotiated route. These execute after the reducer
//! lock is released and use the real service/capture/provider/native interfaces.
use super::*;
use recording_intent::{ContinuationEffect as Effect, ContinuationEvent as Event};

// Lock ordering is coordinator -> delivery, shared with event reconciliation.
pub(super) fn retire_abandoned(state: &AppState) {
    let retired = {
        let mut coordinator = state
            .recording_intent_coordinator
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut delivery = state
            .continuation_delivery_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        runtime_failure::retire_abandoned(&mut coordinator, &mut delivery)
    };
    let manager = state.continuation_context.clone();
    if !retired.is_empty() {
        tauri::async_runtime::spawn(async move {
            for id in retired {
                manager.release(id.get()).await;
            }
        });
    }
}

pub(super) async fn finish_registration(state: &AppState, run: recording_intent::RunId) {
    let retire = {
        let mut coordinator = state
            .recording_intent_coordinator
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut delivery = state
            .continuation_delivery_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        runtime_failure::finish_registration(&mut coordinator, &mut delivery, run)
    };
    if retire {
        state.continuation_context.release(run.get()).await;
    }
}

fn reserve_registration(state: &AppState, run: recording_intent::RunId) -> bool {
    state
        .recording_intent_coordinator
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .register_native_candidate(run)
}

fn emit(app: AppHandle, event: Event) {
    dispatch_recording_coordinator_event(
        app,
        recording_intent::CoordinatorEvent::Continuation(event),
    );
}
fn stopped_at(monotonic_ns: u64) -> tokio::time::Instant {
    tokio::time::Instant::now()
        - Duration::from_nanos(coordinator_monotonic_ns().saturating_sub(monotonic_ns))
}
fn offered(config: &AppConfig) -> bool {
    config.recording_mode == RecordingMode::Dictation
        && config.stt.provider == crate::domain::SttProviderType::Backend
        && config.stt.backend_streaming_provider
            == crate::domain::BackendStreamingProvider::ElevenLabs
        && crate::infrastructure::stt::continuation_opt_in()
}

pub(super) fn observe_ready(app: AppHandle, run_id: recording_intent::RunId) {
    if !crate::infrastructure::stt::continuation_opt_in() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let service = state.transcription_service.clone();
        let config = service.get_config_snapshot();
        if config.provider != crate::domain::SttProviderType::Backend
            || config.backend_streaming_provider
                != crate::domain::BackendStreamingProvider::ElevenLabs
        {
            return;
        }
        drop(state);
        // Actual Ready can follow start_stream's return. This observer cannot
        // promote a completed/changed run, and never synthesizes acceptance.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(session) = service.continuation_session_for_run(run_id.get()).await {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Some(policy) = service.continuation_policy(run_id.get()) {
                        if !state
                            .continuation_delivery_runs
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .accept(run_id.get(), policy.auto_copy)
                        {
                            return;
                        }
                    }
                }

                emit(
                    app,
                    Event::Negotiated {
                        logical_run_id: run_id,
                        connection_generation: session.connection_generation,
                    },
                );
                return;
            }
            if service.logical_provider_run_id() != run_id.get()
                || tokio::time::Instant::now() >= deadline
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

// Once admitted, await settlement even if cancellation arrives during a write.
// Only an operation not yet admitted can be classified as cancelled here.
pub(super) async fn admit_continue(
    cancelled: &AtomicBool,
    resource: impl std::future::Future<Output = recording_intent::ContinueAttachOutcome>,
) -> recording_intent::ContinueAttachOutcome {
    if check_recording_resource_cancellation(cancelled).is_err() {
        recording_intent::ContinueAttachOutcome::Cancelled
    } else {
        resource.await
    }
}

pub(super) fn execute(app: AppHandle, effect: Effect) {
    // Ownership was registered under ordered reduction, before effects escaped.
    // Ordinary Stop sealing does not set this token.
    let cancelled = if let Effect::Continue { effect_id, .. } = effect {
        Some(app.try_state::<AppState>().map_or_else(
            || Arc::new(AtomicBool::new(true)),
            |state| {
                deferred_recording_effect_token(&state.recording_start_cancellations, effect_id)
            },
        ))
    } else {
        None
    };
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let service = state.transcription_service.clone();
        match effect {
            Effect::Pause {
                effect_id,
                key,
                stopped_at_ns,
            } => {
                let result = service
                    .pause_for_continuation(key.logical_run_id.get(), stopped_at(stopped_at_ns))
                    .await;
                let lease = result.ok().filter(|lease| {
                    lease.session.connection_generation == key.connection_generation
                });
                let pause_epoch = lease.as_ref().map(|lease| lease.pause_epoch);
                let continue_window = lease
                    .as_ref()
                    .map(|lease| lease.continue_window)
                    .unwrap_or(Duration::from_secs(2));
                drop(state);
                emit(
                    app,
                    Event::PauseFinished {
                        effect_id,
                        key,
                        pause_epoch,
                        continue_window,
                    },
                );
            }
            Effect::Continue {
                effect_id,
                key,
                run,
                generation,
            } => {
                let token = crate::application::PreparedCaptureToken {
                    run_id: run.run_id.get(),
                    generation,
                };
                let lease = service
                    .paused_continuation_snapshot()
                    .await
                    .filter(|lease| {
                        lease.logical_run_id == key.logical_run_id.get()
                            && lease.session.connection_generation == key.connection_generation
                            && lease.pause_epoch == key.pause_epoch
                    });
                let cancelled = cancelled.unwrap();
                let outcome = admit_continue(&cancelled, async {
                    if let Some(lease) = lease {
                        // Deliberately never cancel/drop this future. It owns any
                        // accepted Restore and any attempted write through settlement.
                        match service
                            .continue_prepared_capture(
                                token,
                                lease,
                                tokio::time::Instant::now(),
                                cancelled.clone(),
                            )
                            .await
                        {
                            Ok(crate::application::ContinueCaptureOutcome::Attached {
                                context_revision,
                                ..
                            }) => {
                                state
                                    .prepared_capture_tokens
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .remove(&run.run_id.get());
                                finish_registration(state.inner(), run.run_id).await;
                                recording_intent::ContinueAttachOutcome::Attached {
                                    context_revision,
                                }
                            }
                            Ok(crate::application::ContinueCaptureOutcome::Unsent(_)) => {
                                recording_intent::ContinueAttachOutcome::Unsent
                            }
                            Ok(crate::application::ContinueCaptureOutcome::Cancelled) => {
                                recording_intent::ContinueAttachOutcome::Cancelled
                            }
                            Err(error) => {
                                // First-write failure consumes B irreversibly. Retire
                                // its routing token without claiming physical release.
                                if retire_consumed_prepared_capture(
                                    &service,
                                    &state.prepared_capture_tokens,
                                    token,
                                )
                                .await
                                {
                                    finish_registration(state.inner(), run.run_id).await;
                                }
                                recording_intent::ContinueAttachOutcome::AttemptedFailure(
                                    recording_intent_error_code(&error.to_string()),
                                )
                            }
                        }
                    } else {
                        recording_intent::ContinueAttachOutcome::Unsent
                    }
                })
                .await;
                state
                    .recording_start_cancellations
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&effect_id.get());
                drop(state);
                emit(
                    app,
                    Event::ContinueFinished {
                        effect_id,
                        key,
                        run_id: run.run_id,
                        generation,
                        outcome,
                    },
                );
            }
            Effect::SealPending {
                effect_id,
                run_id,
                generation,
                cancel,
            } => {
                let token = crate::application::PreparedCaptureToken {
                    run_id: run_id.get(),
                    generation,
                };
                let result = service.stop_pending_capture(token, cancel).await;
                let active = service.capture_is_active_for_run(run_id.get()).await;
                let outcome = match (result, active) {
                    (Ok(_), false) => recording_intent::CaptureStopOutcome::Inactive,
                    (Err(error), false) => recording_intent::CaptureStopOutcome::FailedButInactive(
                        recording_intent_error_code(&error.to_string()),
                    ),
                    (_, true) => recording_intent::CaptureStopOutcome::StillActive(
                        recording_intent::ErrorCode(1),
                    ),
                };
                if cancel && !active {
                    state
                        .prepared_capture_tokens
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&run_id.get());
                    finish_registration(state.inner(), run_id).await;
                }
                drop(state);
                emit(
                    app,
                    Event::PendingCaptureStopped {
                        effect_id,
                        run_id,
                        generation,
                        outcome,
                    },
                );
            }
            Effect::WaitForWindow {
                key,
                stopped_at_ns,
                continue_window,
            } => {
                drop(state);
                tokio::time::sleep_until(stopped_at(stopped_at_ns) + continue_window).await;
                emit(app, Event::WindowElapsed { key });
            }
            Effect::ObserveTerminal {
                logical_run_id,
                connection_generation,
            } => {
                drop(state);
                loop {
                    let observation = service.continuation_observation(logical_run_id.get()).await;
                    let notify = match observation {
                        Some((session, notify, terminal))
                            if session.connection_generation == connection_generation =>
                        {
                            if terminal {
                                emit(
                                    app,
                                    Event::TerminalObserved {
                                        logical_run_id,
                                        connection_generation,
                                    },
                                );
                                return;
                            }
                            notify
                        }
                        _ => {
                            if service
                                .completed_report_for_run(logical_run_id.get())
                                .await
                                .is_some()
                            {
                                emit(
                                    app,
                                    Event::TerminalObserved {
                                        logical_run_id,
                                        connection_generation,
                                    },
                                );
                            }
                            return;
                        }
                    };
                    // Timeout also handles a lifecycle notification before this
                    // wait was armed. At most one monitor exists per logical run.
                    if let Some(notify) = notify {
                        let _ = tokio::time::timeout(Duration::from_millis(250), notify.notified())
                            .await;
                    } else {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        }
    });
}

pub(super) fn show_after_context_capture(
    app: &AppHandle,
    effect_id: recording_intent::EffectId,
    revision: recording_intent::IntentRevision,
    run_id: Option<recording_intent::RunId>,
    policy: recording_intent::RuntimePolicySnapshot,
) -> bool {
    let Some(run_id) = run_id else {
        return false;
    };
    let Some(state) = app.try_state::<AppState>() else {
        return false;
    };
    let config = state
        .recording_intent_policy_snapshots
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&policy.version)
        .cloned();
    let Some(config) = config.filter(offered) else {
        return false;
    };
    if config.auto_paste_text {
        bind_or_capture_auto_paste_target_for_run(state.inner(), run_id.get(), revision.get());
    }
    if !reserve_registration(state.inner(), run_id) {
        return false;
    }
    let target = resolve_auto_paste_target(state.inner(), Some(run_id.get()));
    let manager = state.continuation_context.clone();
    let app = app.clone();
    drop(state);
    tauri::async_runtime::spawn(async move {
        // PrepareCapture runs independently; no AX work is under its microphone
        // lock. Await native capture BEFORE granting the panel focus.
        let result = manager
            .capture(run_id.get(), target, config.auto_paste_text)
            .await;
        let Some(state) = app.try_state::<AppState>() else {
            manager.release(run_id.get()).await;
            return;
        };
        if !matches!(result, crate::domain::ContextValidation::Valid { .. }) {
            finish_registration(state.inner(), run_id).await;
        }
        retire_abandoned(state.inner());
        let still_current = {
            let coordinator = state
                .recording_intent_coordinator
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            coordinator.owns_intent_run(run_id, revision)
                && matches!(coordinator.panel, recording_intent::PanelState::Showing { effect_id: owner, .. } if owner == effect_id)
        };
        let outcome = if !still_current {
            recording_intent::WindowOutcome::Superseded {
                window_epoch: state.recording_window_lifecycle.current(),
            }
        } else if let Some(window) = app.get_webview_window("main") {
            match show_webview_window_with_recording_config(&window, &config, state.inner()) {
                Ok(()) => recording_intent::WindowOutcome::Applied {
                    window_epoch: state.recording_window_lifecycle.current(),
                },
                Err(error) => {
                    recording_intent::WindowOutcome::Failed(recording_intent_error_code(&error))
                }
            }
        } else {
            recording_intent::WindowOutcome::Failed(recording_intent::ErrorCode(2))
        };
        state
            .recording_panel_intent_started_at
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&revision.get());
        drop(state);
        dispatch_recording_coordinator_event(
            app,
            recording_intent::CoordinatorEvent::WindowFinished { effect_id, outcome },
        );
    });
    true
}

pub(super) fn serialized_phase(
    phase: recording_intent::LogicalPhase,
) -> crate::presentation::events::RecordingContinuationPhase {
    use crate::presentation::events::RecordingContinuationPhase as Payload;
    use recording_intent::LogicalPhase as Phase;
    match phase {
        Phase::Active => Payload::Active,
        Phase::Pausing => Payload::Pausing,
        Phase::PausedReclaimable => Payload::PausedReclaimable,
        Phase::ContinuePending => Payload::ContinuePending,
        Phase::Draining | Phase::Finalizing => Payload::Finalizing,
    }
}

// No panel focus transfer is requested in this policy. Register concurrently with
// capture preparation, without coupling microphone progress to the native executor.
pub(super) fn capture_without_panel(app: &AppHandle, run: recording_intent::RunContext) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let config = state
        .recording_intent_policy_snapshots
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&run.policy.version)
        .cloned();
    let Some(config) = config.filter(offered) else {
        return;
    };
    if !reserve_registration(state.inner(), run.run_id) {
        return;
    }
    let target = resolve_auto_paste_target(state.inner(), Some(run.run_id.get()));
    let manager = state.continuation_context.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = manager
            .capture(run.run_id.get(), target, config.auto_paste_text)
            .await;
        if let Some(state) = app.try_state::<AppState>() {
            if !matches!(result, crate::domain::ContextValidation::Valid { .. }) {
                finish_registration(state.inner(), run.run_id).await;
            }
            retire_abandoned(state.inner());
        } else {
            manager.release(run.run_id.get()).await;
        }
    });
}
