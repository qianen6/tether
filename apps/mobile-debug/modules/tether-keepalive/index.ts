// Android foreground service that keeps the tunnel open while the app is in
// the background. A no-op where the native module is missing (iOS, tests).
import { requireOptionalNativeModule } from "expo";

interface Native {
  start(title: string, text: string): boolean;
  stop(): boolean;
}

const native = requireOptionalNativeModule<Native>("TetherKeepalive");

export function startKeepalive(title: string, text: string): void {
  try { native?.start(title, text); } catch { /* not allowed right now (app in background) */ }
}

export function stopKeepalive(): void {
  try { native?.stop(); } catch { /* already stopped */ }
}
