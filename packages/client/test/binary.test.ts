import { describe, expect, it } from "vitest";
import { decodeBinary, encodeBinary, TERM_INPUT, TERM_OUTPUT } from "../src/binary";
import { PROTOCOL_VERSION } from "../src/generated";

describe("binary frames", () => {
  it("round-trips and matches the Rust layout", () => {
    const f = encodeBinary(TERM_OUTPUT, "t1", new TextEncoder().encode("hello"));
    expect(Array.from(f.subarray(0, 4))).toEqual([0x01, 2, 0x74, 0x31]);
    const d = decodeBinary(f);
    expect(d.kind).toBe(TERM_OUTPUT);
    expect(d.term).toBe("t1");
    expect(new TextDecoder().decode(d.data)).toBe("hello");
  });

  it("handles empty data and long ids", () => {
    expect(decodeBinary(encodeBinary(TERM_INPUT, "t", new Uint8Array())).data.length).toBe(0);
    const id = "x".repeat(255);
    expect(decodeBinary(encodeBinary(TERM_OUTPUT, id, new Uint8Array([1]))).term).toBe(id);
    expect(() => encodeBinary(TERM_OUTPUT, "x".repeat(256), new Uint8Array())).toThrow();
  });

  it("rejects malformed frames", () => {
    for (const bad of [[], [1], [1, 0], [1, 5, 97], [7, 1, 97], [1, 1, 0xff]]) {
      expect(() => decodeBinary(new Uint8Array(bad))).toThrow();
    }
  });

  it("sees the generated protocol types", () => {
    expect(PROTOCOL_VERSION).toBe(1);
  });
});
