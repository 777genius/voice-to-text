use tauri_plugin_global_shortcut::Shortcut;

pub const DEFAULT_RECORDING_HOTKEY: &str = "CmdOrCtrl+Shift+X";

/// Best-effort normalizer for hotkey strings stored in config.
///
/// Why: some older frontend versions stored DOM `KeyboardEvent.code` tokens
/// like `Backquote`, which may not be accepted by the shortcut parser on all
/// platforms/versions. We try to migrate those tokens into a parseable form.
///
/// Returns:
/// - `Some(normalized)` if the resulting shortcut parses successfully
/// - `None` if we cannot produce a valid shortcut
pub fn normalize_recording_hotkey(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    // Fast path: already valid.
    if s.parse::<Shortcut>().is_ok() {
        return Some(s.to_string());
    }

    // Tokenize and map known "code" tokens to characters.
    let parts: Vec<&str> = s
        .split('+')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if parts.is_empty() {
        return None;
    }

    let mapped: Vec<String> = parts.into_iter().map(map_key_token).collect();
    let candidate = mapped.join("+");

    if candidate.parse::<Shortcut>().is_ok() {
        return Some(candidate);
    }

    None
}

fn map_key_token(token: &str) -> String {
    match token {
        // Common DOM `KeyboardEvent.code` tokens.
        "Backquote" => "`".to_string(),
        "Minus" => "-".to_string(),
        "Equal" => "=".to_string(),
        "BracketLeft" => "[".to_string(),
        "BracketRight" => "]".to_string(),
        "Backslash" => "\\".to_string(),
        "IntlBackslash" => "\\".to_string(),
        "Semicolon" => ";".to_string(),
        "Quote" => "'".to_string(),
        "Comma" => ",".to_string(),
        "Period" => ".".to_string(),
        "Slash" => "/".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_valid_shortcut() {
        let s = "CmdOrCtrl+Shift+X";
        let out = normalize_recording_hotkey(s).expect("must be valid");
        assert_eq!(out, s);
    }

    #[test]
    fn normalize_converts_backquote_token() {
        // We don't know which token form the parser accepts in every environment,
        // but `normalize_recording_hotkey` must at least produce a parseable shortcut.
        let out = normalize_recording_hotkey("CmdOrCtrl+Backquote").expect("must be valid after normalize");
        assert!(
            out == "CmdOrCtrl+`" || out == "CmdOrCtrl+Backquote",
            "unexpected normalize result: {}",
            out
        );
        assert!(out.parse::<Shortcut>().is_ok(), "normalized shortcut must parse: {}", out);
    }
}

