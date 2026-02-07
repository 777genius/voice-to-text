type DownloadEventParams = {
  os: string;
  arch: string;
  version?: string | null;
  source: string;
};

export const useAnalytics = () => {
  const { gtag } = useGtag();

  const trackNavClick = (target: string) => {
    gtag("event", "nav_click", {
      event_category: "navigation",
      event_label: target,
    });
  };

  const trackLanguageSwitch = (from: string, to: string) => {
    gtag("event", "language_switch", {
      event_category: "settings",
      event_label: to,
      from,
      to,
    });
  };

  const trackThemeToggle = (theme: "light" | "dark") => {
    gtag("event", "theme_toggle", {
      event_category: "settings",
      event_label: theme,
    });
  };

  const trackDownloadClick = ({ os, arch, version, source }: DownloadEventParams) => {
    gtag("event", "download_click", {
      event_category: "download",
      event_label: `${os}_${arch}`,
      os,
      arch,
      version: version ?? undefined,
      source,
    });
  };

  const trackSectionView = (sectionId: string) => {
    gtag("event", "section_view", {
      event_category: "engagement",
      event_label: sectionId,
    });
  };

  const trackFaqExpand = (faqId: string, question: string) => {
    gtag("event", "faq_expand", {
      event_category: "engagement",
      event_label: faqId,
      question,
    });
  };

  return {
    trackNavClick,
    trackLanguageSwitch,
    trackThemeToggle,
    trackDownloadClick,
    trackSectionView,
    trackFaqExpand,
  };
};
