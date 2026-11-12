// End to end through a real TailPocket tunnel (needs network; never in `pnpm test`):
//
//   pnpm --filter @tether/client test:live
//
// Builds and starts `tether serve` with a temporary data directory and the echo
// agent, pairs `tailpocket-phone` with `tether pair --json`, then drives the
// gateway with TetherClient over the phone's loopback forward.
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as pause } from "node:timers/promises";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { WebSocket } from "ws";
import { TetherClient, type ClientState, type WebSocketLike } from "../src/index";
import type { Approval, Event } from "../src/generated";

const root = resolve(import.meta.dirname, "../../..");
const dir = mkdtempSync(join(tmpdir(), "tether-live-"));
const dataDir = join(dir, "host");
const children: ChildProcess[] = [];

function phoneBinary(): string {
  if (process.env.TAILPOCKET_PHONE) return process.env.TAILPOCKET_PHONE;
  const os = process.platform === "win32" ? "windows" : process.platform === "darwin" ? "darwin" : "linux";
  const arch = process.arch === "arm64" ? "arm64" : "amd64";
  const path = join(root, "vendor/tailpocket/bin", `tailpocket-phone-${os}-${arch}${os === "windows" ? ".exe" : ""}`);
  if (!existsSync(path)) throw new Error(`missing ${path} (run scripts/fetch-vendor.sh)`);
  return path;
}

const tetherBin = join(root, "target/debug", process.platform === "win32" ? "tether.exe" : "tether");

function tether(...args: string[]): string {
  const r = spawnSync(tetherBin, ["--data-dir", dataDir, ...args], {
    cwd: root, encoding: "utf8", windowsHide: true,
  });
  if (r.status !== 0) throw new Error(`tether ${args.join(" ")} failed: ${r.stderr}`);
  return r.stdout;
}

async function until<T>(what: string, ms: number, f: () => T | undefined | Promise<T | undefined>): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    try {
      const v = await f();
      if (v !== undefined) return v;
    } catch (e) {
      if (Date.now() > end) throw e;
    }
    if (Date.now() > end) throw new Error(`timed out: ${what}`);
    await pause(200);
  }
}

let client: TetherClient;
let forward = "";
let payload = "";

beforeAll(async () => {
  // Build once so `serve` starts quickly.
  const build = spawnSync("cargo", ["build", "-q", "-p", "tether"], { cwd: root, stdio: "inherit", windowsHide: true });
  if (build.status !== 0) throw new Error("cargo build -p tether failed");
  const serve = spawn(tetherBin, ["--data-dir", dataDir, "serve", "--name", "Tether live"], {
    cwd: root, env: { ...process.env, TETHER_ENABLE_ECHO: "1" }, stdio: ["ignore", "inherit", "inherit"], windowsHide: true,
  });
  children.push(serve);
  await until("tether serve", 60_000, () => (existsSync(join(dataDir, "control.json")) ? true : undefined));

  const { qr } = JSON.parse(tether("pair", "--json", "--minutes", "5")) as { qr: string };
  const link = join(dir, "phone-link.json");
  const pair = spawnSync(phoneBinary(), ["pair", "-device", "Tether live phone", "-prefix", "tether1:", "-key", join(dir, "phone-key"), "-out", link, "-"], {
    input: qr, encoding: "utf8", timeout: 150_000, windowsHide: true,
  });
  if (pair.status !== 0) throw new Error(`tailpocket-phone pair failed: ${pair.stderr}`);
  payload = (JSON.parse(readFileSync(link, "utf8")) as { payload: string }).payload;

  const connect = spawn(phoneBinary(), ["connect", "-link", link], { stdio: ["ignore", "pipe", "inherit"], windowsHide: true });
  children.push(connect);
  const lines = createInterface({ input: connect.stdout! });
  forward = await new Promise<string>((ok, fail) => {
    const timer = setTimeout(() => fail(new Error("phone did not announce its port")), 20_000);
    lines.once("line", line => { clearTimeout(timer); ok(line.trim().replace(/^listening /, "")); });
  });

  const values = new Map<string, string>();
  client = new TetherClient({
    url: `ws://${forward}/v1/ws`,
    deviceName: "Tether live",
    pairSecret: payload,
    storage: { get: async k => values.get(k) ?? null, set: async (k, v) => { values.set(k, v); } },
    transport: { connect: url => new WebSocket(url) as unknown as WebSocketLike },
  });
}, 600_000);

afterAll(() => {
  client?.close();
  for (const c of children) c.kill();
  rmSync(dir, { recursive: true, force: true });
});

// Tests in one file run in order.
describe("through the tunnel", () => {
  it("pairs and lists agents", async () => {
    const welcome = await client.connect();
    expect(welcome.host_name).toBe("Tether live");
    const agents = await client.listAgents();
    expect(agents.some(a => a.id === "echo")).toBe(true);
    expect(agents.some(a => a.id === "codex")).toBe(true);
  }, 120_000);

  it("chats with an approval", async () => {
    const session = await client.createSession("echo");
    const events: Event[] = [];
    const stop = await client.subscribe(session.id, e => events.push(e));
    await client.send(session.id, "approve:ls");
    const approval = await until("approval", 60_000, () =>
      events.find((e): e is Event & { e: "approval"; approval: Approval } => e.e === "approval"));
    await client.answer(session.id, approval.approval.id, "allow_once");
    await until("turn_done", 60_000, () => events.find(e => e.e === "turn_done"));
    stop();
    await client.closeSession(session.id);
  }, 120_000);

  it("runs a shell command in a terminal", async () => {
    const term = await client.openTerminal({ agent: "shell", cols: 100, rows: 30 });
    let out = "";
    const decoder = new TextDecoder();
    term.onData(b => { out += decoder.decode(b, { stream: true }); });
    term.write(process.platform === "win32" ? "echo tether-live-$(40+2)\r" : "echo tether-live-$((40+2))\r");
    await until("echo output", 60_000, () => (out.includes("tether-live-42") ? true : undefined));
    await term.kill();
  }, 120_000);

  it("is cut off when revoked", async () => {
    const states: ClientState[] = [];
    client.on("state", s => states.push(s as ClientState));
    const devices = JSON.parse(tether("devices", "--json")) as { id: string }[];
    expect(devices.length).toBe(1);
    tether("revoke", devices[0]!.id);
    await until("closed", 30_000, () => (client.state === "closed" || states.includes("reconnecting") ? true : undefined));
    // Whatever the client does now, it cannot get back in.
    await expect(client.listSessions()).rejects.toThrow();
  }, 120_000);
});
