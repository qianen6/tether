// What the shell knows that screens deep inside need: the computer's agents, and where the
// main pane sits (beside the sidebar on a tablet), for menus that float over it.
import { createContext, useContext } from "react";
import type { AgentInfo } from "@tether/client";

export const AgentsContext = createContext<AgentInfo[]>([]);

export function useAgentInfos(): AgentInfo[] {
  return useContext(AgentsContext);
}

export interface Pane {
  /** Distance from the screen's left edge. */
  left: number;
  width: number;
  /** A tablet-wide screen: the sidebar stays beside the content. */
  wide: boolean;
}

export const PaneContext = createContext<Pane>({ left: 0, width: 0, wide: false });

export function usePane(): Pane {
  return useContext(PaneContext);
}

/** How wide a conversation's column gets on a big screen. */
export const COLUMN = 820;

/** The left edge of a centred column of `width` in the pane, plus `inset`. */
export function columnLeft(pane: Pane, width: number, inset: number): number {
  return pane.left + Math.max(0, (pane.width - width) / 2) + inset;
}
