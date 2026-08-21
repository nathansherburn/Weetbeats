/*
 * Weetbeats front end.
 *
 * Two views, one workspace. The song is patterns down the left and a scrubber across the
 * top, where one slot holds one whole pattern, however long that pattern is. Click a
 * pattern and the workspace becomes the step grid for it; click it again, or hit escape,
 * and you are back at the song. The patterns panel never goes away, because it is how you
 * get from one pattern to the next.
 *
 * It holds a copy of the project so a click can light a box up straight away, but Rust owns
 * it: every change goes there too, and the audio thread hears about it from Rust.
 *
 * Instruments come in through the system file picker, which Rust opens. There is no file
 * browser in here, and no HTML5 drag and drop: the webview's own drag handler swallows
 * those events before the page sees them, so dropping a file is a native event instead.
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
const LANE = 34; // one pattern's row, in the panel and in the song: must match --lane
const HEAD = 30; // the strip along the top of both views: must match --head
const STEPS_PER_BAR = 4; // sixteenth notes, so four steps to a beat
const DEFAULT_STEPS = 16;
const MAX_STEPS = 64; // as far as the engine will play

// The song is drawn in real time, so a longer pattern is a longer block. Small enough that
// a whole song fits on screen, big enough to read a name in.
const SONG_STEP = 4;
const MIN_SLOT = 34;

const state = {
  bpm: 120,
  tracks: [], // { id, name, gain, muted, soloed, peaks }
  patterns: [], // { id, name, steps, notes: Map<trackId, Set<step>> }
  song: [], // pattern ids, in the order they play
  open: null, // the pattern the editor has, or null for the song view
  playing: false,
  step: 0,
  progress: 0,
  slot: 0, // where the song is up to
  sounding: null, // the pattern making noise right now
  audio: null,
  needsDraw: true,
};

const el = {};
for (const id of [
  "play", "bpm", "bpmValue", "master", "meterFill", "status", "projectName", "openProject",
  "saveAs", "addPattern", "patternList", "song", "songScroll", "scrubber", "lanes",
  "songHint", "editor", "add", "addBig", "steps", "fewerSteps", "moreSteps",
  "trackHeaders", "grid", "ruler", "empty",
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
  state.audio = startup.audio;
  state.bpm = project.bpm;
  const waveforms = new Map((startup.waveforms ?? []).map((w) => [w.track, w.peaks]));
  state.tracks = project.tracks.map((track) => ({
    ...track,
    peaks: waveforms.get(track.id) ?? [],
  }));
  state.patterns = project.patterns.map(readPattern);
  state.song = project.song.slice();

  el.bpm.value = String(Math.round(state.bpm));
  el.bpmValue.textContent = String(Math.round(state.bpm));
  el.master.value = String(Math.round(project.masterGain * 100));
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
  return patternById(id)?.steps ?? DEFAULT_STEPS;
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

// --- the patterns panel ---------------------------------------------------

function drawPatternPanel() {
  el.patternList.replaceChildren(
    ...state.patterns.map((pattern) => {
      const row = document.createElement("div");
      row.className = "prow";
      row.dataset.id = String(pattern.id);
      row.classList.toggle("open", state.open === pattern.id);
      row.classList.toggle("playing", state.playing && state.sounding === pattern.id);

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
          const copied = after.patterns[after.patterns.findIndex((p) => p.id === pattern.id) + 1];
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
  syncScroll(el.patternList, el.songScroll);
}

/*
 * Which pattern is open and which one is making noise, without rebuilding the rows. The
 * pattern that is sounding changes every slot while a song plays, and replacing the panel
 * a second at a time would throw away a rename halfway through being typed.
 */
