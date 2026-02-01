import darkMain from "~/assets/images/screenshots/dark.svg";
import lightMain from "~/assets/images/screenshots/light.svg";
import settings from "~/assets/images/screenshots/settings.svg";
import recording from "~/assets/images/screenshots/recording.svg";

export interface Screenshot {
  id: string;
  labelKey: string;
  lightSrc: string;
  darkSrc: string;
  width: number;
  height: number;
}

export const screenshots: Screenshot[] = [
  {
    id: "main",
    labelKey: "screenshots.main",
    lightSrc: lightMain,
    darkSrc: darkMain,
    width: 1200,
    height: 750
  },
  {
    id: "settings",
    labelKey: "screenshots.settings",
    lightSrc: settings,
    darkSrc: settings,
    width: 1200,
    height: 750
  },
  {
    id: "recording",
    labelKey: "screenshots.recording",
    lightSrc: recording,
    darkSrc: recording,
    width: 1200,
    height: 750
  }
];
