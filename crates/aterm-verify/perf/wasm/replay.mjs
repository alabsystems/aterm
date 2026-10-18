// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The CPU module's boundary replay (`verify wasm-smoke`): one fixed
// script of calls into aterm-wasm's exports, every answer encoded
// type-exactly (BigInt, undefined, -0, typed arrays by constructor, class
// instances by their getters, a thrown string vs a thrown Error) and held
// to the answers recorded in crates/aterm-wasm/tests/fixtures/abi_replay.json.
//
// The answers were recorded on the module wasm-bindgen 0.2.108 bound
// (docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.7), so a module
// bound another way that reproduces them carries every value across the
// boundary as that one did.
//
//   node replay.mjs check  <answers.json> <glue dir> nodejs|web
//   node replay.mjs record <answers.json> <glue dir> nodejs|web
//
// The script covers the bench.mjs workloads at a size a debug build runs in
// seconds, the key-encode table, search, link_at, selection, the message
// band, the mouse and paste encoders and the static exports. Every frame is
// hashed (SHA-256) twice: the `rgba()` copy and the `rgba_ptr` view of
// linear memory. Pointers are recorded only as numbers: the bytes behind
// them are recorded instead. The clocks the module reads are pinned
// (`performance.now`, `Date.now`), and the font is the one in the tree, so
// the answers do not depend on the box.
//
// `check` prints the first differences and exits 1 on any; `record`
// rewrites the answers. A change to what the engine draws or answers moves
// some of them: `verify glue --record-replay`, then review the diff (one
// answer per line, labelled by its call).

import { createHash } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

const STEM = 'aterm_wasm'
const [mode, answersPath, dir, flavor] = process.argv.slice(2)
if (!['check', 'record'].includes(mode) || !answersPath || !dir || !['nodejs', 'web'].includes(flavor)) {
  console.error('usage: node replay.mjs check|record <answers.json> <glue dir> nodejs|web')
  process.exit(2)
}

// --- pinned clocks: the module reads them at call time ----------------------
let monoMs = 1000
let wallMs = 1_760_000_000_000
function advance(ms) {
  monoMs += ms
  wallMs += ms
}
Object.defineProperty(performance, 'now', { value: () => monoMs, configurable: true, writable: true })
Date.now = () => wallMs

// --- the module, and its linear memory -------------------------------------
let memory = null
let mod
if (flavor === 'nodejs') {
  // The CommonJS glue keeps its instance private; take the memory as the
  // glue instantiates.
  const Instance = WebAssembly.Instance
  WebAssembly.Instance = function (module, imports) {
    const instance = new Instance(module, imports)
    memory = instance.exports.memory
    return instance
  }
  mod = createRequire(import.meta.url)(join(dir, `${STEM}.js`))
  WebAssembly.Instance = Instance
} else {
  mod = await import(pathToFileURL(join(dir, `${STEM}.js`)).href)
  memory = mod.initSync({ module: readFileSync(join(dir, `${STEM}_bg.wasm`)) }).memory
}
if (!(memory instanceof WebAssembly.Memory)) {
  console.error('wasm-replay: the module exported no memory')
  process.exit(1)
}

// --- type-exact encoding -----------------------------------------------------
function sha(bytes) {
  return createHash('sha256').update(bytes).digest('hex')
}
function bytesOf(view) {
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength)
}
function getters(proto) {
  return Object.entries(Object.getOwnPropertyDescriptors(proto))
    .filter(([, d]) => typeof d.get === 'function')
    .map(([k]) => k)
    .sort()
}
const CLASSES = new Set(['AtermTerminal', 'BudgetedSearchResult', 'LinkHit', 'SearchMeta', 'SelectionRange'])

function enc(v) {
  switch (typeof v) {
    case 'undefined':
      return ['undefined']
    case 'boolean':
      return ['boolean', v]
    case 'bigint':
      return ['bigint', v.toString()]
    case 'number':
      return ['number', Object.is(v, -0) || !Number.isFinite(v) ? String(v) : v]
    case 'string':
      return v.length <= 200 ? ['string', v] : ['string#', v.length, sha(v)]
    default:
  }
  if (v === null) return ['null']
  if (ArrayBuffer.isView(v)) {
    const name = v.constructor.name
    return v.length <= 64
      ? [name, Array.from(v, (x) => (typeof x === 'bigint' ? x.toString() : x))]
      : [`${name}#`, v.length, sha(bytesOf(v))]
  }
  const name = v?.constructor?.name
  if (CLASSES.has(name)) {
    const fields = {}
    for (const g of getters(Object.getPrototypeOf(v))) fields[g] = enc(v[g])
    return ['class', name, fields]
  }
  return ['other', Object.prototype.toString.call(v)]
}