function markPatternRows() {
  for (const row of el.patternList.children) {
    const id = Number(row.dataset.id);
    row.classList.toggle("open", state.open === id);
    row.classList.toggle("playing", state.playing && state.sounding === id);
  }
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
    drawSong();
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") finish(true);
    if (e.key === "Escape") {
      // Do not let escape out of here: in the pattern editor it closes the pattern.
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
    state.song = after.song.slice();
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

// --- pattern length -------------------------------------------------------

async function setSteps(steps) {
  const pattern = openPatternNow();
  if (!pattern) return;
  const asked = Math.max(1, Math.min(MAX_STEPS, Math.round(steps) || 1));
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

el.steps.addEventListener("change", () => setSteps(Number(el.steps.value)));
// The buttons move in bars, because that is how music is counted.
el.fewerSteps.addEventListener("click", () => setSteps(stepsOf(state.open) - STEPS_PER_BAR));
el.moreSteps.addEventListener("click", () => setSteps(stepsOf(state.open) + STEPS_PER_BAR));

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

function resize() {
  size(el.grid, gridWidth(), gridHeight());
  size(el.ruler, gridWidth(), HEAD - 1);
  size(el.lanes, songWidth(), Math.max(LANE, state.patterns.length * LANE));
  size(el.scrubber, songWidth(), HEAD - 1);
  el.songHint.classList.toggle("hidden", state.song.length > 0);
  state.needsDraw = true;
  drawRuler();
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
    const onBeat = step % STEPS_PER_BAR === 0;
    ctx.fillStyle = onBeat ? PALETTE.dim : PALETTE.line;
    if (onBeat) {
      ctx.fillText(String(step / STEPS_PER_BAR + 1), step * CELL + GAP + 2, 15);
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
      const onBeat = step % STEPS_PER_BAR === 0;

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

el.grid.addEventListener("pointerdown", (e) => {
  const cell = cellAt(e);
  if (!cell) return;
  // Drag across boxes to paint, like FL Studio: what the first box becomes is what the
  // rest become, so a drag never toggles boxes back and forth under your finger.
  painting = !notesFor(cell.pattern, cell.track).has(cell.step);
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

/*
 * Where every slot starts and how wide it is. A slot is one whole pattern, so its width is
 * that pattern's length: change a pattern's length and its block in the song changes with
 * it. The last one is empty and is how the song gets longer.
 */
function songLayout() {
  const slots = [];
  let x = 0;
  for (const id of state.song) {
    const w = Math.max(MIN_SLOT, stepsOf(id) * SONG_STEP);
    slots.push({ x, w, pattern: id });
    x += w;
  }
  slots.push({ x, w: DEFAULT_STEPS * SONG_STEP, pattern: null });
  return slots;
}

function songWidth() {
  const slots = songLayout();
  const last = slots[slots.length - 1];
  return last.x + last.w;
}

function slotAt(clientX, canvas) {
  const rect = canvas.getBoundingClientRect();
  const x = clientX - rect.left;
  const slots = songLayout();
  for (let i = 0; i < slots.length; i++) {
    if (x >= slots[i].x && x < slots[i].x + slots[i].w) return i;
  }
  return null;
}

/* Where the playhead is across the song, in pixels. */
function songPlayheadX() {
  const slots = songLayout();
  const slot = slots[Math.min(state.slot, slots.length - 1)];
  const pattern = patternById(state.sounding);
  if (!slot || !pattern) return null;
  const through = (state.step + state.progress) / Math.max(1, pattern.steps);
  return slot.x + slot.w * Math.min(1, through);
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

  const slots = songLayout();
  for (let i = 0; i < slots.length; i++) {
    const slot = slots[i];
    ctx.fillStyle = PALETTE.line;
    ctx.fillRect(slot.x, 8, 1, height - 8);
    if (slot.pattern !== null) {
      ctx.fillStyle = i === state.slot && state.playing ? PALETTE.lit : PALETTE.dim;
      ctx.fillText(String(i + 1), slot.x + 5, 15);
    }
  }

  if (state.playing) {
    const x = songPlayheadX();
    if (x !== null) {
      ctx.fillStyle = PALETTE.lit;
      ctx.beginPath();
      ctx.moveTo(x - 4, 2);
      ctx.lineTo(x + 4, 2);
      ctx.lineTo(x, 9);
      ctx.closePath();
      ctx.fill();
    }
  }
}

function drawLanes() {
  const ctx = el.lanes.getContext("2d");
  const width = drawnWidth(el.lanes);
  const height = Math.max(LANE, state.patterns.length * LANE);
  ctx.clearRect(0, 0, width, height);
  ctx.font = "11px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";

  const slots = songLayout();
  for (let row = 0; row < state.patterns.length; row++) {
    const pattern = state.patterns[row];
    const y = row * LANE;

    for (let i = 0; i < slots.length; i++) {
      const slot = slots[i];
      const here = slot.pattern === pattern.id;
      const x = slot.x + 2;
      const w = slot.w - 4;
      const h = LANE - 7;

      if (here) {
        const live = state.playing && state.slot === i;
        ctx.fillStyle = live ? PALETTE.lit : PALETTE.accent;
      } else {
        // The spare slot at the end reads as spare, not as an empty one you can fill in.
        ctx.fillStyle = slot.pattern === null ? "#1c1825" : "#211d2a";
      }
      roundRect(ctx, x, y + 3, w, h, 5);
      ctx.fill();

      if (here && w > 26) {
        ctx.save();
        roundRect(ctx, x, y + 3, w, h, 5);
        ctx.clip();
        ctx.fillStyle = "#22101a";
        ctx.fillText(pattern.name, x + 6, y + LANE / 2 - 1);
        ctx.restore();
      }
    }
  }

  if (state.playing) {
    const x = songPlayheadX();
    if (x !== null) {
      ctx.fillStyle = PALETTE.lit;
      ctx.fillRect(x, 0, 1, height);
    }
  }
}

/* Tick a slot to put a pattern in the song. Tick the same one again to take it out. */
el.lanes.addEventListener("pointerdown", async (e) => {
  const index = slotAt(e.clientX, el.lanes);
  const rect = el.lanes.getBoundingClientRect();
  const row = Math.floor((e.clientY - rect.top) / LANE);
  if (index === null || row < 0 || row >= state.patterns.length) return;
  const id = state.patterns[row].id;
  try {
    state.song = state.song[index] === id
      ? await invoke("clear_song_slot", { index })
      : await invoke("set_song_slot", { index, pattern: id });
    resize();
  } catch (err) {
    showError(err);
  }
});

/* Drag along the scrubber to play from a slot. */
function scrub(e, force) {
  if (!state.song.length) return;
  const index = slotAt(e.clientX, el.scrubber);
  if (index === null) return;
  const slot = Math.min(index, state.song.length - 1);
  // Dragging over the slot it is already in is not worth a seek; pressing on it is, because
  // that means "play this from the top".
  if (slot === state.slot && !force) return;
  state.slot = slot;
  state.step = 0;
  state.needsDraw = true;
  invoke("seek_song", { index: slot });
}

let scrubbing = false;
el.scrubber.addEventListener("pointerdown", (e) => {
  scrubbing = true;
  el.scrubber.setPointerCapture(e.pointerId);
  scrub(e, true);
});
el.scrubber.addEventListener("pointermove", (e) => {
  if (scrubbing) scrub(e);
});
const stopScrubbing = () => {
  scrubbing = false;
};
el.scrubber.addEventListener("pointerup", stopScrubbing);
el.scrubber.addEventListener("pointercancel", stopScrubbing);

// --- the project ----------------------------------------------------------

el.openProject.addEventListener("click", async () => {
  try {
    const opened = await invoke("open_project");
    if (opened) applyStartup(opened);
  } catch (e) {
    showError(e);
  }
});

el.saveAs.addEventListener("click", async () => {
  try {
    const saved = await invoke("save_project_as");
    if (saved) {
      el.projectName.textContent = saved.name;
      el.projectName.title = saved.folder;
      showSaved(saved.name);
    }
  } catch (e) {
    showError(e);
  }
});

async function saveNow() {
  try {
    showSaved(await invoke("save_project"));
  } catch (e) {
    showError(e);
  }
}

// --- transport ------------------------------------------------------------

function setPlaying(playing) {
  state.playing = playing;
  el.play.classList.toggle("on", playing);
  el.play.querySelector(".glyph").innerHTML = playing ? "&#9632;" : "&#9654;";
  invoke("set_playing", { playing });
  state.needsDraw = true;
}

el.play.addEventListener("click", () => setPlaying(!state.playing));

el.bpm.addEventListener("input", async () => {
  const value = Number(el.bpm.value);
  el.bpmValue.textContent = String(value);
  state.bpm = await invoke("set_bpm", { bpm: value });
});

el.master.addEventListener("input", () => {
  invoke("set_master_gain", { gain: Number(el.master.value) / 100 });
});

window.addEventListener("keydown", (e) => {
  // Only a text field has any use for a space. A focused slider does not, and having it
  // swallow the play key after you nudge the tempo is maddening.
  const typing = e.target.matches("input:not([type=range]), textarea, [contenteditable]");

  if (e.metaKey || e.ctrlKey) {
    if (e.key === "s") {
      e.preventDefault();
      if (e.shiftKey) el.saveAs.click();
      else saveNow();
    }
    if (e.key === "o") {
      e.preventDefault();
      el.openProject.click();
    }
    return;
  }

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
      state.slot = p.slot;
      state.sounding = p.pattern;
      if (p.playing !== state.playing) {
        state.playing = p.playing;
        el.play.classList.toggle("on", p.playing);
        el.play.querySelector(".glyph").innerHTML = p.playing ? "&#9632;" : "&#9654;";
        markPatternRows();
      } else if (state.playing && wasSounding !== state.sounding) {
        // Which pattern is making the noise is worth showing in the panel.
        markPatternRows();
      }
      // While it plays the playhead moves every frame, so there is always something to draw.
      if (state.playing) state.needsDraw = true;
      el.meterFill.style.width = `${Math.min(100, p.peak * 100)}%`;
      showStatus(p);
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

let noticeUntil = 0;

function showStatus(p) {
  if (performance.now() < noticeUntil) return;
  if (p.saveError) {
    el.status.innerHTML = `<span class="warn">could not save: ${escapeText(p.saveError)}</span>`;
    return;
  }
  const audio = state.audio
    ? `${Math.round(state.audio.sampleRate / 100) / 10}k · ${state.audio.channels}ch`
    : "";
  const voices = p.voices > 0 ? ` · ${p.voices} voices` : "";
  el.status.innerHTML =
    p.streamErrors > 0
      ? `<span class="warn">${p.streamErrors} audio dropouts</span>`
      : `${audio}${voices}`;
}

function showError(e) {
  el.status.innerHTML = `<span class="warn">${escapeText(e)}</span>`;
  noticeUntil = performance.now() + 4000;
}

function showSaved(name) {
  el.status.innerHTML = `<span class="ok">saved ${escapeText(name)}</span>`;
  noticeUntil = performance.now() + 1500;
}

function escapeText(value) {
  const box = document.createElement("span");
  box.textContent = String(value);
  return box.innerHTML;
}

boot();
