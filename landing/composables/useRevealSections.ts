export const useRevealSections = () => {
  if (!import.meta.client) return;

  let intersectionObserver: IntersectionObserver | null = null;
  let mutationObserver: MutationObserver | null = null;
  const observed = new WeakSet<Element>();

  const revealNow = (section: Element) => {
    section.classList.add("section-reveal", "section-reveal--visible");
  };

  const observeSection = (section: Element) => {
    if (observed.has(section) || section.id === "hero") return;
    observed.add(section);

    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches || !intersectionObserver) {
      revealNow(section);
      return;
    }

    section.classList.add("section-reveal");
    intersectionObserver.observe(section);
  };

  const collectSections = () => {
    document.querySelectorAll(".page .section").forEach(observeSection);
  };

  onMounted(() => {
    intersectionObserver = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          entry.target.classList.add("section-reveal--visible");
          intersectionObserver?.unobserve(entry.target);
        }
      },
      { threshold: 0.16, rootMargin: "0px 0px -12% 0px" },
    );

    collectSections();

    mutationObserver = new MutationObserver(() => collectSections());
    const page = document.querySelector(".page");
    if (page) {
      mutationObserver.observe(page, { childList: true, subtree: true });
    }
  });

  onUnmounted(() => {
    intersectionObserver?.disconnect();
    mutationObserver?.disconnect();
  });
};