/** A call's answer: its value, or what it threw (a string, or an Error's class and message). */
function call(f) {
  try {
    return enc(f())
  } catch (e) {
    return typeof e === 'string' ? ['throw', 'string', e] : ['throw', e?.constructor?.name, String(e?.message)]
  }
}

const answers = []
function rec(label, value) {
  answers.push([label, value])
}
function step(label, f) {
  rec(label, call(f))
}

/** A view of linear memory, built fresh per read (growth detaches old views). */
function view(ptr, len) {
  return new Uint8Array(memory.buffer, ptr >>> 0, len >>> 0)
}

// --- the module's surface ----------------------------------------------------
function surface() {
  const internal = (k) => k.startsWith('__') || k === 'default' || k === 'initSync'
  const names = Object.keys(mod).filter((k) => !internal(k)).sort()
  const out = { exports: names }
  for (const k of names) {
    const v = mod[k]
    if (typeof v !== 'function') continue
    if (CLASSES.has(k)) {
      const own = Object.getOwnPropertyDescriptors(v.prototype)
      out[k] = {
        methods: Object.keys(own).filter((m) => typeof own[m].value === 'function' && !internal(m)).sort(),
        getters: getters(v.prototype),
        statics: Object.getOwnPropertyNames(v)
          .filter((m) => typeof v[m] === 'function' && !internal(m))
          .sort(),
        dispose: typeof v.prototype[Symbol.dispose] === 'function'
      }
    }
  }
  return out
}
rec('surface', surface())

// --- inputs ------------------------------------------------------------------
const FONT = readFileSync(new URL('../../../aterm-render/assets/DejaVuSansMono.ttf', import.meta.url))
const THEME = [0x00e0e0e0, 0x00101010, 0x00e0e0e0, 0x00404060]
const text = (s) => new TextEncoder().encode(s)

function mixed(bytes) {
  const frame = [
    'the quick brown fox jumps over the lazy dog 0123456789\r\n',
    '\x1b[1;38;5;202mERROR\x1b[0m \x1b[4;48;5;19mwarning\x1b[0m normal text here\r\n',
    '\x1b[2K\x1b[1Gredrawn line after erase-in-line and cursor-to-col-1\r\n',
    '\x1b[7minverse\x1b[27m \x1b[3mitalic\x1b[23m \x1b[9mstrike\x1b[29m mixed sgr\r\n',
    '\x1b[10;5Hpositioned via CUP \x1b[K then erased to end of line\r\n',
    'plain throughput line with a tab\tand more ascii content padding 42\r\n',
    '\x1b[38;2;120;200;255mtruecolor\x1b[0m run plus \x1b[48;2;10;10;10mbg\x1b[0m\r\n',
    '\x1b[H\x1b[2Jfull clear then home, forcing a screen reset every frame\r\n'
  ].join('')
  return text(frame.repeat(Math.ceil(bytes / frame.length)))
}
function replog(n) {
  const t = ['INFO  svc-api    request completed status=200', 'WARN  svc-api    slow request', 'ERROR svc-db     connection reset']
  let s = ''
  for (let i = 0; i < n; i++) s += `2026-07-22T12:00:${String(i % 60).padStart(2, '0')}Z ${t[i % t.length]} latency_ms=${i % 250}\r\n`
  return text(s)
}
const GLYPHS = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/-='
function rotating(n) {
  let s = ''
  for (let line = 0; line < n; line++) {
    let body = ''
    for (let c = 0; c < 40; c++) body += GLYPHS[(line + c) % GLYPHS.length]
    s += `${line} ${body}\r\n`
  }
  return s
}
function styled(n) {
  let s = ''
  for (let line = 0; line < n; line++) {
    for (let k = 0; k < 20; k++) {
      s += `\x1b[${[1, 3, 4, 7][(line + k) % 4]};38;5;${(line * 7 + k * 13) % 256}m`
      s += GLYPHS.slice((line + k) % 60, ((line + k) % 60) + 4)
    }
    s += '\x1b[0m\r\n'
  }
  return s
}

