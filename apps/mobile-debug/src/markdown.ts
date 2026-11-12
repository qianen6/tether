// Codex answers in Markdown: bold, lists, tables, code, links to files on the computer.
// markdown-it parses it; this turns its tokens into blocks of styled runs that
// MarkdownView.tsx draws with React Native's Text. No React Native imports, so vitest runs it.
import MarkdownIt from "markdown-it";

type Token = ReturnType<MarkdownIt["parse"]>[number];

/** A line break, as typed: a chat answer's lines are lines, as on chatgpt.com. */
const md = new MarkdownIt({ html: false, linkify: true, breaks: true });

export type Run =
  | { t: "text"; text: string; strong?: boolean; em?: boolean; strike?: boolean; code?: boolean; href?: string }
  | { t: "br" };

export type Align = "left" | "center" | "right" | null;

export type Block =
  | { t: "p"; runs: Run[] }
  | { t: "h"; level: number; runs: Run[] }
  | { t: "code"; lang: string; text: string }
  | { t: "quote"; blocks: Block[] }
  | { t: "list"; ordered: boolean; start: number; items: Block[][] }
  | { t: "table"; align: Align[]; head: Run[][]; rows: Run[][][] }
  | { t: "hr" };

export function parse(src: string): Block[] {
  return blocks(md.parse(src, {}), { i: 0 });
}

/** The text alone, for a line that has no room for styles. */
export function plain(src: string): string {
  const out: string[] = [];
  const walk = (bs: Block[]) => {
    for (const b of bs) {
      if (b.t === "p" || b.t === "h") out.push(runsText(b.runs));
      else if (b.t === "code") out.push(b.text.replace(/\n$/, ""));
      else if (b.t === "quote") walk(b.blocks);
      else if (b.t === "list") b.items.forEach(walk);
      else if (b.t === "table") [b.head, ...b.rows].forEach(r => out.push(r.map(runsText).join(" | ")));
    }
  };
  walk(parse(src));
  return out.join("\n");
}

export function runsText(runs: Run[]): string {
  return runs.map(r => (r.t === "br" ? "\n" : r.text)).join("");
}

/** A link's target as written: markdown-it percent-encodes it (C:/%E7%88%B1…). */
export function linkTarget(href: string): string {
  try {
    return decodeURI(href);
  } catch {
    return href;
  }
}

function blocks(tokens: Token[], at: { i: number }, end?: string): Block[] {
  const out: Block[] = [];
  while (at.i < tokens.length) {
    const tok = tokens[at.i]!;
    if (end && tok.type === end) {
      at.i++;
      return out;
    }
    switch (tok.type) {
      case "paragraph_open":
      case "heading_open": {
        const inline = tokens[at.i + 1];
        const runs = inline?.type === "inline" ? inlines(inline.children ?? []) : [];
        out.push(tok.type === "heading_open" ? { t: "h", level: Number(tok.tag.slice(1)) || 1, runs } : { t: "p", runs });
        at.i += 3;
        break;
      }
      case "fence":
      case "code_block":
        out.push({ t: "code", lang: tok.info.trim(), text: tok.content });
        at.i++;
        break;
      case "hr":
        out.push({ t: "hr" });
        at.i++;
        break;
      case "blockquote_open":
        at.i++;
        out.push({ t: "quote", blocks: blocks(tokens, at, "blockquote_close") });
        break;
      case "bullet_list_open":
      case "ordered_list_open": {
        const ordered = tok.type === "ordered_list_open";
        const start = Number(tok.attrGet("start") ?? 1) || 1;
        const close = ordered ? "ordered_list_close" : "bullet_list_close";
        const items: Block[][] = [];
        at.i++;
        while (at.i < tokens.length && tokens[at.i]!.type !== close) {
          if (tokens[at.i]!.type === "list_item_open") {
            at.i++;
            items.push(blocks(tokens, at, "list_item_close"));
          } else at.i++;
        }
        at.i++;
        out.push({ t: "list", ordered, start, items });
        break;
      }
      case "table_open":
        out.push(table(tokens, at));
        break;
      default:
        at.i++;
    }
  }
  return out;
}

function table(tokens: Token[], at: { i: number }): Block {
  const align: Align[] = [];
  const head: Run[][] = [];
  const rows: Run[][][] = [];
  let inHead = false;
  let row: Run[][] | undefined;
  for (at.i++; at.i < tokens.length; at.i++) {
    const tok = tokens[at.i]!;
    if (tok.type === "table_close") {
      at.i++;
      break;
    }
    if (tok.type === "thead_open") inHead = true;
    else if (tok.type === "thead_close") inHead = false;
    else if (tok.type === "tr_open") row = [];
    else if (tok.type === "tr_close" && row) {
      if (!inHead) rows.push(row);
      row = undefined;
    } else if (tok.type === "th_open" || tok.type === "td_open") {
      const inline = tokens[at.i + 1];
      const cell = inline?.type === "inline" ? inlines(inline.children ?? []) : [];
      if (inHead) {
        head.push(cell);
        const style = tok.attrGet("style") ?? "";
        align.push(/center/.test(style) ? "center" : /right/.test(style) ? "right" : /left/.test(style) ? "left" : null);
      } else row?.push(cell);
    }
  }
  return { t: "table", align, head, rows };
}

function inlines(children: Token[]): Run[] {
  const runs: Run[] = [];
  let strong = 0;
  let em = 0;
  let strike = 0;
  const links: string[] = [];
  const text = (s: string, code = false) => {
    if (!s) return;
    const href = links[links.length - 1];
    runs.push({
      t: "text",
      text: s,
      ...(strong ? { strong: true } : {}),
      ...(em ? { em: true } : {}),
      ...(strike ? { strike: true } : {}),
      ...(code ? { code: true } : {}),
      ...(href ? { href } : {}),
    });
  };
  for (const tok of children) {
    switch (tok.type) {
      case "text":
        text(tok.content);
        break;
      case "code_inline":
        text(tok.content, true);
        break;
      case "softbreak":
      case "hardbreak":
        runs.push({ t: "br" });
        break;
      case "strong_open":
        strong++;
        break;
      case "strong_close":
        strong--;
        break;
      case "em_open":
        em++;
        break;
      case "em_close":
        em--;
        break;
      case "s_open":
        strike++;
        break;
      case "s_close":
        strike--;
        break;
      case "link_open":
        links.push(tok.attrGet("href") ?? "");
        break;
      case "link_close":
        links.pop();
        break;
      case "image":
        text(tok.content || "图片");
        break;
      default:
        text(tok.content);
    }
  }
  return runs;
}
