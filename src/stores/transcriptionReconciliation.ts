export interface PartialAnimationTransition {
  renderedText: string;
  textToAnimate: string;
}

/**
 * Prefix growth can keep the typing animation. Corrections replace the mutable
 * hypothesis atomically so the UI never renders an empty intermediate frame.
 */
export function reconcilePartialAnimation(
  renderedText: string,
  targetText: string
): PartialAnimationTransition {
  if (targetText === renderedText) {
    return { renderedText, textToAnimate: '' };
  }

  if (targetText.startsWith(renderedText)) {
    return {
      renderedText,
      textToAnimate: targetText.slice(renderedText.length),
    };
  }

  let stablePrefixLength = 0;
  const maxPrefixLength = Math.min(renderedText.length, targetText.length);
  while (
    stablePrefixLength < maxPrefixLength &&
    renderedText.charAt(stablePrefixLength) === targetText.charAt(stablePrefixLength)
  ) {
    stablePrefixLength++;
  }

  return {
    renderedText:
      renderedText.slice(0, stablePrefixLength) + targetText.slice(stablePrefixLength),
    textToAnimate: '',
  };
}
