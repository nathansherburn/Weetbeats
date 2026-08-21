/*
 * Weetbeats front end.
 *
 * Two views, one workspace. The song sits underneath: bars across the top, patterns down
 * the left, and any number of patterns in the same bar so a kick pattern, a hat pattern and
 * a snare pattern add up to a beat. A pattern opens on top of the song like a window, with
 * its step grid and a close button; click the pattern again, or hit escape, and you are back
 * at the song. The patterns panel never goes away, because it is how you get from one
 * pattern to the next.
 *
 * It holds a copy of the project so a click can light a box up straight away, but Rust owns
 * it: every change goes there too, and the audio thread hears about it from Rust.
 *
 * Instruments come in through the system file picker, and opening and saving are in the File
 * menu, both of which Rust drives. There is no file browser in here, and no HTML5 drag and
 * drop: the webview's own drag handler swallows those events before the page sees them, so
 * dropping a file is a native event instead.
 *
 * The grids are canvases rather than a box per step. Sixteen steps by a few tracks would
 * survive as elements, but the piano roll in stage 4 will not, and this is the same drawing
 * code either way.
 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const CELL = 42; // width of one step in the pattern editor
const GAP = 4; // gap between steps
const ROW = 46; // must match --row in the stylesheet
const HEADERS = 296; // the instrument column: must match --headers
const LANE = 34; // one pattern's row, in the panel and in the song: must match --lane
const HEAD = 30; // the strip along the top of both views: must match --head
const STEPS_PER_BEAT = 4; // sixteenth notes
const STEPS_PER_BAR = 16; // a bar of the song, and the length of a new pattern
const MAX_STEPS = 64; // as far as the engine will play

// The song is drawn in real time, so a bar is always the same width and a pattern painted
// across two bars is twice as wide as one painted across one.
const SONG_STEP = 4;
const BAR_PX = STEPS_PER_BAR * SONG_STEP;

// Pixels of drag per step of a number field.
const DRAG_PIXELS = 3;

const state = {
  bpm: 120,
  tracks: [], // { id, name, gain, muted, soloed, peaks }
  patterns: [], // { id, name, steps, notes: Map<trackId, Set<step>> }
  song: [], // one entry per bar: the pattern ids playing in it
  open: null, // the pattern the editor has, or null for the song view
  playing: false,
  step: 0,
  progress: 0,
  bar: 0, // which bar of the song is playing
  sounding: 0, // patterns making noise right now, one bit each
  needsDraw: true,
};

const el = {};
for (const id of [
  "play", "bpm", "meterFill", "status", "projectName", "songMode", "addPattern",
  "patternList", "song",
  "songScroll", "scrubber", "lanes", "songHint", "editor", "editorScroll", "closePattern",
  "add", "addBig", "steps", "fewerSteps", "moreSteps", "trackHeaders", "grid", "ruler",
  "empty",
]) {
  el[id] = document.getElementById(id);
}

const css = getComputedStyle(document.documentElement);
const colour = (name, fallback) =>
  (css.getPropertyValue(name) || fallback).trim() || fallback;

const PALETTE = {
  line: colour("--line", "#2e2839"),
  dim: colour("--dim", "#8b8399"),
  accent: colour("--accent", "#ff4d87"),
  lit: colour("--lit", "#ffd75e"),
};

/*
 * The webview's own right click menu offers a page reload and nothing else of use, and it
 * gets in the way of right clicking to rub a box out.
 */
window.addEventListener("contextmenu", (e) => e.preventDefault());

// --- start up -------------------------------------------------------------

async function boot() {
  applyStartup(await invoke("startup"));
  requestAnimationFrame(tick);
}

/*
 * Everything the front end knows, from Rust. Also runs when another project is opened, so
 * it has to replace the lot rather than add to it.
 */
function applyStartup(startup) {
  const project = startup.project;
  state.bpm = project.bpm;
  const waveforms = new Map((startup.waveforms ?? []).map((w) => [w.track, w.peaks]));
  state.tracks = project.tracks.map((track) => ({
    ...track,
    peaks: waveforms.get(track.id) ?? [],
  }));
  state.patterns = project.patterns.map(readPattern);
  state.song = project.song.map((bar) => bar.slice());

  el.bpm.value = String(Math.round(state.bpm));
  el.projectName.textContent = startup.name;
  el.projectName.title = startup.folder;

  // Land on the song when there is one, and in the first pattern when there is not: a new
  // project has nothing to arrange yet, so the grid is the only place worth being.
  if (state.song.length) {
    closePattern();
  } else {
    openPattern(state.patterns[0]?.id ?? 0);
  }
  drawPatternPanel();
  drawTrackHeaders();
  resize();
  if (startup.message) showError(startup.message);
}