// --- frames ------------------------------------------------------------------
/** Render, then hash the frame both ways and read the dirty bands and spill. */
function frame(label, t) {
  step(`${label} render`, () => t.render())
  const w = t.width
  const h = t.height
  const copy = t.rgba()
  const rgba = sha(copy)
  const ptr = t.rgba_ptr()
  const live = sha(view(ptr, w * h * 4))
  const bands = t.present_band_count()
  const rects = t.spill_rect_count()
  rec(`${label} frame`, {
    w: enc(w),
    h: enc(h),
    rgba,
    rgba_len: copy.length,
    ptr_view_equals_copy: live === rgba,
    ptr: typeof ptr,
    skipped: enc(t.last_render_skipped()),
    bands: enc(bands),
    band_bytes: sha(view(t.present_bands_ptr(), bands * 16)),
    rects: enc(rects),
    rect_bytes: sha(view(t.spill_rects_ptr(), rects * 16)),
    spill_rev: enc(t.spill_rev()),
    spill: sha(view(t.spill_ptr(), t.spill_len())),
    needs_frame: enc(t.needs_frame())
  })
}

/** Every getter of the terminal, by name. */
function state(label, t) {
  const fields = {}
  for (const g of getters(Object.getPrototypeOf(t))) fields[g] = call(() => t[g])
  rec(`${label} state`, fields)
}

// --- the script --------------------------------------------------------------
step('register_font', () => mod.register_font(FONT))
step('register_font again (interned)', () => mod.register_font(FONT))
step('ctor on a bad font throws', () => new mod.AtermTerminal(24, 80, new Uint8Array([1, 2, 3]), 16, ...THEME))
step('new_registered on a bad handle throws', () => mod.AtermTerminal.new_registered(24, 80, 999, 16, ...THEME))
step('tier_capabilities_json', () => mod.AtermTerminal.tier_capabilities_json())
step('set_scrollback_global_budget', () => mod.AtermTerminal.set_scrollback_global_budget(64 << 20))

const t = new mod.AtermTerminal(24, 80, FONT, 16.0, ...THEME)
state('fresh', t)
frame('fresh', t)

// bench: ingest
step('process mixed', () => t.process(mixed(64 << 10)))
frame('mixed', t)
step('process replog', () => t.process(replog(400)))
frame('replog', t)
state('after ingest', t)

// bench: scroll + present over history, typing + present
step('set_scrollback_limit', () => t.set_scrollback_limit(5000))
step('process rotating', () => t.process(text(rotating(3000))))
frame('history', t)
for (let i = 0; i < 12; i++) {
  step(`scroll ${i}`, () => t.scroll_lines(3))
  frame(`scroll ${i}`, t)
}
state('scrolled', t)
step('scroll_to_bottom', () => t.scroll_to_bottom())
for (let i = 0; i < 8; i++) {
  step(`type ${i}`, () => t.process_str('x'))
  frame(`type ${i}`, t)
}

// bench: uniform flood, scrolled stream, styled scrub
{
  const line = text('flood line: the same bytes every time 0123456789\r\n')
  for (let i = 0; i < 6; i++) {
    t.process(line)
    frame(`flood ${i}`, t)
  }
  step('scroll back 50', () => t.scroll_lines(50))
  for (let i = 0; i < 6; i++) {
    step(`stream ${i}`, () => t.process_str(`streamed ${i} while the reader is scrolled back\r\n`))
    frame(`stream ${i}`, t)
    step(`stream ${i} display_offset`, () => t.display_offset)
  }
  step('scroll_to_bottom 2', () => t.scroll_to_bottom())
  step('process styled', () => t.process(text(styled(600))))
  for (let i = 0; i < 6; i++) {
    t.scroll_lines(3)
    frame(`scrub ${i}`, t)
  }
  step('fractional scroll', () => t.scroll_lines_frac(1.5))
  step('scroll_px', () => t.scroll_px(-7.25))
  frame('fractional', t)
  step('scroll_to_top', () => t.scroll_to_top())
  frame('top', t)
  step('scroll_to_bottom 3', () => t.scroll_to_bottom())
}

