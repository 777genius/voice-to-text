//! Included by the Linux source-contract runner, not a native qualification test.
//! Production function bodies are extracted unchanged; no AX or GUI is invoked.

fn snapshot(text: &str) -> LocalSnapshot {
    LocalSnapshot {
        selection: TextRange {
            location: text.encode_utf16().count() as isize,
            length: 0,
        },
        total: text.encode_utf16().count() as isize,
        start: 0,
        anchor: text.encode_utf16().collect(),
    }
}

#[test]
fn e34_identical_final_state_cannot_prove_edit_authorship() {
    let before = snapshot("seed ");
    let expected = before.after_insertion("same").unwrap();
    let indistinguishable = snapshot("seed same");
    // Deliberately documents a residual limit, not a fabricated refusal.
    assert!(expected.contains(&indistinguishable));
    assert!(expected.same_insertion_point(&indistinguishable));
    let changed = snapshot("seed else");
    assert!(!expected.contains(&changed));
    assert!(!expected.same_insertion_point(&changed));
}

#[test]
fn e36_mini_window_is_validation_exception_but_never_selected_target() {
    let target = normalize_auto_paste_target("com.apple.TextEdit".into(), 42).unwrap();
    for bundle in VOICETEXT_BUNDLE_IDS {
        let mini = AutoPasteTarget {
            bundle_id: (*bundle).into(),
            pid: 99,
        };
        assert!(permits_frontmost_validation(
            &target,
            &mini,
            99,
            VOICETEXT_BUNDLE_IDS
        ));
        assert!(normalize_auto_paste_target(format!(" {bundle} "), 99).is_none());
        assert!(!continuation_app_qualified(&mini));
        assert!(!permits_frontmost_validation(
            &target,
            &mini,
            100,
            VOICETEXT_BUNDLE_IDS
        ));
    }
    assert_eq!(target.pid, 42);
}

#[test]
fn e50_unsupported_app_is_legacy_target_but_not_continuation_qualified() {
    for bundle in [
        "com.apple.Notes",
        "com.google.Chrome",
        "test.owned.Unsupported",
    ] {
        let target = normalize_auto_paste_target(bundle.into(), 42).unwrap();
        let eligible = continuation_app_qualified(&target);
        assert!(!eligible);
        let offered = Offer {
            continuation_opted_in: true,
        }
        .offered(
            "elevenlabs",
            &Config {
                continuation_target_eligible: eligible,
            },
        );
        assert!(!offered);
        let mode = ready_mode(offered);
        assert!(
            !mode,
            "unsolicited Ready capability must not enable fast path"
        );
        assert!(ready_mode(true));
        println!("E50_ELIGIBLE={mode}");
    }
    assert!(continuation_app_qualified(
        &normalize_auto_paste_target("com.apple.TextEdit".into(), 42).unwrap()
    ));
}

#[test]
fn e57_normalization_and_equal_length_readback_mismatch_are_not_confirmation() {
    let before = snapshot("seed ");
    let expected = before.after_insertion("e\u{301}").unwrap();
    assert!(expected.contains(&snapshot("seed e\u{301}")));
    assert!(!expected.contains(&snapshot("seed é")));
    // Same length/caret cannot conceal a contradictory anchor.
    assert!(!expected.contains(&snapshot("seed e!")));
}