/* Notes arrive as a lane per track. A Set of steps is what the grid wants. */
function readPattern(pattern) {
  const notes = new Map();
  for (const lane of pattern.lanes) {
    notes.set(lane.track, new Set(lane.notes.map((note) => note.step)));
  }
  return { id: pattern.id, name: pattern.name, steps: pattern.steps, notes };
}

function patternById(id) {
  return state.patterns.find((pattern) => pattern.id === id) ?? null;
}

function openPatternNow() {
  return state.open === null ? null : patternById(state.open);
}

function stepsOf(id) {
  return patternById(id)?.steps ?? STEPS_PER_BAR;
}

/* The steps a track has ticked in a pattern, made on the spot if it has none yet. */
function notesFor(pattern, track) {
  let set = pattern.notes.get(track);
  if (!set) {
    set = new Set();
    pattern.notes.set(track, set);
  }
  return set;
}

// --- the two views --------------------------------------------------------

function openPattern(id) {
  if (patternById(id) === null) return;
  state.open = id;
  el.editor.classList.remove("hidden");
  el.song.classList.add("hidden");
  el.steps.value = String(stepsOf(id));
  invoke("open_pattern", { id });
  drawPatternPanel();
  resize();
}

function closePattern() {
  state.open = null;
  el.editor.classList.add("hidden");
  el.song.classList.remove("hidden");
  invoke("close_pattern");
  drawPatternPanel();
  resize();
}

function togglePattern(id) {
  if (state.open === id) {
    closePattern();
  } else {
    openPattern(id);
  }
}

el.closePattern.addEventListener("click", closePattern);

/* The panel's heading is a way back to the song, for when no pattern is open to close. */
el.songMode.addEventListener("click", () => {
  if (state.open !== null) closePattern();
});

// --- the patterns panel ---------------------------------------------------

function drawPatternPanel() {
  el.patternList.replaceChildren(
    ...state.patterns.map((pattern) => {
      const row = document.createElement("div");
      row.className = "prow";
      row.dataset.id = String(pattern.id);

      const name = document.createElement("span");
      name.className = "pname";
      name.textContent = pattern.name;
      row.append(name);

      const len = document.createElement("span");
      len.className = "plen";
      len.textContent = String(pattern.steps);
      len.title = `${pattern.steps} steps`;
      row.append(len);

      const copy = document.createElement("button");
      copy.className = "tick dup";
      copy.textContent = "⧉";
      copy.title = "Duplicate this pattern";
      copy.addEventListener("click", (e) => {
        e.stopPropagation();
        rearrange("duplicate_pattern", { id: pattern.id }, (after) => {
          // The copy is the one after the original, and it is what you want to work on.
          const at = after.patterns.findIndex((p) => p.id === pattern.id);
          const copied = after.patterns[at + 1];
          if (copied) openPattern(copied.id);
        });
      });
      row.append(copy);

      const kill = document.createElement("button");
      kill.className = "tick kill";
      kill.textContent = "×";
      kill.title = "Delete this pattern";
      kill.addEventListener("click", (e) => {
        e.stopPropagation();
        rearrange("remove_pattern", { id: pattern.id }, () => {
          if (state.open === pattern.id) closePattern();
        });
      });
      row.append(kill);

      row.addEventListener("click", (e) => {
        // The second click of a double click is the start of a rename, not another toggle.
        // Renaming a pattern you just opened is a fine place to end up; toggling the view
        // twice under someone's cursor is not.
        if (e.detail > 1) return;
        togglePattern(pattern.id);
      });
      row.addEventListener("dblclick", () => startRename(row, pattern));
      return row;
    }),
  );
  markPatternRows();
  syncScroll(el.patternList, el.songScroll);
}

/*
 * Which pattern is open and which ones are making noise, without rebuilding the rows. What
 * is sounding changes every bar while a song plays, and replacing the panel a second at a
 * time would throw away a rename halfway through being typed.
 */
function markPatternRows() {
  el.songMode.classList.toggle("on", state.open === null);
  for (const row of el.patternList.children) {
    const id = Number(row.dataset.id);
    row.classList.toggle("open", state.open === id);
    row.classList.toggle("playing", state.playing && isSounding(id));
  }
}

/* The engine reports what is sounding as one bit per pattern. */
function isSounding(id) {
  return ((state.sounding >>> id) & 1) === 1;
}

