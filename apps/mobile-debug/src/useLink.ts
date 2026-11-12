import { useSyncExternalStore } from "react";
import { link } from "./link";

export function useLink() {
  return useSyncExternalStore(link.subscribe, () => link.snapshot);
}
