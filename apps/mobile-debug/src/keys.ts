// The terminal key row, with sticky Ctrl and Shift (Herdr pane-bar.tsx). The page encodes
// each key in the program's own cursor-key mode (scripts/terminal-boot.js).
export type Key = "esc" | "shift" | "tab" | "ctrl" | "ctrlc" | "left" | "up" | "down" | "right" | "enter";
export const ROW: { key: Key; label: string }[] = [
  { key: "esc", label: "esc" }, { key: "shift", label: "⇧" }, { key: "tab", label: "tab" }, { key: "ctrl", label: "ctrl" },
  { key: "ctrlc", label: "^C" }, { key: "left", label: "←" }, { key: "up", label: "↑" }, { key: "down", label: "↓" },
  { key: "right", label: "→" }, { key: "enter", label: "↵" },
];

/** What a key typed in the terminal sends with the row's Ctrl or Shift held: Shift makes
 *  a letter a capital; Ctrl makes a letter or one of @[\]^_ (and space) its control character. */
export function withMods(typed: string, mods: { ctrl: boolean; shift: boolean }): string {
  if (typed.length !== 1) return typed;
  const d = mods.shift ? typed.toUpperCase() : typed;
  if (!mods.ctrl) return d;
  if (d === " ") return "\x00";
  const c = d.toUpperCase().charCodeAt(0);
  return c >= 0x40 && c <= 0x5f ? String.fromCharCode(c & 0x1f) : d;
}