/* Double click a name to change it. Enter or clicking away keeps it, escape drops it. */
function startRename(row, pattern) {
  const name = row.querySelector(".pname");
  if (!name) return;
  const input = document.createElement("input");
  input.className = "rename";
  input.type = "text";
  input.value = pattern.name;
  input.maxLength = 40;
  name.replaceWith(input);
  input.focus();
  input.select();

  let done = false;
  const finish = async (keep) => {
    if (done) return;
    done = true;
    if (keep && input.value !== pattern.name) {
      pattern.name = await invoke("rename_pattern", { id: pattern.id, name: input.value });
    }
    drawPatternPanel();
    state.needsDraw = true;
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") finish(true);
    if (e.key === "Escape") {
      // Do not let escape out of here: everywhere else it closes the pattern.
      e.stopPropagation();
      finish(false);
    }
  });
  input.addEventListener("blur", () => finish(true));
}

el.addPattern.addEventListener("click", () => {
  rearrange("add_pattern", {}, (after) => {
    // A new pattern is empty, so there is nowhere to be but in it.
    const made = after.patterns[after.patterns.length - 1];
    if (made) openPattern(made.id);
  });
});

/*
 * Anything that adds, copies or deletes a pattern gets the patterns and the song back
 * whole. Cheaper to redraw from that than to work out what moved.
 */
async function rearrange(command, args, then) {
  try {
    const after = await invoke(command, args);
    state.patterns = after.patterns.map(readPattern);
    state.song = after.song.map((bar) => bar.slice());
    if (then) then(after);
    if (state.open !== null && patternById(state.open) === null) closePattern();
    drawPatternPanel();
    resize();
  } catch (e) {
    showError(e);
  }
}

/* The panel and the song lanes are one list of patterns, so they scroll as one. */
let syncing = false;

function syncScroll(from, to) {
  if (syncing || !from || !to) return;
  syncing = true;
  to.scrollTop = from.scrollTop;
  syncing = false;
}

el.patternList.addEventListener("scroll", () => syncScroll(el.patternList, el.songScroll));
el.songScroll.addEventListener("scroll", () => syncScroll(el.songScroll, el.patternList));

// --- numbers you can drag, scroll or type ---------------------------------

/*
 * A field for a number, worked the way a number wants to be worked: drag it up and down,
 * scroll it, or click it and type. A slider cannot be typed into and takes four times the
 * room; a stepper is a lot of clicking to get from 120 to 174.
 */
function numberField(input, { min, max, onChange }) {
  const read = () => {
    const value = Number(input.value);
    return Number.isFinite(value) ? value : min;
  };
  const commit = (value) => {
    const settled = Math.max(min, Math.min(max, Math.round(value)));
    input.value = String(settled);
    onChange(settled);
  };

  let dragging = false;
  let startY = 0;
  let startValue = 0;
  let moved = 0;

  input.addEventListener("pointerdown", (e) => {
    // Already typing in it, so a click is a click.
    if (document.activeElement === input) return;
    // No caret, no text selection, no focus: this press is a drag until proven otherwise.
    e.preventDefault();
    dragging = true;
    moved = 0;
    startY = e.clientY;
    startValue = read();
    input.setPointerCapture(e.pointerId);
  });

  input.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const dy = startY - e.clientY;
    moved = Math.max(moved, Math.abs(dy));
    commit(startValue + Math.round(dy / DRAG_PIXELS));
  });

  const release = () => {
    if (!dragging) return;
    dragging = false;
    // A press that went nowhere is a click, and a click means "let me type".
    if (moved < DRAG_PIXELS) {
      input.focus();
      input.select();
    }
  };
  input.addEventListener("pointerup", release);
  input.addEventListener("pointercancel", release);

  input.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      commit(read() - Math.sign(e.deltaY));
    },
    { passive: false },
  );

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") input.blur();
    if (e.key === "ArrowUp") {
      e.preventDefault();
      commit(read() + 1);
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      commit(read() - 1);
    }
  });
  input.addEventListener("change", () => commit(read()));
  input.addEventListener("blur", () => commit(read()));
}

// --- pattern length -------------------------------------------------------

async function setSteps(steps) {
  const pattern = openPatternNow();
  if (!pattern) return;
  const asked = Math.max(1, Math.min(MAX_STEPS, Math.round(steps) || 1));
  if (asked === pattern.steps) return;
  const actual = await invoke("set_pattern_steps", { id: pattern.id, steps: asked });
  pattern.steps = actual;
  // Rust drops the notes that fall off the end, so drop them here too rather than keeping
  // boxes ticked that are not there any more.
  for (const set of pattern.notes.values()) {
    for (const step of [...set]) {
      if (step >= actual) set.delete(step);
    }
  }
  el.steps.value = String(actual);
  drawPatternPanel();
  resize();
}

numberField(el.steps, { min: 1, max: MAX_STEPS, onChange: setSteps });
el.fewerSteps.addEventListener("click", () => setSteps(stepsOf(state.open) - 1));
el.moreSteps.addEventListener("click", () => setSteps(stepsOf(state.open) + 1));

