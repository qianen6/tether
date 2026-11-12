import { describe, expect, it } from "vitest";
import { linkTarget, parse, plain, runsText } from "../src/markdown";

describe("markdown", () => {
  it("reads bold, code and line breaks in a paragraph", () => {
    const [p] = parse("**A2 最适合日常头像**：深蓝底\n第二行 `npm test`");
    expect(p).toEqual({
      t: "p",
      runs: [
        { t: "text", text: "A2 最适合日常头像", strong: true },
        { t: "text", text: "：深蓝底" },
        { t: "br" },
        { t: "text", text: "第二行 " },
        { t: "text", text: "npm test", code: true },
      ],
    });
  });

  it("keeps a link to a file on the computer, with spaces-free angle brackets", () => {
    const [p] = parse("[实际生成提示词与核验](<C:/爱极客AIGREEK/公众号/00_本轮提示词与核验.txt>) · [网站](https://example.com)");
    if (p?.t !== "p") throw new Error("not a paragraph");
    const links = p.runs.filter(r => r.t === "text" && r.href);
    expect(links.map(r => r.t === "text" && [r.text, linkTarget(r.href!)])).toEqual([
      ["实际生成提示词与核验", "C:/爱极客AIGREEK/公众号/00_本轮提示词与核验.txt"],
      ["网站", "https://example.com"],
    ]);
  });

  it("reads a table, its header and alignment", () => {
    const [table] = parse("| 名字 | 图 |\n|:--|--:|\n| A4 | [印章](<C:/a/A4.png>) |\n| A5 | 徽章 |");
    if (table?.t !== "table") throw new Error("not a table");
    expect(table.align).toEqual(["left", "right"]);
    expect(table.head.map(runsText)).toEqual(["名字", "图"]);
    expect(table.rows.map(r => r.map(runsText))).toEqual([["A4", "印章"], ["A5", "徽章"]]);
  });

  it("reads lists, headings, quotes and code", () => {
    const blocks = parse("## 计划\n\n1. 先看\n2. 再改\n\n- 一\n  - 二\n\n> 注意\n\n```ts\nconst a = 1;\n```");
    expect(blocks.map(b => b.t)).toEqual(["h", "list", "list", "quote", "code"]);
    const [, ordered, bullets] = blocks;
    expect(ordered).toMatchObject({ ordered: true, start: 1 });
    if (bullets?.t !== "list") throw new Error("not a list");
    expect(bullets.items[0]!.map(b => b.t)).toEqual(["p", "list"]);
    expect(blocks[4]).toEqual({ t: "code", lang: "ts", text: "const a = 1;\n" });
  });

  it("gives the text alone", () => {
    expect(plain("**正在核对可见的模型信息**\n\n我先看看")).toBe("正在核对可见的模型信息\n我先看看");
  });

  it("leaves an unfinished bold as typed while it streams", () => {
    expect(plain("**还没写完")).toBe("**还没写完");
  });
});
