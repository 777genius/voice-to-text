use super::Transcription;

/// In-memory history ownership is independent of the current capture episode.
/// Legacy providers still append segment entries; negotiated runs update one entry.
#[derive(Default)]
pub struct TranscriptionHistory {
    entries: Vec<(Option<u64>, Transcription)>,
}
impl TranscriptionHistory {
    pub fn record(&mut self, logical_run: Option<u64>, segment: Transcription, limit: usize) {
        if segment.text.is_empty() {
            return;
        }
        if let Some((_, entry)) = logical_run.and_then(|id| {
            self.entries
                .iter_mut()
                .find(|(owner, _)| *owner == Some(id))
        }) {
            if !entry.text.ends_with(char::is_whitespace)
                && !segment.text.starts_with(char::is_whitespace)
            {
                entry.text.push(' ');
            }
            entry.text.push_str(&segment.text);
            entry.delivery_seq = segment.delivery_seq;
            entry.timing_known &= segment.timing_known;
            if entry.timing_known {
                let end = (entry.start + entry.duration).max(segment.start + segment.duration);
                entry.start = entry.start.min(segment.start);
                entry.duration = end - entry.start;
            } else {
                entry.start = 0.0;
                entry.duration = 0.0;
            }
            entry.confidence = entry
                .confidence
                .zip(segment.confidence)
                .map(|(a, b)| a.min(b));
            if entry.language != segment.language {
                entry.language = None;
            }
        } else {
            self.entries.push((logical_run, segment));
        }
        if self.entries.len() > limit {
            self.entries.drain(..self.entries.len() - limit);
        }
    }
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Transcription> {
        self.entries.iter().map(|(_, entry)| entry)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_segments_remain_one_entry_across_episodes_and_other_history_entries() {
        let mut history = TranscriptionHistory::default();
        history.record(Some(7), Transcription::final_result("first".into()), 3);
        history.record(None, Transcription::final_result("legacy".into()), 3);
        history.record(
            Some(7),
            Transcription::final_result("late stable".into()),
            3,
        );
        assert_eq!(
            history
                .entries()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first late stable", "legacy"]
        );
        history.record(None, Transcription::final_result("another".into()), 2);
        assert_eq!(
            history
                .entries()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            vec!["legacy", "another"]
        );
    }
    #[test]
    fn unknown_timing_is_not_fabricated_and_empty_segments_create_no_history() {
        let mut history = TranscriptionHistory::default();
        history.record(
            Some(7),
            Transcription::final_result("first".into()).with_timing(1.0, 2.0),
            2,
        );
        history.record(Some(7), Transcription::final_result("next".into()), 2);
        history.record(Some(8), Transcription::final_result(String::new()), 2);
        let entry = history.entries().next().unwrap();
        assert!(!entry.timing_known);
        assert_eq!(entry.duration, 0.0);
        assert_eq!(history.entries().len(), 1);
        history.record(None, Transcription::final_result("hidden".into()), 0);
        assert_eq!(history.entries().len(), 0);
    }
}
