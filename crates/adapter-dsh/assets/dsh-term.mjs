// Tether's DSH terminal: a line-mode client for a `dsh web` (DSH 0.1.x
// gateway), for the phone's terminal when DSH has no `tui` profile.
// Runs on the Node.js that runs DSH (>= 22: global fetch and WebSocket).
//
//   node dsh-term.mjs --dsh <dsh command> [--resume <session id>]
//
// Finds a running `dsh web` the same way Tether does (TETHER_DSH_URL, the
// address Tether saved, http://127.0.0.1:3080), else starts one and stops it
// on exit.
import { spawn } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'
import { randomUUID } from 'node:crypto'

const argv = process.argv.slice(2)
const flag = (name) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : undefined }
const dshCommand = flag('--dsh') ?? 'dsh'
let resumeId = flag('--resume')

const color = process.env.NO_COLOR ? { dim: (s) => s, bold: (s) => s, cyan: (s) => s, red: (s) => s, green: (s) => s, yellow: (s) => s }
  : { dim: (s) => `\x1b[2m${s}\x1b[0m`, bold: (s) => `\x1b[1m${s}\x1b[0m`, cyan: (s) => `\x1b[36m${s}\x1b[0m`, red: (s) => `\x1b[31m${s}\x1b[0m`, green: (s) => `\x1b[32m${s}\x1b[0m`, yellow: (s) => `\x1b[33m${s}\x1b[0m` }
const out = (s) => process.stdout.write(s)
const say = (s = '') => out(`${s}\n`)

// ---------------------------------------------------------------- dsh web
const STATE = join(tmpdir(), 'tether-dsh-web.json')
let origin = null
let ownChild = null

