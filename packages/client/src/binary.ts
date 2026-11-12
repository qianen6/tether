// Binary frames carry terminal bytes: [kind: u8][id_len: u8][id][data…]
// (docs/PROTOCOL.md §2). Mirrors `tether_proto::binary`.

export const TERM_OUTPUT = 0x01;
export const TERM_INPUT = 0x02;

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

export function encodeBinary(kind: number, term: string, data: Uint8Array): Uint8Array {
  const id = encoder.encode(term);
  if (id.length === 0 || id.length > 255) throw new Error("terminal id must be 1-255 bytes");
  const out = new Uint8Array(2 + id.length + data.length);
  out[0] = kind;
  out[1] = id.length;
  out.set(id, 2);
  out.set(data, 2 + id.length);
  return out;
}

export function decodeBinary(frame: Uint8Array): { kind: number; term: string; data: Uint8Array } {
  if (frame.length < 2) throw new Error("binary frame too short");
  const kind = frame[0]!;
  if (kind !== TERM_OUTPUT && kind !== TERM_INPUT) throw new Error(`unknown binary frame kind ${kind}`);
  const n = frame[1]!;
  if (n === 0) throw new Error("empty terminal id");
  if (frame.length < 2 + n) throw new Error("binary frame shorter than its terminal id");
  const term = decoder.decode(frame.subarray(2, 2 + n));
  return { kind, term, data: frame.subarray(2 + n) };
}