// --- instruments ----------------------------------------------------------

/*
 * One trip to the picker can bring back a whole kit, so this takes a list. Rust has
 * already copied the samples into the project folder, made the tracks and told the audio
 * thread about them; all that is left is to draw the rows.
 */
let adding = false;

async function addInstruments(command, args) {
  // One dialog at a time. Without this a double click opens two pickers.
  if (adding) return;
  adding = true;
  el.add.disabled = true;
  el.addBig.disabled = true;
  try {
    const added = await invoke(command, args);
    for (const item of added.tracks) {
      state.tracks.push({ ...item.track, peaks: item.peaks });
    }
    if (added.tracks.length) {
      drawTrackHeaders();
      resize();
    }
    if (added.failed.length) {
      // One line is enough; the rest would just scroll past.
      const more = added.failed.length > 1 ? ` (and ${added.failed.length - 1} more)` : "";
      showError(added.failed[0] + more);
    }
  } catch (e) {
    showError(e);
  } finally {
    adding = false;
    el.add.disabled = false;
    el.addBig.disabled = false;
  }
}

el.add.addEventListener("click", () => addInstruments("add_instruments"));
el.addBig.addEventListener("click", () => addInstruments("add_instruments"));

function drawTrackHeaders() {
  el.empty.classList.toggle("hidden", state.tracks.length > 0);

  el.trackHeaders.replaceChildren(
    ...state.tracks.map((track) => {
      const row = document.createElement("div");
      row.className = "track";

      const name = document.createElement("div");
      name.className = "name";
      name.textContent = track.name;
      name.title = `${track.name} — click to hear it`;
      name.addEventListener("click", () => invoke("audition", { id: track.id }));
      row.append(name);

      const wave = document.createElement("canvas");
      wave.className = "wave";
      wave.width = 68;
      wave.height = 36;
      drawWaveform(wave, track.peaks);
      row.append(wave);

      const mute = toggle("M", "mute", track.muted, (on) => {
        track.muted = on;
        invoke("set_track_muted", { id: track.id, muted: on });
      });
      const solo = toggle("S", "solo", track.soloed, (on) => {
        track.soloed = on;
        invoke("set_track_soloed", { id: track.id, soloed: on });
      });

      const gain = document.createElement("input");
      gain.type = "range";
      gain.min = "0";
      gain.max = "120";
      gain.step = "1";
      gain.value = String(Math.round(track.gain * 100));
      gain.title = "Volume";
      gain.addEventListener("input", () => {
        track.gain = Number(gain.value) / 100;
        invoke("set_track_gain", { id: track.id, gain: track.gain });
      });

      const kill = document.createElement("button");
      kill.className = "tick kill";
      kill.textContent = "×";
      kill.title = "Delete this track";
      kill.addEventListener("click", () => removeTrack(track.id));

      row.append(mute, solo, gain, kill);
      return row;
    }),
  );
}

function toggle(label, extra, on, onChange) {
  const button = document.createElement("button");
  button.className = `tick ${extra}${on ? " on" : ""}`;
  button.textContent = label;
  button.title = extra;
  button.addEventListener("click", () => {
    const next = !button.classList.contains("on");
    button.classList.toggle("on", next);
    onChange(next);
  });
  return button;
}

/* Deleting a track takes its notes out of every pattern, and its sample out of the folder. */
function removeTrack(id) {
  invoke("remove_track", { id });
  state.tracks = state.tracks.filter((track) => track.id !== id);
  for (const pattern of state.patterns) {
    pattern.notes.delete(id);
  }
  drawTrackHeaders();
  resize();
}

function drawWaveform(canvas, peaks) {
  const ctx = canvas.getContext("2d");
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  if (!peaks || !peaks.length) return;
  ctx.fillStyle = PALETTE.dim;
  const step = w / peaks.length;
  for (let i = 0; i < peaks.length; i++) {
    const bar = Math.max(1, peaks[i] * h);
    ctx.fillRect(i * step, (h - bar) / 2, Math.max(1, step - 0.5), bar);
  }
}

// --- dropping a file on the window ------------------------------------------

/*
 * The webview installs its own drag handler, which is why HTML5 dragstart and drop never
 * fire in here. The native events it sends instead carry real filesystem paths, which is
 * better anyway: a path can be handed straight to Rust, which copies the file into the
 * project folder.
 */
listen("tauri://drag-enter", () => document.body.classList.add("drop-target"));
listen("tauri://drag-leave", () => document.body.classList.remove("drop-target"));
listen("tauri://drag-drop", (event) => {
  document.body.classList.remove("drop-target");
  const paths = event.payload?.paths ?? [];
  if (paths.length) {
    addInstruments("add_dropped", { paths });
  }
});