async function rpc(method, payload, timeoutMs = 30000) {
  const body = { type: 'client-request', rpcId: randomUUID(), method, payload }
  const res = await fetch(`${origin}/api/${method}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body), signal: AbortSignal.timeout(timeoutMs) })
  if (!res.ok) throw new Error(`${method}: HTTP ${res.status} ${await res.text()}`)
  const env = await res.json()
  if (!env.result?.ok) throw Object.assign(new Error(env.result?.error?.message ?? 'error'), { code: env.result?.error?.code })
  return env.result.value
}

async function respond(message) {
  const res = await fetch(`${origin}/api/respond`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(message) })
  return (await res.json()).accepted === true
}

async function alive(candidate) {
  try {
    origin = candidate
    await rpc('host.describe', {}, 2000)
    return true
  } catch { origin = null; return false }
}

async function findOrStart() {
  const candidates = []
  if (process.env.TETHER_DSH_URL) candidates.push(process.env.TETHER_DSH_URL.replace(/\/$/, ''))
  try { candidates.push(JSON.parse(readFileSync(STATE, 'utf8')).origin) } catch {}
  candidates.push('http://127.0.0.1:3080')
  for (const c of candidates) if (c && await alive(c)) return
  say(color.dim('正在启动 dsh web…'))
  const windows = process.platform === 'win32'
  const child = spawn(windows ? `"${dshCommand}"` : dshCommand, ['web', '--no-open', '--host', '127.0.0.1', '--port', '0'], { shell: windows, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true, cwd: process.env.USERPROFILE ?? process.env.HOME })
  ownChild = child
  let tail = ''
  child.stderr.on('data', (d) => { tail = (tail + d).slice(-2000) })
  origin = await new Promise((resolve, reject) => {
    let buf = ''
    const timer = setTimeout(() => reject(new Error(`dsh web 没有打印地址\n${tail}`)), 120000)
    child.stdout.on('data', (d) => {
      buf += d
      const m = /dsh web: (http:\/\/[^\s/?#]+)/.exec(buf)
      if (m) { clearTimeout(timer); resolve(m[1]) }
    })
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`dsh web 退出了（${code}）\n${tail}`)) })
  })
  child.stdout.resume()
  try { writeFileSync(STATE, JSON.stringify({ origin })) } catch {}
}

function stopOwn() {
  if (!ownChild || ownChild.exitCode !== null) return
  if (process.platform === 'win32') spawn('taskkill', ['/pid', String(ownChild.pid), '/t', '/f'], { windowsHide: true })
  else ownChild.kill('SIGTERM')
}

// ---------------------------------------------------------------- session
let sessionId = null
let running = false
let approval = null      // { rpcId, approvalId }
let question = null      // { rpcId, questions }
let midLine = false      // assistant text printed without a final newline
const tools = new Map()  // callId → { name, args }
const toolsShown = new Set()

const rl = createInterface({ input: process.stdin, output: process.stdout, terminal: Boolean(process.stdin.isTTY) })
const prompt = () => { if (!running || approval || question) { rl.setPrompt(approval ? color.yellow('允许？(y/N) ') : question ? color.yellow('回答› ') : color.cyan('› ')); rl.prompt(true) } }
const lineBreak = () => { if (midLine) { out('\n'); midLine = false } }
const parseArgs = (raw) => { try { return typeof raw === 'string' ? JSON.parse(raw) : raw ?? {} } catch { return {} } }
const textOf = (content) => (Array.isArray(content) ? content : []).filter((c) => c?.type === 'text').map((c) => c.text ?? '').join('')
const clip = (s, lines = 12) => { const all = s.replace(/\s+$/, '').split(/\r?\n/); return all.length > lines ? [...all.slice(0, lines), color.dim(`…（还有 ${all.length - lines} 行）`)].join('\n') : all.join('\n') }

function onEvent(ev, view) {
  const d = ev.data ?? {}
  switch (ev.type) {
    case 'turn/start': running = true; break
    case 'assistant/chunk': {
      const c = d.chunk ?? {}
      if (c.type === 'text-delta' && c.text) { out(c.text); midLine = !c.text.endsWith('\n') }
      if (c.type === 'block-end' && c.block?.type === 'text') lineBreak()
      break
    }
    case 'tool/call': {
      const args = parseArgs(d.arguments)
      tools.set(d.callId, { name: d.name, args })
      if (d.name === 'ask_user_question') break
      lineBreak()
      const what = view?.view?.title ?? args.command ?? args.file_path ?? args.path ?? args.pattern ?? args.query ?? args.url ?? ''
      say(`${color.green('⏺')} ${color.bold(d.name)} ${what}`)
      toolsShown.add(d.callId)
      break
    }
    case 'tool/result': {
      const result = (d.message?.content ?? []).find((p) => p.type === 'tool-result') ?? {}
      const id = result.toolCallId ?? d.message?.source?.callId
      if (!toolsShown.delete(id)) break
      const output = view?.view?.card === 'terminal' ? view.view.output ?? '' : textOf(result.content)
      const text = clip(output)
      if (text) say((result.isError ? color.red : color.dim)(text.split('\n').map((l) => `  ${l}`).join('\n')))
      break
    }
    case 'llm/retry': lineBreak(); say(color.dim(`模型暂时不可用，重试 ${d.retry ?? ''}/${d.maxRetries ?? ''}…`)); break
    case 'compaction/start': lineBreak(); say(color.dim('正在压缩上下文…')); break
    case 'turn/end': {
      lineBreak()
      running = false; approval = null; question = null
      if (d.reason?.kind === 'aborted') say(color.dim('（已停止）'))
      if (d.reason?.kind === 'error') say(color.red(`出错：${d.reason.error?.message ?? d.reason.message ?? ''}`))
      prompt()
      break
    }
  }
}

function onMux(rpcId, p) {
  if (p.sessionId !== sessionId) return
  if (p.type === 'session/event') return onEvent(p.event, p.view)
  if (p.type === 'approval/requested') {
    if (approval?.approvalId === p.approvalId) return
    lineBreak()
    const call = tools.get(p.callId)
    say(color.yellow(`需要你批准：${p.toolName}${call?.args?.command ? ` → ${call.args.command}` : ''}`))
    if (p.reason) say(color.dim(p.reason))
    approval = { rpcId, approvalId: p.approvalId }
    return prompt()
  }
  if (p.type === 'approval/resolved') { if (approval?.approvalId === p.approvalId) { approval = null; say(color.dim(`（${p.outcome === 'allowed-once' ? '已允许' : '已拒绝'}）`)) } return }
  if (p.type === 'question/requested') {
    if (question?.rpcId === rpcId) return
    lineBreak()
    for (const [i, q] of p.questions.entries()) {
      say(color.yellow(`${p.questions.length > 1 ? `${i + 1}) ` : ''}${q.header ? `${q.header}：` : ''}${q.question}`))
      if (q.detail) say(q.detail)
      for (const [n, o] of (q.options ?? []).entries()) say(`  ${n + 1}. ${o.label}${o.description ? color.dim(` — ${o.description}`) : ''}`)
    }
    say(color.dim(p.questions.length > 1 ? '每个问题回答一次（序号或自己的答案），用 ; 分开' : '回复序号或自己的答案'))
    question = { rpcId, questions: p.questions }
    return prompt()
  }
  if (p.type === 'question/resolved') { question = null }
}

function answersFor(questions, reply) {
  const parts = questions.length > 1 ? reply.split(/[;；]/) : [reply]
  return questions.map((q, i) => {
    const line = (parts[i] ?? '').trim()
    const labels = (q.options ?? []).map((o) => o.label)
    if (!line) return { id: q.id, selected: [] }
    const tokens = q.multiSelect ? line.split(/[,，、\s]+/).filter(Boolean) : [line]
    const picked = tokens.map((t) => /^\d+$/.test(t) ? labels[Number(t) - 1] : labels.find((l) => l.toLowerCase() === t.toLowerCase()))
    return picked.every(Boolean) ? { id: q.id, selected: picked } : { id: q.id, selected: [], custom: line }
  })
}

async function openSocket(stream, onFrame) {
  const ws = new WebSocket(`${origin.replace(/^http/, 'ws')}/api/${stream}`)
  ws.onmessage = (e) => { try { const f = JSON.parse(e.data); onFrame(f.rpcId, f.payload ?? {}) } catch {} }
  ws.onclose = () => { lineBreak(); say(color.red('与 DSH 的连接断开了')); quit(1) }
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error(`连接 ${stream} 失败`)) })
  return ws
}

async function openSession(id) {
  if (id) {
    const page = await rpc('session.history', { sessionId: id, maxMessages: 6 })
    sessionId = id
    say(color.dim(`已接上 DSH 会话 ${id}`))
    for (const { event } of page.events ?? []) {
      if (event.type === 'user/message' && event.data?.source?.kind === 'user') say(color.cyan(`› ${textOf(event.data.content)}`))
      if (event.type === 'assistant/message') { const t = textOf(event.data?.message?.content); if (t.trim()) say(t) }
    }
  } else {
    const created = await rpc('session.create', { cwd: process.cwd() })
    sessionId = created.sessionId
    say(color.dim(`新的 DSH 会话 ${sessionId}（${process.cwd()}）`))
  }
}

async function listSessions() {
  const { items } = await rpc('session.list', {})
  const rows = items.filter((s) => s.origin !== 'subagent').sort((a, b) => b.updatedAt - a.updatedAt).slice(0, 15)
  for (const s of rows) say(`${color.dim(new Date(s.updatedAt).toLocaleString())}  ${s.projections?.values?.title ?? '(无标题)'}  ${color.dim(s.sessionId)}`)
  say(color.dim('/resume <会话 id> 接着聊'))
}

let quitting = false
function quit(code = 0) {
  if (quitting) return
  quitting = true
  stopOwn()
  setTimeout(() => process.exit(code), 200)
}

async function onLine(line) {
  const text = line.trim()
  if (approval) {
    const yes = /^(y|yes|是|允许|好|ok)$/i.test(text)
    const { rpcId, approvalId } = approval
    approval = null
    await respond({ type: 'client-response', rpcId, result: { ok: true, value: { sessionId, approvalId, outcome: yes ? 'allowed-once' : 'rejected' } } })
    return
  }
  if (question) {
    const { rpcId, questions } = question
    question = null
    await respond({ type: 'client-response', rpcId, result: { ok: true, value: { sessionId, answer: { answers: answersFor(questions, text) } } } })
    return
  }
  if (!text) return prompt()
  if (text === '/exit' || text === '/quit') return quit(0)
  if (text === '/help') { say('/sessions 最近的会话  /resume <id> 接着聊  /new 新会话  /exit 退出\n其他 / 开头的是 DSH 自己的命令（/compact、/permission read-only…）\nCtrl+C 停止当前回答；空闲时再按一次退出'); return prompt() }
  if (text === '/sessions') { await listSessions(); return prompt() }
  if (text === '/new') { await openSession(null); return prompt() }
  if (text.startsWith('/resume')) { const id = text.split(/\s+/)[1]; if (id) await openSession(id); else await listSessions(); return prompt() }
  if (text.startsWith('/')) {
    const done = await rpc('commands/execute', { args: { agentId: sessionId, line: text, images: [] } })
    if (done) { say((done.result?.kind === 'error' ? color.red : color.dim)(done.result?.text ?? '完成')); return prompt() }
  }
  running = true
  await rpc('session.prompt', { sessionId, mode: 'queue', content: [{ type: 'text', text }] })
}

// ---------------------------------------------------------------- main
try {
  await findOrStart()
  await openSocket('events.mux', onMux)
  await openSocket('events.host', () => {})
  await openSession(resumeId)
  say(color.dim('输入消息发给 DSH；/help 看命令'))
} catch (e) {
  say(color.red(`无法连接 DSH：${e.message}`))
  quit(1)
}

let lastInterrupt = 0
rl.on('SIGINT', async () => {
  if (running && Date.now() - lastInterrupt > 1500) {
    lastInterrupt = Date.now()
    lineBreak(); say(color.dim('正在停止…（再按一次 Ctrl+C 退出）'))
    if (question) { const { rpcId } = question; question = null; respond({ type: 'client-response', rpcId, result: { ok: false, error: { code: 'cancelled', message: 'cancelled', details: {} } } }).catch(() => {}) }
    rpc('session.cancel', { sessionId }).catch((e) => say(color.red(e.message)))
    return
  }
  quit(0)
})
rl.on('close', () => quit(0))
rl.on('line', (line) => { onLine(line).catch((e) => { lineBreak(); say(color.red(e.message)); running = false; prompt() }) })
process.on('SIGTERM', () => quit(0))
process.on('SIGHUP', () => quit(0))
prompt()