// search
for (const [q, cs, re] of [
  ['jkl', true, false],
  ['JKL', false, false],
  ['[0-9]+ abc', true, true],
  ['(unclosed', true, true],
  ['no such text anywhere', true, false],
  ['', true, false],
  ['é€😀', true, false]
]) {
  step(`search ${q}`, () => t.search(q, cs, re))
  step(`search_meta ${q}`, () => t.search_meta(q, cs, re))
  step(`search_summary ${q}`, () => t.search_summary(q, cs, re, 5))
}
{
  let cursor
  for (let i = 0; i < 4; i++) {
    let r
    step(`search_budgeted ${i}`, () => {
      r = t.search_budgeted('jkl', true, false, cursor, 700)
      return r
    })
    cursor = r?.cursor
    if (cursor === undefined) break
  }
  step('search_budgeted null cursor', () => t.search_budgeted('abc', true, false, null, 50))
  step('search_budgeted_cancel', () => t.search_budgeted_cancel())
  step('search_display_origin', () => t.search_display_origin)
  step('scroll_search_line_into_view', () => t.scroll_search_line_into_view(1000))
  frame('search line', t)
  step('search_index_release', () => t.search_index_release())
  step('scroll_to_bottom 4', () => t.scroll_to_bottom())
}

// links
step('authorize https', () => t.authorize_hyperlink_scheme('https'))
step('authorize javascript', () => t.authorize_hyperlink_scheme('javascript'))
step(
  'process links',
  () =>
    t.process_str(
      '\x1b[2J\x1b[Hsee https://example.com/a?b=c#d and /usr/local/bin/aterm\r\n' +
        '\x1b]8;;https://alab.systems/x\x1b\\osc eight link\x1b]8;;\x1b\\ then file:///tmp/f.txt\r\n' +
        'mailto:hello@example.com ftp://h/p (https://paren.example/x)\r\n'
    )
)
for (const row of [0, 1, 2, 3]) {
  for (let col = 0; col < 64; col += 3) step(`link_at ${row},${col}`, () => t.link_at(row, col))
}
step('revoke https', () => t.revoke_hyperlink_scheme('https'))
step('link_at after revoke', () => t.link_at(1, 2))

// selection
step('selection_range none', () => t.selection_range())
step('selection_text none', () => t.selection_text())
step('selection_start', () => t.selection_start(0, 4))
step('selection_extend', () => t.selection_extend(2, 10))
step('selection_finish', () => t.selection_finish())
step('selection_range', () => t.selection_range())
step('selection_text', () => t.selection_text())
frame('selection', t)
step('selection_word', () => t.selection_word(0, 12))
step('selection_text word', () => t.selection_text())
step('selection_line', () => t.selection_line(1, 3))
step('selection_text line', () => t.selection_text())
step('set_word_separators', () => t.set_word_separators(' /:'))
step('selection_word separators', () => t.selection_word(0, 12))
step('set_word_separators default', () => t.set_word_separators(undefined))
step('selection_start in history', () => t.selection_start(-5, 0))
step('selection_extend in history', () => t.selection_extend(-2, 79))
step('selection_range history', () => t.selection_range())
step('selection_text history', () => t.selection_text())
step('set_selection_fg', () => t.set_selection_fg(0x00ff0000))
step('set_selection_inactive', () => t.set_selection_inactive(true))
step('set_selection_inactive_bg', () => t.set_selection_inactive_bg(undefined))
frame('selection inactive', t)
step('selection_clear', () => t.selection_clear())
step('selection_range cleared', () => t.selection_range())

// rows and cells
step('process wide', () => t.process_str('\x1b[2J\x1b[Hwide: 漢字 and 😀 then a long line ' + 'w'.repeat(90) + '\r\n'))
for (const [r, c] of [[0, 0], [0, 6], [0, 7], [0, 9], [0, 79], [30, 0]]) {
  step(`cell_text ${r},${c}`, () => t.cell_text(r, c))
  step(`cell_is_wide ${r},${c}`, () => t.cell_is_wide(r, c))
}
for (const r of [0, 1, 2, 23, 24, 1000]) {
  step(`row_text ${r}`, () => t.row_text(r))
  step(`row_len ${r}`, () => t.row_len(r))
  step(`row_is_wrapped ${r}`, () => t.row_is_wrapped(r))
}
step('row_range_json', () => t.row_range_json(0, 3))
step('row_range_json past the end', () => t.row_range_json(30, 3))