// --- what the File menu did -------------------------------------------------

/*
 * Opening and saving are in the menu bar, which is Rust's. A menu item has nothing to return
 * to, so Rust tells us what happened instead.
 */
listen("project", (event) => applyStartup(event.payload));
listen("saved", (event) => showSaved(event.payload));
listen("trouble", (event) => showError(event.payload));

// --- sizing the canvases --------------------------------------------------

function size(canvas, w, h) {
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  canvas.style.width = `${w}px`;
  canvas.style.height = `${h}px`;
  canvas.getContext("2d").setTransform(dpr, 0, 0, dpr, 0, 0);
}

/* How wide a canvas ended up, which is the width its drawing code works in. */
function drawnWidth(canvas) {
  return parseInt(canvas.style.width, 10) || 0;
}

function gridWidth() {
  return stepsOf(state.open) * CELL;
}

function gridHeight() {
  return Math.max(ROW, state.tracks.length * ROW);
}

/*
 * Bars to draw: the song, one spare on the end, and enough to fill the window either way.
 * Empty bars are as good a place to put a pattern as any, so they are all drawn and all
 * clickable.
 */
function songBars() {
  const room = Math.ceil(el.songScroll.clientWidth / BAR_PX);
  return Math.max(state.song.length + 1, room);
}

function resize() {
  size(el.grid, gridWidth(), gridHeight());
  size(el.ruler, gridWidth(), HEAD - 1);
  size(el.lanes, songBars() * BAR_PX, Math.max(LANE, state.patterns.length * LANE));
  size(el.scrubber, songBars() * BAR_PX, HEAD - 1);
  el.songHint.classList.toggle("hidden", state.song.length > 0);
  state.needsDraw = true;
  drawRuler();
}

/* The song got longer or shorter, so only resize when it actually changed shape. */
function songChanged() {
  if (drawnWidth(el.lanes) !== songBars() * BAR_PX) {
    resize();
  } else {
    el.songHint.classList.toggle("hidden", state.song.length > 0);
    state.needsDraw = true;
  }
}

window.addEventListener("resize", resize);

// --- the pattern editor ---------------------------------------------------

function drawRuler() {
  const ctx = el.ruler.getContext("2d");
  const steps = stepsOf(state.open);
  ctx.clearRect(0, 0, drawnWidth(el.ruler), HEAD);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";
  for (let step = 0; step < steps; step++) {
    const onBeat = step % STEPS_PER_BEAT === 0;
    ctx.fillStyle = onBeat ? PALETTE.dim : PALETTE.line;
    if (onBeat) {
      ctx.fillText(String(step / STEPS_PER_BEAT + 1), step * CELL + GAP + 2, 15);
    } else {
      ctx.fillRect(step * CELL + GAP, 14, 3, 1);
    }
  }
}

function drawGrid() {
  const pattern = openPatternNow();
  if (!pattern) return;
  const ctx = el.grid.getContext("2d");
  const width = drawnWidth(el.grid);
  const height = gridHeight();
  ctx.clearRect(0, 0, width, height);

  // The column the playhead is in, drawn under the steps so lit boxes stay readable.
  if (state.playing) {
    ctx.fillStyle = "rgba(255,215,94,0.09)";
    ctx.fillRect(state.step * CELL, 0, CELL, height);
  }

  for (let row = 0; row < state.tracks.length; row++) {
    const track = state.tracks[row];
    const steps = pattern.notes.get(track.id);
    const y = row * ROW;

    for (let step = 0; step < pattern.steps; step++) {
      const x = step * CELL + GAP;
      const w = CELL - GAP * 2;
      const h = ROW - GAP * 2 - 1;
      const on = steps ? steps.has(step) : false;
      const onBeat = step % STEPS_PER_BEAT === 0;

      if (on) {
        const live = state.playing && state.step === step;
        ctx.fillStyle = live ? PALETTE.lit : PALETTE.accent;
      } else {
        ctx.fillStyle = onBeat ? "#272132" : "#201c29";
      }
      roundRect(ctx, x, y + GAP, w, h, 5);
      ctx.fill();
    }
  }
}

function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

// --- painting steps -------------------------------------------------------

let painting = null; // the value being painted, so a drag keeps doing one thing

function cellAt(event) {
  const pattern = openPatternNow();
  if (!pattern) return null;
  const rect = el.grid.getBoundingClientRect();
  const step = Math.floor((event.clientX - rect.left) / CELL);
  const row = Math.floor((event.clientY - rect.top) / ROW);
  if (step < 0 || step >= pattern.steps) return null;
  if (row < 0 || row >= state.tracks.length) return null;
  return { pattern, track: state.tracks[row].id, step };
}

