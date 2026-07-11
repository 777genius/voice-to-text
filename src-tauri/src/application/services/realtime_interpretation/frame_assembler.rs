pub(super) struct Pcm16FrameAssembler {
    frame_samples: usize,
    pending: Vec<i16>,
}

impl Pcm16FrameAssembler {
    pub(super) fn new(frame_samples: usize) -> Self {
        assert!(frame_samples > 0, "frame size must be positive");
        Self {
            frame_samples,
            pending: Vec::with_capacity(frame_samples),
        }
    }

    pub(super) fn push(&mut self, mut samples: &[i16]) -> Vec<Vec<i16>> {
        let mut frames = Vec::new();
        while !samples.is_empty() {
            let needed = self.frame_samples - self.pending.len();
            let consumed = needed.min(samples.len());
            self.pending.extend_from_slice(&samples[..consumed]);
            samples = &samples[consumed..];

            if self.pending.len() == self.frame_samples {
                frames.push(std::mem::replace(
                    &mut self.pending,
                    Vec::with_capacity(self.frame_samples),
                ));
            }
        }
        frames
    }

    pub(super) fn finish_padded(&mut self) -> Option<Vec<i16>> {
        if self.pending.is_empty() {
            return None;
        }

        let mut frame =
            std::mem::replace(&mut self.pending, Vec::with_capacity(self.frame_samples));
        frame.resize(self.frame_samples, 0);
        Some(frame)
    }

    #[cfg(test)]
    pub(super) fn pending_samples(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_chunks_form_exact_frames_with_bounded_remainder() {
        let mut assembler = Pcm16FrameAssembler::new(4);

        assert!(assembler.push(&[1, 2]).is_empty());
        let frames = assembler.push(&[3, 4, 5, 6, 7, 8, 9]);

        assert_eq!(frames, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
        assert_eq!(assembler.pending_samples(), 1);
    }

    #[test]
    fn final_partial_frame_is_zero_padded_once() {
        let mut assembler = Pcm16FrameAssembler::new(4);
        assembler.push(&[7, 8]);

        assert_eq!(assembler.finish_padded(), Some(vec![7, 8, 0, 0]));
        assert_eq!(assembler.finish_padded(), None);
    }
}
