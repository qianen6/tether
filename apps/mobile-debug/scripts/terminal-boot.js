// The terminal page's script, after Herdr Anywhere's (app/src/ui/terminal-page.ts, MIT). build-terminal.mjs
// inlines it after xterm.js and its addons, with window.__terminal = {theme, fonts, fontSize, ...}.
// A phone has no mouse, so the page reads fingers: the keyboard comes up only when the app asks for it, a drag
// scrolls back and a flick keeps going and slows down, a long press selects a word (dragging extends it) and a tap
// on a link opens it. A full-screen program (Codex) keeps its own scrollback: a drag reaches it as a mouse wheel, or
// as arrow keys when it doesn't follow the mouse, as Termux does.
// Page → app (ReactNativeWebView.postMessage, JSON): ready / size {cols, rows}, input {data} or {b64} (bytes),
// selected {text}, link {url}, tap, away {away} (scrolled back, or at the bottom again).
// App → page (injected): window.tether.write(b64) .reset() .focus() .key(name, ctrl, shift) .submit(b64) .bottom().
(function () {
  var cfg = window.__terminal;
  var box = document.getElementById('t');
  var term, fit, last = '', away = false, wide = 0, tall = 0, kitty = [];
  var URLCHAR = /[^\s"'<>\u2500-\u257f\u3000-\u9fff\uff00-\uffef]/;
  function post(m) { window.ReactNativeWebView.postMessage(JSON.stringify(m)); }
  function bytes(b64) { var s = atob(b64), a = new Uint8Array(s.length); for (var i = 0; i < s.length; i++) a[i] = s.charCodeAt(i); return a; }
  // Only a new width changes the terminal's size. A TUI told of a new height redraws, and what it had on screen ends
  // up in the scrollback a second time; so the keyboard and the composer cover the page's bottom, and lift() keeps
  // what the screen shows above them.
  function room() { return window.innerHeight - cfg.pad * 2; }
  function lastRow() {
    var b = term.buffer.active, end = b.cursorY;
    for (var i = term.rows - 1; i > end; i--) { var l = b.getLine(b.baseY + i); if (l && l.translateToString(true).trim()) { end = i; break; } }
    return end;
  }
  function lift() {
    var over = tall - room(), b = term.buffer.active, y = 0;
    if (over > 0) {
      y = over;
      if (!away) {
        var h = term.element.querySelector('.xterm-screen').getBoundingClientRect().height / term.rows;
        y = Math.max(0, Math.min(over, (lastRow() + 1 + b.baseY - b.viewportY) * h - room()));
      }
    }
    box.style.transform = y ? 'translateY(' + (-y) + 'px)' : '';
  }
  // The view ends at the screen's last line: empty rows under it give way to the history above. A full-screen program
  // owns every row.
  function live() {
    var b = term.buffer.active;
    if (b.type === 'alternate') return b.baseY;
    return b.baseY - Math.min(b.baseY, term.rows - 1 - lastRow());
  }
  // Scrolled back from the live view, or at it again. Only a finger or Latest moves the view; output that lands while
  // the view sits above the empty rows would otherwise read as scrolling back.
  function where() {
    var b = term.buffer.active, a = b.viewportY < live();
    if (a !== away) { away = a; post({ t: 'away', away: a }); lift(); }
  }
  function follow() { if (!away && term.buffer.active.viewportY !== live()) term.scrollToLine(live()); }
  // A full-screen program (its own screen, no scrollback to repeat) fits what is left above the keyboard: it
  // redraws there whole, where lifting would push its top out of sight.
  function size() {
    var h = room();
    if (window.innerWidth !== wide || term.buffer.active.type === 'alternate') { wide = window.innerWidth; tall = h; }
    else if (h > tall) tall = h;
    box.style.bottom = 'auto';
    box.style.height = tall + 'px';
    try { fit.fit(); } catch (e) {}
    follow();
    lift();
    var now = term.cols + 'x' + term.rows;
    if (now !== last && term.cols > 0 && term.rows > 0) { var first = !last; last = now; post({ t: first ? 'ready' : 'size', cols: term.cols, rows: term.rows }); }
  }
  // Every column that fits. FitAddon keeps room for the ruler, a whole column when the screen is an exact number of them.
  function columns(cols) {
    var cell = term.element.querySelector('.xterm-screen').getBoundingClientRect().width / term.cols;
    return cell > 0 ? Math.max(cols, Math.floor((box.getBoundingClientRect().width + 0.01) / cell)) : cols;
  }
  // Where a row's text ends, in cells (Chinese takes two), and the character there.
  function ending(l) {
    for (var x = l ? l.length - 1 : -1; x >= 0; x--) { var k = l.getCell(x); if (k && k.getChars().trim()) return { end: x + Math.max(1, k.getWidth()), wide: k.getWidth() === 2 }; }
    return { end: 0, wide: false };
  }
  function indent(l) { for (var x = 0; l && x < l.length; x++) { var k = l.getCell(x); if (k && k.getChars().trim()) return x; } return l ? l.length : 0; }
  // Did an agent's TUI break this row's line at the right edge? It writes each row on its own, so the terminal does
  // not know. It did when the next row's first word would not have fitted after this row's text, and the next row goes
  // on at the same indent or deeper, without starting a list item, a quote, a heading or a box.
  function tuiWraps(y) {
    var b = term.buffer.active, l = b.getLine(y), n = b.getLine(y + 1);
    if (!l || !n || n.isWrapped) return null;
    var e = ending(l), from = indent(n);
    if (!e.end || e.end < term.cols / 2 || from >= n.length || from < indent(l)) return null;
    var head = n.translateToString(true, from, Math.min(n.length, from + 6));
    if (/^([-*+>#|•·⏺⎿\u2500-\u257f]|\d+[.)] )/.test(head)) return null;
    var first = n.getCell(from), joined = e.wide || first.getWidth() === 2, w = 0;
    if (first.getWidth() === 2) w = 2;
    else for (var x = from; x < n.length; x++) { var k = n.getCell(x); if (!k.getChars().trim() || k.getWidth() === 2) break; w++; }
    if (e.end + (joined ? 0 : 1) + w <= term.cols) return null;
    return { from: from, gap: joined ? '' : ' ' };
  }
  // What a selection copies: rows the terminal wrapped, and rows a TUI wrapped, join into the line they were.
  var span = null;
  function selectedText() {
    var all = term.getSelection();
    if (!span || !all) return all;
    var b = term.buffer.active, s = span.s, e = span.e, text = '', start = s.col;
    for (var y = s.row; y <= e.row; y++) {
      var l = b.getLine(y); if (!l) break;
      var n = y < e.row ? b.getLine(y + 1) : null, w = n ? tuiWraps(y) : null;
      var end = y === e.row ? e.col + 1 : l.length;
      text += start < end ? l.translateToString(!(n && n.isWrapped), start, end) : '';
      start = 0;
      if (!n || n.isWrapped) continue;
      if (w) {
        text = text.replace(/\s+$/, '');
        // A link longer than a row is cut anywhere: its pieces join with nothing between them.
        text += /https?:\/\/\S*$/.test(text) && ending(l).end >= term.cols ? '' : w.gap;
        start = w.from;
      } else text += '\n';
    }
    return text;
  }
  // The keyboard: only when the app asks. xterm focuses its textarea on every mouse event, so between two asks the
  // textarea says it wants no keyboard.
  function keyboard() { var ta = term.textarea; if (ta && ta.getAttribute('inputmode') !== 'text') { ta.blur(); ta.setAttribute('inputmode', 'text'); } term.focus(); }
  // Where a finger is, in cells of the whole buffer.
  function cell(x, y) {
    var r = term.element.querySelector('.xterm-screen').getBoundingClientRect();
    var w = r.width / term.cols, h = r.height / term.rows;
    var col = Math.max(0, Math.min(term.cols - 1, Math.floor((x - r.left) / w)));
    var row = Math.max(0, Math.min(term.rows - 1, Math.floor((y - r.top) / h)));
    return { col: col, row: row + term.buffer.active.viewportY, h: h };
  }
  // A row's text, and where each cell starts in it (a wide character takes two cells and one place in the text).
  function rowOf(y) {
    var l = term.buffer.active.getLine(y), text = '', at = [], c;
    for (var x = 0; l && x < l.length; x++) { at.push(text.length); c = l.getCell(x); text += c ? c.getChars() || (c.getWidth() ? ' ' : '') : ' '; }
    return { text: text, at: at, wrapped: !!(l && l.isWrapped) };
  }
  // Does the text of row y go on in row y + 1? The terminal says when it wrapped it; a TUI wraps by itself, with its
  // indent, at the right edge, and a link's characters are then on both sides of the break.
  function flows(y) {
    var a = rowOf(y).text.replace(/\s+$/, ''), n = rowOf(y + 1);
    return n.wrapped || (ending(term.buffer.active.getLine(y)).end >= term.cols - 6 && URLCHAR.test(a[a.length - 1] || ' ') && URLCHAR.test(n.text.replace(/^\s+/, '')[0] || ' '));
  }
  function linkInText(c) {
    var first = c.row, rows = 0;
    while (first > 0 && rows++ < 8 && flows(first - 1)) first--;
    var text = '', base = 0, y;
    for (y = first; ; y++) {
      var r = rowOf(y), piece = r.text;
      var wrapped = r.wrapped || y === first;
      if (!wrapped) {
        // Only a link goes on across a TUI's break: other text keeps its lines.
        if (!/https?:\/\/\S*$/.test(text)) { if (y > c.row) break; text = ''; }
        piece = piece.replace(/^\s+/, '');
      }
      if (y === c.row) {
        var lead = r.text.length - piece.length, i = r.at[c.col] === undefined ? r.text.length : r.at[c.col];
        if (i < lead) return '';
        base = text.length + i - lead;
      }
      text += flows(y) ? piece.replace(/\s+$/, '') : piece;
      if (y >= c.row && !flows(y)) break;
      if (y - first > 12) break;
    }
    var re = /https?:\/\/[^\s"'<>\u2500-\u257f\u3000-\u9fff\uff00-\uffef]+/g, m;
    while ((m = re.exec(text))) { var url = m[0].replace(/[.,;:!?)\]]+$/, ''); if (base >= m.index && base < m.index + url.length) return url; }
    return '';
  }
  // A link behind text (OSC 8): nothing in the buffer's public API says which cell has one, so note where each was written.
  var linked = [], linking = null;
  function here() { var b = term.buffer.active; return { at: term.registerMarker(0) || { line: b.baseY + b.cursorY }, col: b.cursorX }; }
  function hyperlink(data) {
    var uri = data.slice(data.indexOf(';') + 1);
    if (linking) { linking.end = here(); linked.push(linking); linking = null; if (linked.length > 200) linked.shift(); }
    if (/^https?:\/\//.test(uri)) { linking = { url: uri, start: here() }; }
    return false;
  }
  function linkBehind(c) {
    for (var i = linked.length - 1; i >= 0; i--) {
      var k = linked[i], a = k.start.at.line, z = k.end.at.line;
      if (a < 0 || z < 0) continue;
      if ((c.row > a || (c.row === a && c.col >= k.start.col)) && (c.row < z || (c.row === z && c.col < k.end.col))) return k.url;
    }
    return '';
  }
  function linkAt(c) { return linkBehind(c) || linkInText(c); }
  // The word under the finger, selected; a drag goes on from its start.
  function selectWord(c) {
    var l = term.buffer.active.getLine(c.row);
    var space = function (x) { var k = l && l.getCell(x); return !k || (k.getWidth() && !k.getChars().trim()); };
    if (space(c.col)) { term.select(c.col, c.row, 1); span = { s: c, e: c }; return c; }
    var a = c.col, b = c.col;
    while (a > 0 && !space(a - 1)) a--;
    while (b < term.cols - 1 && !space(b + 1)) b++;
    term.select(a, c.row, b - a + 1);
    span = { s: { col: a, row: c.row }, e: { col: b, row: c.row } };
    return { col: a, row: c.row, h: c.h };
  }
  function selectSpan(from, to) {
    var s = from, e = to;
    if (to.row < from.row || (to.row === from.row && to.col < from.col)) { s = to; e = from; }
    term.select(s.col, s.row, (e.row - s.row) * term.cols + (e.col - s.col) + 1);
    span = { s: s, e: e };
  }
  // Back in history by this many lines. A full-screen program has none here: xterm turns a wheel into what that program
  // asked for (its mouse encoding, or arrow keys in its cursor mode).
  function ownScroll() { return term.modes.mouseTrackingMode !== 'none' || term.buffer.active.type === 'alternate'; }
  function scrollView(lines, x, y) {
    if (!ownScroll()) { term.scrollLines(-lines); if (term.buffer.active.viewportY > live()) term.scrollToLine(live()); return where(); }
    for (var i = Math.abs(lines); i > 0; i--) term.element.dispatchEvent(new WheelEvent('wheel', { deltaY: lines > 0 ? -1 : 1, deltaMode: 1, clientX: x, clientY: y, bubbles: true, cancelable: true }));
  }
  // After the finger lifts the view keeps its speed (px per ms, down = back in history) and loses it smoothly, the
  // curve native lists use: speed halves about every 225 ms. xterm scrolls whole lines, so what is less than a line waits.
  var gliding = 0;
  function stopGlide() { var was = !!gliding; if (gliding) cancelAnimationFrame(gliding); gliding = 0; return was; }
  function glide(v, h, x, y) {
    // A program answers every wheel over the network, so a flick sends it fewer than the page can scroll by itself.
    var most = ownScroll() ? 2 : 8;
    v = Math.max(-most, Math.min(most, v));
    if (Math.abs(v) < 0.25) return;
    var last = performance.now(), carry = 0;
    function step(now) {
      var dt = Math.max(0, now - last);
      last = now; carry += v * dt; v *= Math.exp(-dt / 325);
      var n = Math.trunc(carry / h), from = term.buffer.active.viewportY;
      if (n) { scrollView(n, x, y); carry -= n * h; }
      var ended = Math.abs(v) < 0.03 || (n && !ownScroll() && term.buffer.active.viewportY === from);
      gliding = ended ? 0 : requestAnimationFrame(step);
    }
    gliding = requestAnimationFrame(step);
  }
  function touches() {
    var t0 = null, selecting = null, press = 0, scrolled = 0, moved = false, dragging = false, trail = [], stopped = false;
    // touchstart keeps its default: it is what gives the WebView the system's focus, without which focusing the
    // textarea brings no keyboard up. touch-action:none already stops panning and zooming. touchend never does: its
    // default is the mouse events a tap turns into.
    box.addEventListener('touchstart', function (e) {
      clearTimeout(press);
      stopped = stopGlide();
      if (e.touches.length !== 1) { t0 = null; selecting = null; return; }
      var p = e.touches[0];
      t0 = { x: p.clientX, y: p.clientY, at: Date.now() }; moved = false; scrolled = 0; selecting = null; dragging = false; trail = [];
      press = setTimeout(function () { if (!t0 || moved) return; selecting = selectWord(cell(t0.x, t0.y)); }, 450);
    }, { passive: true });
    box.addEventListener('contextmenu', function (e) { e.preventDefault(); });
    box.addEventListener('touchmove', function (e) {
      e.preventDefault();
      if (!t0 || e.touches.length !== 1) return;
      var p = e.touches[0], dx = p.clientX - t0.x, dy = p.clientY - t0.y;
      if (Math.abs(dx) > 8 || Math.abs(dy) > 8) { moved = true; clearTimeout(press); }
      if (selecting) { selectSpan(selecting, cell(p.clientX, p.clientY)); return; }
      if (moved && Math.abs(dy) > Math.abs(dx)) {
        var h = cell(p.clientX, p.clientY).h, lines = Math.trunc((dy - scrolled) / h);
        if (lines) { scrollView(lines, p.clientX, p.clientY); scrolled += lines * h; }
        dragging = true;
        trail.push({ y: p.clientY, at: e.timeStamp });
        while (trail.length > 2 && e.timeStamp - trail[0].at > 100) trail.shift();
      }
    }, { passive: false });
    box.addEventListener('touchend', function (e) {
      clearTimeout(press);
      e.preventDefault();
      if (!t0 || e.touches.length) return;
      var p = e.changedTouches[0], ms = Date.now() - t0.at, start = t0;
      t0 = null;
      if (selecting) { selecting = null; var text = selectedText(); if (text) post({ t: 'selected', text: text }); return; }
      if (dragging && trail.length > 1 && e.timeStamp - trail[trail.length - 1].at < 60) {
        var a = trail[0], z = trail[trail.length - 1];
        glide((z.y - a.y) / (z.at - a.at), cell(p.clientX, p.clientY).h, p.clientX, p.clientY);
        return;
      }
      if (!moved && ms < 450 && !stopped) {
        term.clearSelection(); span = null;
        var url = linkAt(cell(start.x, start.y));
        if (url) { post({ t: 'link', url: url }); return; }
        post({ t: 'tap' });
      }
    }, { passive: false });
  }
  function boot() {
    term = new Terminal({ overviewRuler: { width: 1 }, fontFamily: cfg.fonts, fontSize: cfg.fontSize,
      fontWeight: cfg.fontWeight, fontWeightBold: cfg.fontWeightBold, lineHeight: cfg.lineHeight,
      scrollback: cfg.scrollback, cursorBlink: true, allowProposedApi: true, customGlyphs: true, theme: cfg.theme });
    // The widths Codex and Claude Code count (emoji are 2 cells, a joined emoji one character); xterm's default is
    // Unicode 6, where they are 1 and all that follows on the line lands in the wrong column.
    term.loadAddon(new UnicodeGraphemesAddon.UnicodeGraphemesAddon());
    term.unicode.activeVersion = '15-graphemes';
    fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    var propose = fit.proposeDimensions.bind(fit);
    fit.proposeDimensions = function () { var d = propose(); if (d) d.cols = columns(d.cols); return d; };
    term.open(box);
    // Opening the page must not bring the keyboard up.
    if (term.textarea) {
      term.textarea.setAttribute('inputmode', 'none'); term.textarea.blur();
      term.textarea.addEventListener('blur', function () { term.textarea.setAttribute('inputmode', 'none'); });
    }
    // WebGL draws box drawing as joined shapes; without it (or when the context is lost) the DOM renderer draws on.
    try { var gl = new WebglAddon.WebglAddon(); gl.onContextLoss(function () { gl.dispose(); }); term.loadAddon(gl); } catch (e) {}
    new ResizeObserver(size).observe(document.documentElement);
    term.onData(function (d) { post({ t: 'input', data: d }); });
    term.onBinary(function (d) { var s = ''; for (var i = 0; i < d.length; i++) s += String.fromCharCode(d.charCodeAt(i) & 255); post({ t: 'input', b64: btoa(s) }); });
    term.onWriteParsed(function () { follow(); where(); lift(); });
    term.buffer.onBufferChange(function () { size(); });
    term.parser.registerOscHandler(8, hyperlink);
    // Which keys the program asked for (kitty keyboard protocol): the way it reads Ctrl with Enter or Tab.
    term.parser.registerCsiHandler({ prefix: '>', final: 'u' }, function (p) { kitty.push(p[0] || 0); return false; });
    term.parser.registerCsiHandler({ prefix: '<', final: 'u' }, function () { kitty.pop(); return false; });
    term.parser.registerCsiHandler({ prefix: '=', final: 'u' }, function (p) { kitty[Math.max(0, kitty.length - 1)] = p[0] || 0; return false; });
    var CSI = { up: 'A', down: 'B', right: 'C', left: 'D', home: 'H', end: 'F' };
    // Shift and Ctrl held from the key row, as xterm writes modified keys (1 + 1 for Shift + 4 for Ctrl); Shift+Tab
    // alone is back-tab, which every program knows.
    function held(name, plain, shift, ctrl) {
      var mod = 1 + (shift ? 1 : 0) + (ctrl ? 4 : 0);
      if (name === 'tab' && shift && !ctrl) return '\x1b[Z';
      var code = name === 'enter' ? 13 : name === 'tab' ? 9 : 0;
      if (code) return kitty[kitty.length - 1] ? '\x1b[' + code + ';' + mod + 'u' : '\x1b[27;' + mod + ';' + code + '~';
      return CSI[name] ? '\x1b[1;' + mod + CSI[name] : plain;
    }
    window.tether = {
      write: function (b64) { term.write(bytes(b64)); },
      reset: function () { stopGlide(); term.reset(); kitty = []; span = null; linked = []; linking = null; },
      focus: keyboard,
      bottom: function () { stopGlide(); term.scrollToLine(live()); where(); },
      // A row key, in the program's cursor-key mode.
      key: function (name, ctrl, shift) {
        var seq = name === 'esc' ? '\x1b' : name === 'tab' ? '\t' : name === 'enter' ? '\r' : name === 'ctrlc' ? '\x03'
          : CSI[name] ? (term.modes.applicationCursorKeysMode ? '\x1bO' : '\x1b[') + CSI[name] : '';
        if (ctrl || shift) seq = held(name, seq, shift, ctrl);
        if (seq) post({ t: 'input', data: seq });
      },
      // The composer's text: pasted (bracketed when the program asked, so a line break stays in it), then Enter on
      // its own, a moment later, as a typed key.
      submit: function (b64) {
        term.paste(new TextDecoder().decode(bytes(b64)));
        setTimeout(function () { post({ t: 'input', data: '\r' }); }, 80);
      },
    };
    touches();
    size();
  }
  // Cells are measured from the font, so wait for it (a moment at most).
  var booted = false;
  function once() { if (!booted) { booted = true; boot(); } }
  try {
    Promise.all([document.fonts.load(cfg.fontSize + "px 'JetBrains Mono'"), document.fonts.load(cfg.fontSize + "px 'Noto Sans Symbols 2'", '\u23fa'),
      document.fonts.load(cfg.fontSize + "px 'Noto Sans Symbols'", '\u23bf'), document.fonts.load(cfg.fontSize + "px 'Noto Sans Mono'", '\u23ba')]).then(once, once);
  } catch (e) { once(); }
  setTimeout(once, 1500);
})();