function paint(cell, on) {
  if (!cell) return;
  const steps = notesFor(cell.pattern, cell.track);
  if (steps.has(cell.step) === on) return;
  if (on) {
    steps.add(cell.step);
  } else {
    steps.delete(cell.step);
  }
  state.needsDraw = true;
  invoke("set_step", {
    pattern: cell.pattern.id,
    track: cell.track,
    step: cell.step,
    on,
  }).then((actual) => {
    // Rust has the final say: a track that is full will not take another note.
    if (actual !== on) {
      if (actual) steps.add(cell.step);
      else steps.delete(cell.step);
      state.needsDraw = true;
    }
  });
}

/* True when a press is the right button, which always rubs out rather than draws. */
function erasing(event) {
  return event.button === 2 || (event.buttons & 2) === 2;
}

el.grid.addEventListener("pointerdown", (e) => {
  const cell = cellAt(e);
  if (!cell) return;
  // Drag across boxes to paint, like FL Studio: what the first box becomes is what the
  // rest become, so a drag never toggles boxes back and forth under your finger. The right
  // button always rubs out, which saves aiming at the box you meant to remove.
  painting = erasing(e) ? false : !notesFor(cell.pattern, cell.track).has(cell.step);
  el.grid.setPointerCapture(e.pointerId);
  paint(cell, painting);
});

el.grid.addEventListener("pointermove", (e) => {
  if (painting === null) return;
  paint(cellAt(e), painting);
});

const stopPainting = () => {
  painting = null;
};
el.grid.addEventListener("pointerup", stopPainting);
el.grid.addEventListener("pointercancel", stopPainting);

// --- the song -------------------------------------------------------------

function barPatterns(bar) {
  return state.song[bar] ?? [];
}

function barHas(bar, id) {
  return barPatterns(bar).includes(id);
}

/*
 * Put a pattern in a bar, or take it out, the same way Rust does: bars before it are filled
 * in as empty so a pattern can be dropped in at bar twelve, and empty bars on the end are
 * not part of the song.
 */
function setBar(bar, id, on) {
  if (on) {
    while (state.song.length <= bar) state.song.push([]);
    if (!state.song[bar].includes(id)) {
      state.song[bar].push(id);
      state.song[bar].sort((a, b) => a - b);
    }
  } else if (bar < state.song.length) {
    state.song[bar] = state.song[bar].filter((p) => p !== id);
    while (state.song.length && state.song[state.song.length - 1].length === 0) {
      state.song.pop();
    }
  }
}

/* Where the playhead is across the song, in pixels. */
function songPlayheadX() {
  const through = (state.step + state.progress) / STEPS_PER_BAR;
  return (state.bar + Math.min(1, through)) * BAR_PX;
}

function drawSong() {
  drawScrubber();
  drawLanes();
}

function drawScrubber() {
  const ctx = el.scrubber.getContext("2d");
  const width = drawnWidth(el.scrubber);
  const height = HEAD - 1;
  ctx.clearRect(0, 0, width, height);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";

  const bars = songBars();
  for (let bar = 0; bar < bars; bar++) {
    // Every fourth bar gets a brighter mark: that is where a phrase usually turns over.
    const phrase = bar % 4 === 0;
    ctx.fillStyle = PALETTE.line;
    ctx.fillRect(bar * BAR_PX, phrase ? 6 : 10, 1, height - 6);
    ctx.fillStyle = state.playing && bar === state.bar ? PALETTE.lit : PALETTE.dim;
    if (phrase || bars < 40) {
      ctx.fillText(String(bar + 1), bar * BAR_PX + 5, 15);
    }
  }

  if (state.playing) {
    const x = songPlayheadX();
    ctx.fillStyle = PALETTE.lit;
    ctx.beginPath();
    ctx.moveTo(x - 4, 2);
    ctx.lineTo(x + 4, 2);
    ctx.lineTo(x, 9);
    ctx.closePath();
    ctx.fill();
  }
}