// replies, titles, events, notifications, bell
step('take_response none', () => t.take_response())
step('process DA', () => t.process_str('\x1b[c\x1b[6n'))
step('take_response DA', () => t.take_response())
step('process title', () => t.process_str('\x1b]0;replay title ✓\x07\x1b]7;file://host/tmp\x07'))
step('title', () => t.title())
step('take_osc_events', () => t.take_osc_events())
step('take_osc_events drained', () => t.take_osc_events())
step('authorize_notifications', () => t.authorize_notifications(true))
step('process notify', () => t.process_str('\x1b]9;hello from replay\x07\x1b]777;notify;Title;Body\x07'))
step('take_notifications', () => t.take_notifications())
step('process bell', () => t.process_str('\x07'))
step('drain_bell', () => t.drain_bell())
step('drain_bell drained', () => t.drain_bell())
step('last_command_output', () => t.last_command_output())
step('take_missing_font_classes', () => t.take_missing_font_classes())

// the key-encode table: per key, one hash over every mods x event x base x mode
const KEYS = [
  'a', 'A', 'z', '1', ' ', '/', 'é', '€', '😀', 'Enter', 'Tab', 'Backspace', 'Escape', 'Delete', 'Insert',
  'Home', 'End', 'PageUp', 'PageDown', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
  'F1', 'F2', 'F5', 'F12', 'Shift', 'Control', 'Dead', 'Unidentified', 'Process', ''
]
const MODE_BITS = [0, 1, 0x7fff, 0x1234, 0xffffffff]
function keyRow(f) {
  const cells = []
  for (let mods = 0; mods < 16; mods++) {
    for (const ev of [0, 1, 2]) {
      for (const base of [undefined, 'a']) cells.push(JSON.stringify(call(() => f(mods, ev, base))))
    }
  }
  return sha(cells.join('\n'))
}
for (const key of KEYS) {
  rec(`encode_key ${JSON.stringify(key)}`, keyRow((m, e, b) => t.encode_key(key, m, e, b)))
  for (const bits of MODE_BITS) {
    rec(`encode_key_with_mode ${JSON.stringify(key)} ${bits}`, keyRow((m, e, b) => mod.encode_key_with_mode(key, m, e, b, bits)))
  }
}
step('kitty push', () => t.process_str('\x1b[>31u\x1b[?1h'))
step('keyboard_mode_bits', () => t.keyboard_mode_bits)
for (const key of ['a', 'Enter', 'ArrowUp', 'F5']) {
  rec(`kitty input ${key}`, keyRow((m, e, b) => t.encode_key(key, m, e, b)))
}
step('set_kitty_keyboard_enabled off', () => t.set_kitty_keyboard_enabled(false))
rec('plain input a', keyRow((m, e, b) => t.encode_key('a', m, e, b)))
step('kitty pop', () => t.process_str('\x1b[<u\x1b[?1l'))
step('set_kitty_keyboard_enabled on', () => t.set_kitty_keyboard_enabled(true))

// mouse and paste
step('encode_mouse_press off', () => t.encode_mouse_press(3, 4, 0, 0))
step('process mouse modes', () => t.process_str('\x1b[?1003h\x1b[?1006h\x1b[?2004h'))
state('mouse modes', t)
for (const [c, r, b, m] of [[0, 0, 0, 0], [3, 4, 1, 4], [79, 23, 2, 15], [300, 300, 0, 0]]) {
  step(`encode_mouse_press ${c},${r},${b},${m}`, () => t.encode_mouse_press(c, r, b, m))
  step(`encode_mouse_release ${c},${r},${b},${m}`, () => t.encode_mouse_release(c, r, b, m))
  step(`encode_mouse_motion ${c},${r},${b},${m}`, () => t.encode_mouse_motion(c, r, b, m))
  step(`encode_mouse_wheel ${c},${r},${m}`, () => t.encode_mouse_wheel(c, r, m % 2 === 0, m))
}
step('format_paste bracketed', () => t.format_paste('line one\nline two\x1b[201~ evil'))
step('process modes off', () => t.process_str('\x1b[?1003l\x1b[?1006l\x1b[?2004l'))
step('format_paste plain', () => t.format_paste('a\r\nb'))

