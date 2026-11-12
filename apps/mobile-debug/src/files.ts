// The history copy as files under the app's documents (kept by the system,
// gone with the app). Keys become paths: `thread/codex/<id>` → `.../thread/codex/<id>.json`.
import { Directory, File, Paths } from "expo-file-system";
import { HistoryCache, memoryKV, type KV } from "./history";

const root = () => new Directory(Paths.document, "history");

/** A key as path parts, each safe as a file name. */
function parts(key: string): string[] {
  return key.split("/").map(p => p.replace(/[^A-Za-z0-9._-]/g, c => `%${c.charCodeAt(0).toString(16)}`));
}

function file(key: string): File {
  const p = parts(key);
  const name = `${p.pop()}.json`;
  return new File(new Directory(root(), ...p), name);
}

const filesKV: KV = {
  async get(key) {
    const f = file(key);
    return f.exists ? f.text() : null;
  },
  async set(key, value) {
    const f = file(key);
    if (!f.parentDirectory.exists) f.parentDirectory.create({ intermediates: true, idempotent: true });
    if (!f.exists) f.create();
    await f.write(value);
  },
  async remove(key) {
    const f = file(key);
    if (f.exists) f.delete();
  },
};

function pick(): KV {
  try {
    root();
    return filesKV;
  } catch {
    // No file system (a web preview): this run only.
    return memoryKV();
  }
}

export const history = new HistoryCache(pick());