function drawLanes() {
  const ctx = el.lanes.getContext("2d");
  const width = drawnWidth(el.lanes);
  const height = Math.max(LANE, state.patterns.length * LANE);
  const bars = songBars();
  ctx.clearRect(0, 0, width, height);
  ctx.font = "11px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";

  for (let row = 0; row < state.patterns.length; row++) {
    const pattern = state.patterns[row];
    const y = row * LANE;

    // The empty grid first, so a bar you could paint into looks like one.
    for (let bar = 0; bar < bars; bar++) {
      if (barHas(bar, pattern.id)) continue;
      ctx.fillStyle = bar % 4 === 0 ? "#221e2c" : "#1e1a27";
      roundRect(ctx, bar * BAR_PX + 2, y + 3, BAR_PX - 4, LANE - 7, 5);
      ctx.fill();
    }

    // Then a block per run of bars. A run is one block rather than one a bar because the
    // pattern plays straight through it: that is what makes a pattern longer than a bar
    // work, and the ticks inside show where it comes round again.
    for (let bar = 0; bar < bars; bar++) {
      if (!barHas(bar, pattern.id)) continue;
      let end = bar;
      while (end + 1 < bars && barHas(end + 1, pattern.id)) end += 1;

      const x = bar * BAR_PX + 2;
      const w = (end - bar + 1) * BAR_PX - 4;
      const h = LANE - 7;
      const live = state.playing && isSounding(pattern.id) && state.bar >= bar && state.bar <= end;
      ctx.fillStyle = live ? PALETTE.lit : PALETTE.accent;
      roundRect(ctx, x, y + 3, w, h, 5);
      ctx.fill();

      const room = (end - bar + 1) * STEPS_PER_BAR;
      ctx.save();
      roundRect(ctx, x, y + 3, w, h, 5);
      ctx.clip();
      // Where the pattern comes round again inside the run.
      ctx.fillStyle = "rgba(0,0,0,0.22)";
      for (let step = pattern.steps; step < room; step += pattern.steps) {
        ctx.fillRect(bar * BAR_PX + step * SONG_STEP, y + 3, 1, h);
      }
      // A pattern with more in it than the run has room for is cut off at the end, and the
      // notch is the only thing that would tell you.
      if (pattern.steps > room) {
        ctx.fillStyle = "rgba(0,0,0,0.4)";
        ctx.fillRect(x + w - 5, y + 3, 5, h);
      }
      if (w > 26) {
        ctx.fillStyle = "#22101a";
        ctx.fillText(pattern.name, x + 6, y + LANE / 2 - 1);
      }
      ctx.restore();
      bar = end;
    }
  }

  if (state.playing) {
    ctx.fillStyle = PALETTE.lit;
    ctx.fillRect(songPlayheadX(), 0, 1, height);
  }
}

/* Paint patterns into the song the same way you paint boxes into a pattern. */
let painting_song = null;

function songCellAt(event) {
  const rect = el.lanes.getBoundingClientRect();
  const bar = Math.floor((event.clientX - rect.left) / BAR_PX);
  const row = Math.floor((event.clientY - rect.top) / LANE);
  if (bar < 0 || bar >= songBars()) return null;
  if (row < 0 || row >= state.patterns.length) return null;
  return { bar, id: state.patterns[row].id };
}

function paintSong(cell, on) {
  if (!cell) return;
  if (barHas(cell.bar, cell.id) === on) return;
  setBar(cell.bar, cell.id, on);
  songChanged();
  invoke("set_song_bar", { bar: cell.bar, pattern: cell.id, on }).then((actual) => {
    if (actual !== on) {
      setBar(cell.bar, cell.id, actual);
      songChanged();
    }
  });
}

el.lanes.addEventListener("pointerdown", (e) => {
  const cell = songCellAt(e);
  if (!cell) return;
  painting_song = erasing(e) ? false : !barHas(cell.bar, cell.id);
  el.lanes.setPointerCapture(e.pointerId);
  paintSong(cell, painting_song);
});

el.lanes.addEventListener("pointermove", (e) => {
  if (painting_song === null) return;
  paintSong(songCellAt(e), painting_song);
});

const stopPaintingSong = () => {
  painting_song = null;
};
el.lanes.addEventListener("pointerup", stopPaintingSong);
el.lanes.addEventListener("pointercancel", stopPaintingSong);

// --- the scrubber ---------------------------------------------------------

function barAt(event, canvas) {
  const rect = canvas.getBoundingClientRect();
  const bar = Math.floor((event.clientX - rect.left) / BAR_PX);
  return bar >= 0 && bar < songBars() ? bar : null;
}

/* Drag along the scrubber to play from a bar. */
function scrub(event, force) {
  if (!state.song.length) return;
  const bar = barAt(event, el.scrubber);
  if (bar === null) return;
  const wanted = Math.min(bar, state.song.length - 1);
  // Dragging over the bar it is already in is not worth a seek; pressing on it is, because
  // that means "play this from the top".
  if (wanted === state.bar && !force) return;
  state.bar = wanted;
  state.step = 0;
  state.needsDraw = true;
  invoke("seek_song", { bar: wanted });
}

let scrubbing = false;