// the message band
step('set_band_visible', () => t.set_band_visible(true))
step('set_band_motion', () => t.set_band_motion(false))
step('set_details_view', () => t.set_details_view(true))
step('set_frame_period_ms', () => t.set_frame_period_ms(16.667))
for (const line of [
  'post replay sev=warn key=r1 hold=30 Replay warning -- with a detail line',
  'post replay sev=error key=r2 Replay error',
  'post replay sev=info key=r3 an info record',
  'progress copy.p1 pct=40 Copying files -- 4 of 10',
  'progress copy.p2 done=3/9 unit=items tag=replay Counting',
  'done copy.p1 ok finished',
  'done copy.p2 withdraw',
  'post aterm sev=warn reserved tag',
  'post',
  'dismiss 999',
  'act 1 details',
  'unknown verb'
]) {
  step(`notice ${line}`, () => t.notice(line))
  advance(40)
  step(`messages_tick after ${line}`, () => t.messages_tick())
}
step('messages_next_wake_ms', () => t.messages_next_wake_ms())
state('band', t)
frame('band', t)
step('messages_read', () => t.messages_read(''))
step('messages_read filtered', () => t.messages_read('5 sev=warn'))
step('messages_read bad', () => t.messages_read('since=x'))
step('messages_entry 1', () => t.messages_entry(1))
step('messages_entry missing', () => t.messages_entry(4096))
step('messages_page all', () => t.messages_page('', false, -120))
step('messages_page problems', () => t.messages_page('replay', true, 300))
step('band_hover', () => t.band_hover(40, 8))
step('band_press_acts', () => t.band_press_acts(40, 8))
step('band_press', () => t.band_press(40, 8))
step('band_hover off', () => t.band_hover(-5, -5))
let log
step('messages_log_since 0', () => {
  log = t.messages_log_since(0)
  return log
})
step('messages_quit', () => t.messages_quit(0))
step('messages_restore after a notice', () => t.messages_restore(typeof log === 'string' ? log : ''))
advance(31_000)
step('messages_tick late', () => t.messages_tick())
step('messages_next_wake_ms late', () => t.messages_next_wake_ms())
frame('band late', t)

// effects and prediction
step('set_sparkle_words_enabled', () => t.set_sparkle_words_enabled(true))
step('set_sparkle_profanity', () => t.set_sparkle_profanity('confetti', 0.8, 900, 0.3, 1.0, true, 0.1))
step('set_sparkle_feline', () => t.set_sparkle_feline('purr', true, true, false))
step('set_sparkle_ink', () => t.set_sparkle_ink(true, 0.7, 600, false))
step('set_sparkle_custom_specs bad toml', () => t.set_sparkle_custom_specs('this is = not [toml'))
step('sparkle_lexicon_warnings', () => t.sparkle_lexicon_warnings)
step('process sparkle', () => t.process_str('\x1b[2J\x1b[Hwhat the fuck, a cat! meow\r\n'))
step('note_typed_char', () => t.note_typed_char('k'))
step('note_keystroke', () => t.note_keystroke())
step('set_cursor_trail', () => t.set_cursor_trail(true, 300, 8, 0x00ff8800))
step('set_cursor_glow', () => t.set_cursor_glow(true, 'aurora', 0x00112233, undefined, 400, 6, 0.8, 3, true))
step('set_cursor_pet', () => t.set_cursor_pet(true, 42n))
step('note_console_input', () => t.note_console_input('typing'))
step('note_console_input bad', () => t.note_console_input('?'))
step('set_matrix_rain', () =>
  t.set_matrix_rain(30, 0.5, 1.0, 0.6, 0.9, undefined, 'green', undefined, 120, 30, true, false, true, false, 7n)
)
step('set_matrix_rain_enabled', () => t.set_matrix_rain_enabled(true))
step('set_output_streak', () => t.set_output_streak(true, 0.8, 0.5, 4, 2.0, false, 9n))
step('set_output_streak_theme', () => t.set_output_streak_theme(undefined))
step('set_effects_visibility', () => t.set_effects_visibility('focused'))
for (let i = 0; i < 10; i++) {
  step(`advance_effects ${i}`, () => t.advance_effects(16.667))
  step(`is_effects_active ${i}`, () => t.is_effects_active())
  step(`effects_next_deadline_ms ${i}`, () => t.effects_next_deadline_ms())
  step(`advance_kitty_animations ${i}`, () => t.advance_kitty_animations())
  step(`cursor_pet_alpha ${i}`, () => t.cursor_pet_alpha)
  frame(`effects ${i}`, t)
}
step('pet_press_px', () => t.pet_press_px(100.5, 200.25))
step('note_pointer_px', () => t.note_pointer_px(12.5, 30))
step('note_pointer_leave', () => t.note_pointer_leave())
step('note_matrix_rain_signal', () => t.note_matrix_rain_signal(3, 0.5))
step('set_predictive_echo', () => t.set_predictive_echo('always'))
for (const ch of 'ls -la') step(`predict_char ${ch}`, () => t.predict_char(ch))
step('predict_backspace', () => t.predict_backspace())
step('predict_overlay', () => t.predict_overlay())
step('predict_next_deadline_ms', () => t.predict_next_deadline_ms())
advance(250)
step('predict_reconcile', () => t.predict_reconcile())
step('predict_overlay after', () => t.predict_overlay())
step('predict_line_submit', () => t.predict_line_submit())
step('predict_reset', () => t.predict_reset())
frame('predicted', t)

