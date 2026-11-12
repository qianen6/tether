// The phone's look, after Remodex (github.com/Emanuele-web04/remodex, CodexMobile): iOS
// system neutrals on white (black at night), black call-to-action, soft grey fills for
// the user's messages and chips. Token names stay shadcn's (as Herdr Anywhere's were):
// sidebar = drawer, sidebarAccent = pressed, sidebarForeground = dim text, accent =
// selected, secondary/muted = raised, fill = user bubble and chips; chart1..4 = working,
// needs you, done, idle; ok = connected.
import { useColorScheme } from "react-native";

const light = {
  background: "#ffffff",
  foreground: "#111113",
  popover: "#ffffff",
  primary: "#111113",
  primaryForeground: "#ffffff",
  secondary: "#f2f2f7",
  muted: "#f2f2f7",
  mutedForeground: "#6e6e73",
  accent: "#ececf0",
  destructive: "#d93a2b",
  border: "#e5e5ea",
  input: "#d8d8dd",
  ring: "#1f62d0",
  sidebar: "#ffffff",
  sidebarForeground: "#9c9ca1",
  sidebarAccent: "#f2f2f7",
  sidebarPrimary: "#fafafa",
  fill: "#efeff2",
  ok: "#34c759",
  chart1: "#a36a00",
  chart2: "#d93a2b",
  chart3: "#0a84ff",
  chart4: "#aeaeb2",
  chart5: "#1f62d0",
  terminal: {
    background: "#fafaf9", foreground: "#1b1b1e", cursor: "#1b1b1e", selection: "#d9d9d9",
    // Herdr's PAPER palette (herdr-app settings.rs): ANSI colours that read on white.
    ansi: ["#25272c", "#c62a2a", "#1a7f37", "#8f5d00", "#1f62d0", "#7a45c8", "#0f6f74", "#5f6068",
      "#7b7e87", "#d1242f", "#1f883d", "#9a6700", "#0969da", "#8250df", "#1b7c83", "#111214"],
  },
};

export type Theme = typeof light;

const dark: Theme = {
  background: "#000000",
  foreground: "#f2f2f7",
  popover: "#1c1c1e",
  primary: "#f2f2f7",
  primaryForeground: "#000000",
  secondary: "#1c1c1e",
  muted: "#1c1c1e",
  mutedForeground: "#98989f",
  accent: "#2c2c2e",
  destructive: "#ff6b5e",
  border: "#2c2c2e",
  input: "#3a3a3c",
  ring: "#96b2d7",
  sidebar: "#000000",
  sidebarForeground: "#6e6e73",
  sidebarAccent: "#1c1c1e",
  sidebarPrimary: "#0d0d0e",
  fill: "#232326",
  ok: "#30d158",
  chart1: "#e5c07b",
  chart2: "#ff6b5e",
  chart3: "#4da3ff",
  chart4: "#636366",
  chart5: "#96b2d7",
  terminal: {
    background: "#131518", foreground: "#eeeeef", cursor: "#eeeeef", selection: "#3a3c40",
    // Herdr's GRAPHITE palette.
    ansi: ["#45484f", "#dc8c95", "#a6bd91", "#d8be87", "#96b2d7", "#b5a2ce", "#8cbdc3", "#d2d4d9",
      "#818793", "#f0a5ad", "#bad5a2", "#ead2a1", "#b0cbef", "#cbb7e6", "#a4d5db", "#f0f0f2"],
  },
};

export const RADIUS = 10;
/** Bundled by the expo-font plugin (app.json); the family is the file name. */
export const FONT = "Geist";
export const MONO = "JetBrainsMono-Variable";

export function useTheme(): Theme {
  return useColorScheme() === "dark" ? dark : light;
}

/** `#rrggbb` at `alpha` (0–1), for tinted backgrounds such as chart2/15. */
export function alpha(hex: string, a: number): string {
  return `${hex}${Math.round(a * 255).toString(16).padStart(2, "0")}`;
}
