import { describe, expect, it } from 'vitest';
import { appendTranscriptText, mergeTranscriptText } from './transcriptionText';

describe('transcription text helpers', () => {
  it('appends finalized chunks without removing boundary repeats', () => {
    expect(appendTranscriptText('two two', 'two two three')).toBe('two two two two three');
  });

  it('merges overlapping live interim text for display only', () => {
    expect(mergeTranscriptText('Ты слышишь, что', 'Ты слышишь, что я говорю?')).toBe(
      'Ты слышишь, что я говорю?'
    );
  });
});