// look and chrome
step('set_chrome', () => t.set_chrome(6, 18))
step('set_line_height', () => t.set_line_height(1.2))
step('set_minimum_contrast', () => t.set_minimum_contrast(4.5))
step('set_palette_color', () => t.set_palette_color(1, 200, 30, 40))
step('set_default_background', () => t.set_default_background(5, 6, 7))
step('set_color_scheme', () => t.set_color_scheme(false))
step('set_forced_colors', () => t.set_forced_colors(true, 0xffffff, 0, 0x0000ff, 0xffffff, 0xeeeeee))
step('set_font_features', () => t.set_font_features('-liga,+calt'))
step('set_cursor_opacity', () => t.set_cursor_opacity(0.5))
step('set_background_opacity', () => t.set_background_opacity(0.9))
frame('restyled', t)
state('restyled', t)

// resize and reflow
step('resize', () => t.resize(30, 50))
state('resized', t)
for (let i = 0; i < 4; i++) step(`pump_reflow ${i}`, () => t.pump_reflow())
step('drain_scrollback_backlog', () => t.drain_scrollback_backlog(1000))
frame('resized', t)
step('set_px', () => t.set_px(13.5))
step('set_cell_pixel_size', () => t.set_cell_pixel_size(9, 19))
frame('repx', t)

// fonts
step('set_fallback_font bad', () => t.set_fallback_font(new Uint8Array(16)))
step('add_fallback_font bad', () => t.add_fallback_font(new Uint8Array(0)))
step('set_fallback_font_registered bad', () => t.set_fallback_font_registered(77))
step('set_bold_font_registered', () => t.set_bold_font_registered(0))

// bench: restore (serialize -> fresh engine -> replay)
let payload
step('serialize', () => {
  payload = t.serialize(undefined)
  return payload
})
step('serialize 10', () => t.serialize(10))
step('serialize_scrollback', () => t.serialize_scrollback(null))
{
  let u
  step('new_registered', () => {
    u = mod.AtermTerminal.new_registered(30, 50, 0, 13.5, ...THEME)
    return u
  })
  step('restore', () => u.process_str(payload))
  step('messages_restore', () => u.messages_restore(typeof log === 'string' ? log : ''))
  step('messages_restore again', () => u.messages_restore('not a log line\nnor this'))
  step('messages_read restored', () => u.messages_read(''))
  frame('restored', u)
  state('restored', u)
  step('free restored', () => u.free())
}
state('end', t)
step('free', () => t.free())

// --- check or record -------------------------------------------------------
if (mode === 'record') {
  // One answer per line, so a re-record reads as a diff of the calls it moved.
  writeFileSync(answersPath, `{"answers": [\n${answers.map((a) => JSON.stringify(a)).join(',\n')}\n]}\n`)
  console.error(`wasm-replay: recorded ${answers.length} answers (${flavor})`)
  process.exit(0)
}
const want = JSON.parse(readFileSync(answersPath, 'utf8')).answers
let failures = 0
const n = Math.max(want.length, answers.length)
for (let i = 0; i < n; i++) {
  const got = JSON.stringify(answers[i])
  const exp = JSON.stringify(want[i])
  if (got !== exp) {
    failures++
    if (failures <= 10) {
      console.error(`wasm-replay: FAIL ${flavor} #${i}: got ${got?.slice(0, 400)}`)
      console.error(`wasm-replay:      want ${exp?.slice(0, 400)}`)
    }
  }
}
console.error(
  `wasm-replay: ${flavor}: ${answers.length} answers, ${failures === 0 ? 'all as recorded' : `${failures} differ`}`
)
process.exit(failures === 0 ? 0 : 1)