el.scrubber.addEventListener("pointerdown", async (e) => {
  // The right button takes the bar out of the song, closing the gap it leaves.
  if (erasing(e)) {
    const bar = barAt(e, el.scrubber);
    if (bar === null || bar >= state.song.length) return;
    try {
      state.song = (await invoke("remove_song_bar", { bar })).map((one) => one.slice());
      songChanged();
    } catch (err) {
      showError(err);
    }
    return;
  }
  scrubbing = true;
  el.scrubber.setPointerCapture(e.pointerId);
  scrub(e, true);
});

el.scrubber.addEventListener("pointermove", (e) => {
  if (scrubbing) scrub(e, false);
});

const stopScrubbing = () => {
  scrubbing = false;
};
el.scrubber.addEventListener("pointerup", stopScrubbing);
el.scrubber.addEventListener("pointercancel", stopScrubbing);

// --- transport ------------------------------------------------------------

function setPlaying(playing) {
  state.playing = playing;
  showPlaying(playing);
  invoke("set_playing", { playing });
  state.needsDraw = true;
}

function showPlaying(playing) {
  el.play.classList.toggle("on", playing);
  el.play.querySelector(".glyph").innerHTML = playing ? "&#9632;" : "&#9654;";
}

el.play.addEventListener("click", () => setPlaying(!state.playing));

numberField(el.bpm, {
  min: 40,
  max: 240,
  onChange: async (bpm) => {
    state.bpm = await invoke("set_bpm", { bpm });
  },
});

window.addEventListener("keydown", (e) => {
  // Only a text field has any use for a space. A focused number does not, and having it
  // swallow the play key after you nudge the tempo is maddening.
  const typing = e.target.matches("input:not(.number), textarea, [contenteditable]");

  if (e.code === "Space" && !typing) {
    e.preventDefault();
    setPlaying(!state.playing);
  }
  if (e.code === "Escape") {
    // Escape means "out of here": out of whatever field you are in, then out of the
    // pattern, and only then the panic button. The rename box keeps escape for itself and
    // stops it reaching here, because there it means "forget the new name".
    if (e.target instanceof HTMLElement) e.target.blur();
    if (state.open !== null) {
      closePattern();
    } else {
      invoke("panic_stop");
      setPlaying(false);
    }
  }
});

// --- the playhead ---------------------------------------------------------

/*
 * Polled, not pushed. Tauri's messaging is not real time, so an event per step would
 * arrive in clumps and the playhead would judder. The audio thread writes its position
 * into an atomic and this reads it whenever the browser is about to paint.
 */
let lastPoll = 0;

async function tick(now) {
  requestAnimationFrame(tick);

  // No point asking sixty times a second when nothing is moving.
  const interval = state.playing ? 0 : 200;
  if (now - lastPoll >= interval) {
    lastPoll = now;
    try {
      const p = await invoke("playhead");
      const wasSounding = state.sounding;
      state.step = p.step;
      state.progress = p.progress;
      state.bar = p.bar;
      state.sounding = p.patterns;
      if (p.playing !== state.playing) {
        state.playing = p.playing;
        showPlaying(p.playing);
        markPatternRows();
      } else if (state.playing && wasSounding !== state.sounding) {
        // Which patterns are making the noise is worth showing in the panel.
        markPatternRows();
      }
      // While it plays the playhead moves every frame, so there is always something to draw.
      if (state.playing) state.needsDraw = true;
      el.meterFill.style.width = `${Math.min(100, p.peak * 100)}%`;
      if (p.saveError) showError(`could not save: ${p.saveError}`);
      else if (p.streamErrors > 0) showWarning(`${p.streamErrors} audio dropouts`);
    } catch {
      // A failed poll is not worth a dialog; the next one will do.
    }
  }

  if (state.needsDraw) {
    state.needsDraw = false;
    if (state.open === null) drawSong();
    else drawGrid();
  }
}

// --- the status line ------------------------------------------------------

/*
 * Empty unless there is something to say. What the device is doing is not something to say:
 * it never changes, and it was in the way of the things that do.
 */
let noticeUntil = 0;

function note(html, holdFor) {
  el.status.innerHTML = html;
  noticeUntil = performance.now() + holdFor;
}

function showError(e) {
  note(`<span class="warn">${escapeText(e)}</span>`, 5000);
}

/* Something the app is unhappy about that is not a one-off event. */
function showWarning(text) {
  if (performance.now() < noticeUntil) return;
  note(`<span class="warn">${escapeText(text)}</span>`, 1000);
}

function showSaved(name) {
  note(`<span class="ok">saved ${escapeText(name)}</span>`, 1500);
}

function escapeText(value) {
  const box = document.createElement("span");
  box.textContent = String(value);
  return box.innerHTML;
}

/* Clear a notice once it has had its moment. */
setInterval(() => {
  if (el.status.innerHTML && performance.now() >= noticeUntil) {
    el.status.innerHTML = "";
  }
}, 250);

boot();
