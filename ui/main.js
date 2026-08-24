/*
 * Weetbeats front end.
 *
 * Three views, one workspace. The song sits underneath: bars across the top, patterns down
 * the left, and blocks that overlap freely so a kick pattern, a hat pattern and a snare
 * pattern add up to a beat. A block starts wherever the snap puts it and is as long as you
 * drag it out to be; longer than its pattern, it repeats. A pattern opens on top of the
 * song like a window, with its step grid and a close button; click the pattern again, or hit
 * escape, and you are back at the song. The patterns panel never goes away, because it is
 * how you get from one pattern to the next.
 *
 * An instrument's row in a pattern is a small piano roll rather than a line of boxes, and
 * clicking it opens the roll proper. Both are the same lane of notes seen two ways, so the
 * keyboard button switches between them without anything being lost.
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

/*
 * One step of the pattern editor. Narrow on purpose: a bar of sixteen is what you are
 * usually looking at, and at forty two pixels a two bar pattern did not fit on a laptop.
 * The boxes are inset more from the top and bottom than from the sides, which keeps them
 * looking like boxes rather than tall thin slots now that they are narrower than the row.
 */
const CELL = 30; // width of one step in the pattern editor
const GAP = 3; // gap between steps
const BOX_INSET = 7; // and from the top and bottom of the row
const ROW = 46; // must match --row in the stylesheet
const HEADERS = 296; // the instrument column: must match --headers
const LANE = 34; // one pattern's row, in the panel and in the song: must match --lane
const HEAD = 34; // the strip along the top of every view: must match --head
/*
 * The band of the open pattern's colour along the bottom of that strip. The stylesheet
 * draws it, as a border on the ruler and on the chip beside it, so the two cannot land a
 * pixel apart; this is only how much of the strip the ruler has left to draw in. Must match
 * the border-bottom on .ruler, .corner and .head-chip.
 */
const HEAD_LINE = 3;
const STEPS_PER_BEAT = 4; // sixteenth notes
const STEPS_PER_BAR = 16; // a bar of the song, and the length of a new pattern
const MAX_STEPS = 256; // as far as the engine will play
const MAX_NOTES = 256; // notes one track can hold in one pattern: must match MAX_NOTES_PER_TRACK

// The sound editor's two pictures. Must match .sound-wave and .sound-env in the stylesheet.
const SOUND_WAVE_HEIGHT = 96;
const SOUND_ENV_HEIGHT = 118;

// The piano roll. Must match --keys, --semitone and --velocity in the stylesheet.
const KEYS = 152;
const SEMITONE = 15;
const VELOCITY = 54;
const ROLL_CELL = 30; // one step, narrower than a box: melodies are longer than beats
/*
 * How far the roll zooms, both ways at once, the way pinching a map works. Kept modest on
 * purpose: the roll draws the whole of it rather than the window, and ten octaves at three
 * times the size would be a canvas past what a browser will hand out.
 */
const MIN_ROLL_ZOOM = 0.4;
const MAX_ROLL_ZOOM = 2.5;
const DEFAULT_PITCH = 60; // middle C, and the sampler's unity pitch
// How loud a track is in a pattern until somebody moves it. Must match DEFAULT_TRACK_GAIN.
const DEFAULT_GAIN = 0.8;
// The whole of MIDI. A sampler stretched five octaves down is a different instrument, and
// people do that on purpose, so the roll goes as far as the note numbers do.
const LOW_PITCH = 0;
const HIGH_PITCH = 127;
const PITCHES = HIGH_PITCH - LOW_PITCH + 1;
// How many steps past the end of the pattern the roll draws. Notes go there, and putting
// one there makes the pattern longer: that is how a bar becomes two.
const ROLL_SPARE = 16;
const BLACK_KEYS = [1, 3, 6, 8, 10]; // semitones from C that are black

/*
 * The song is drawn in real time: a block twice as long is twice as wide. `SONG_STEP` is
 * one step at 1x, and the zoom multiplies it — pinch the trackpad or use the buttons.
 */
const SONG_STEP = 4;
const MIN_ZOOM = 0.25;
const MAX_ZOOM = 8;
const ZOOM_STEP = 1.3; // one press of a zoom button
const MAX_SNAP = 64; // the coarsest the song grid goes
const EDGE = 6; // how close to a block's edge counts as grabbing the edge

/*
 * A colour per pattern in the song, so a glance tells you what is where. Patterns get one
 * by their place in the list until somebody picks; the picked one is saved with the project.
 * All light enough to read dark text on, because the block's name is written across it.
 */
const BLOCK_COLOURS = [
  "#ff4d87", "#ff9d4d", "#ffd75e", "#9be34d",
  "#4de3a8", "#4dc9ff", "#8f8bff", "#f07bff",
];

/*
 * The little pictures on a track's row: mute, solo, and the switch to the piano roll.
 *
 * Drawings rather than letters. M and S only read as mute and solo if you already know the
 * words, they say nothing in the languages that do not start those words with those letters,
 * and ♪ was a character out of the body text with the wrong weight, the wrong size and no
 * say in either. These are one square each, drawn in `currentColor`, so a button lighting up
 * takes its icon with it.
 *
 * Written out as markup because an SVG is markup, and this is the whole of it: three static
 * strings with nothing in them from anywhere else.
 */
const ICONS = {
  // A speaker with the sound crossed out.
  mute: `<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
    <path d="M2.4 6.2h2.2L7.6 3.4v9.2L4.6 9.8H2.4z" fill="currentColor" />
    <path d="M10.2 5.8l3.6 4.4M13.8 5.8l-3.6 4.4" fill="none" stroke="currentColor"
      stroke-width="1.5" stroke-linecap="round" />
  </svg>`,
  // Headphones: the one track you have put them on to listen to.
  solo: `<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
    <path d="M3.1 10.4V8.6a4.9 4.9 0 0 1 9.8 0v1.8" fill="none" stroke="currentColor"
      stroke-width="1.5" stroke-linecap="round" />
    <rect x="1.5" y="9.4" width="3" height="4.4" rx="1.5" fill="currentColor" />
    <rect x="11.5" y="9.4" width="3" height="4.4" rx="1.5" fill="currentColor" />
  </svg>`,
  // A plug, for a track whose sound is a CLAP instrument rather than a file.
  plug: `<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
    <path d="M5.4 1.8v3.4M10.6 1.8v3.4" fill="none" stroke="currentColor" stroke-width="1.5"
      stroke-linecap="round" />
    <rect x="3" y="5.2" width="10" height="4.6" rx="1.4" fill="currentColor" />
    <path d="M8 9.8v4.4" fill="none" stroke="currentColor" stroke-width="1.5"
      stroke-linecap="round" />
  </svg>`,
  // A keyboard, because that is what the row turns into.
  keys: `<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
    <rect x="1.9" y="3.4" width="12.2" height="9.2" rx="1.6" fill="none" stroke="currentColor"
      stroke-width="1.4" />
    <path d="M6 3.4v9.2M10 3.4v9.2" fill="none" stroke="currentColor" stroke-width="1.2" />
    <path d="M4.7 3.4h2.6v5H4.7zM8.7 3.4h2.6v5H8.7z" fill="currentColor" />
  </svg>`,
};

/* One of them, ready to go in a button. */
function icon(name) {
  const holder = document.createElement("span");
  holder.className = "icon";
  holder.innerHTML = ICONS[name] ?? "";
  return holder;
}

// Pixels of drag per step of a number field.
const DRAG_PIXELS = 3;

const state = {
  bpm: 120,
  tracks: [], // { id, name, gain, muted, soloed, peaks }
  patterns: [], // { id, name, steps, notes: Map<trackId, Set<step>> }
  song: [], // { pattern, step, length }, sorted: what plays where
  open: null, // the pattern the editor has, or null for the song view
  roll: null, // the track the piano roll has, or null for the boxes
  sound: null, // the track the sound editor has, or null for the music
  trimming: null, // which end of the sound is being dragged: "start", "end", or null
  drawLength: 1, // how long the last note drawn was, so the next one matches
  picked: [], // the notes picked out, as { track, note }: what a drag, a copy or a delete has
  marquee: null, // the box being dragged round some notes: { where, from, to }
  clip: null, // what was copied: { rows: [{ down, notes }], base }
  blocks: [], // the blocks picked out in the song, as entries of state.song
  clipBlocks: null, // and what was copied out of the song: { blocks, base }
  overStep: null, // the step the pointer is over in the song, which is where a paste goes
  overRow: null, // the row the pointer is over in the step grid, which is where a paste goes
  snap: STEPS_PER_BAR, // what blocks in the song snap to, in steps
  zoom: 1, // how wide a step of the song is drawn, as a multiple of SONG_STEP
  rollZoom: 1, // how big a step and a semitone are drawn in the roll
  selected: 0, // the pattern picked out in the panel and in the song
  pinch: null, // a pinch waiting for the next frame: { where, delta, x, y }
  playing: false,
  step: 0,
  progress: 0,
  sounding: 0, // patterns making noise right now, one bit each
  needsDraw: true,
};

const el = {};
for (const id of [
  "play", "bpm", "meterMask", "status", "songMode", "songName", "addPattern",
  "patternList", "song", "snap", "zoomIn", "zoomOut", "zoomRead",
  "songScroll", "songGrid", "scrubber", "lanes", "songHint", "editor", "editorScroll",
  "closePattern",
  "add", "addBig", "steps", "fewerSteps", "moreSteps", "trackHeaders", "grid", "ruler",
  "empty", "roll", "rollScroll", "rollName", "rollRuler", "keys", "notes", "velocity",
  "closeRoll", "rollZoomIn", "rollZoomOut", "rollZoomRead", "workspace", "patternTab",
  "sound", "soundChip", "soundName", "closeSound", "hearSound", "soundBody", "soundWave",
  "soundEnv", "soundTrimKnobs", "soundShapeKnobs", "soundToneKnobs",
  "addPlugin", "picker", "closePicker", "rescan", "pluginFilter", "pluginList", "pickerNote",
  "pluginBody", "pluginParams", "pluginLevelKnobs", "paramFilter", "pluginWindow",
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

/*
 * Nothing here is allowed to fail quietly. A command that comes back in a shape this code
 * did not expect used to leave the window looking like it had ignored you — the change had
 * gone to the audio thread, so you could hear it, but nothing on screen moved. Now it says
 * so, which is the difference between a bug you can report and one you have to guess at.
 */
window.addEventListener("unhandledrejection", (e) => {
  showError(e.reason?.message ?? e.reason ?? "something went wrong");
});
window.addEventListener("error", (e) => showError(e.message));

// --- start up -------------------------------------------------------------

async function boot() {
  applyStartup(await invoke("startup"));
  requestAnimationFrame(tick);
}

/*
 * A project that has changed underneath us: an undo, a redo. Everything drawn from it is
 * drawn again, and the view stays where it was — undoing a note must not throw you back out
 * to the song — unless what it was looking at has gone.
 */
function applyProject(startup) {
  // Everything underneath is about to be replaced, and a note picked out is the note
  // itself, not the place it was in.
  forgetPicked();
  const project = startup.project;
  state.bpm = project.bpm;
  const waveforms = new Map((startup.waveforms ?? []).map((w) => [w.track, w.peaks]));
  state.tracks = project.tracks.map((track) => ({
    ...track,
    peaks: waveforms.get(track.id) ?? [],
  }));
  state.patterns = project.patterns.map(readPattern);
  state.song = project.song.map((one) => ({ ...one }));

  el.bpm.value = String(Math.round(state.bpm));
  el.songName.textContent = startup.name;
  closeColours();

  if (state.open !== null && patternById(state.open) === null) {
    closePattern();
  } else if (state.sound !== null && trackById(state.sound) === null) {
    // The sound it was showing has gone: an undo took the track back out.
    closeSound();
  } else if (state.roll !== null && !isPitched(state.roll)) {
    // The track it was showing is a row of boxes again, or gone altogether.
    closeRoll();
  } else if (state.open !== null) {
    el.steps.value = String(stepsOf(state.open));
  }
  if (patternById(state.selected) === null) {
    state.selected = state.patterns[0]?.id ?? 0;
  }
  drawPatternPanel();
  drawTrackHeaders();
  resize();
}

/*
 * Everything the front end knows, from Rust. Also runs when another project is opened, so
 * it has to replace the lot rather than add to it — and unlike an undo it decides where to
 * put you, because you have not been anywhere yet.
 */
function applyStartup(startup) {
  state.open = null;
  state.roll = null;
  applyProject(startup);
  el.songMode.title = `${startup.folder} — click for the song, double click to rename it`;
  // Land on the song when there is one, and in the first pattern when there is not: a new
  // project has nothing to arrange yet, so the grid is the only place worth being.
  if (state.song.length) {
    closePattern();
  } else {
    openPattern(state.patterns[0]?.id ?? 0);
  }
  if (startup.message) showError(startup.message);
}

/*
 * Notes arrive as a lane per track, and that is how they are kept. The step grid is the
 * notes at the sampler's own pitch, one step long; the piano roll is all of them.
 */
function readPattern(pattern) {
  const notes = new Map();
  for (const lane of pattern.lanes) {
    notes.set(
      lane.track,
      lane.notes.map((note) => ({ ...note })),
    );
  }
  return {
    id: pattern.id,
    name: pattern.name,
    steps: pattern.steps,
    colour: pattern.colour ?? null,
    // Silent wherever it plays, from the speaker on its row. The pattern's own switch, not
    // one of the row of switches below, which are about one track inside it.
    muted: pattern.muted ?? false,
    // How each track sits in this pattern: how loud, whether it is heard, and whether it is
    // an instrument. All decisions about the part rather than about the sound, so they belong
    // to the pattern — the same bass can hold a rhythm down in one and play a melody in the
    // next, loud in one and half its level in another.
    mix: new Map((pattern.mix ?? []).map((one) => [one.track, { ...one }])),
    notes,
  };
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

/* A track's notes in a pattern, made on the spot if it has none yet. */
function notesFor(pattern, track) {
  let notes = pattern.notes.get(track);
  if (!notes) {
    notes = [];
    pattern.notes.set(track, notes);
  }
  return notes;
}

/* The note a step box means: the sampler's own pitch, one step long. */
function stepNote(pattern, track, step) {
  return notesFor(pattern, track).find(
    (note) => note.step === step && note.pitch === DEFAULT_PITCH,
  );
}

function trackById(id) {
  return state.tracks.find((track) => track.id === id) ?? null;
}

/*
 * How a track sits in the pattern that is open. Every track has an answer, whether or not
 * anybody has touched it, so this fills in the default for the ones nobody has.
 */
function mixOf(track) {
  const open = openPatternNow();
  return (
    open?.mix.get(track) ?? {
      track,
      gain: DEFAULT_GAIN,
      muted: false,
      soloed: false,
      pitched: false,
    }
  );
}

/* Change one thing about it, here and in Rust. */
function setMix(track, change, command, args) {
  const open = openPatternNow();
  if (!open || trackById(track) === null) return;
  const mix = { ...mixOf(track), ...change };
  open.mix.set(track, mix);
  invoke(command, { pattern: open.id, track, ...args });
  state.needsDraw = true;
}

function isPitched(track) {
  return mixOf(track).pitched;
}

// --- the two views --------------------------------------------------------

/*
 * One of four at a time: the song, a pattern's boxes, a pattern's piano roll, or one track's
 * sound. The first three are the same music at different magnifications; the fourth is a
 * different question altogether — not what is played but what it is played with — which is
 * why it sits on top of the pattern rather than beside it.
 */
function showView() {
  const inPattern = state.open !== null;
  const inSound = inPattern && state.sound !== null;
  const inRoll = inPattern && !inSound && state.roll !== null;
  el.song.classList.toggle("hidden", inPattern);
  el.editor.classList.toggle("hidden", !inPattern || inRoll || inSound);
  el.roll.classList.toggle("hidden", !inRoll);
  el.sound.classList.toggle("hidden", !inSound);
}

/*
 * Which pattern the editor and the roll belong to, said in colour. The tab in the corner,
 * the close button, the line under the ruler and every note drawn inside all come from
 * here, so a pattern looks like the block in the song it was opened from.
 */
function showPatternColour() {
  const open = openPatternNow();
  const colour = open === null ? PALETTE.accent : colourOf(open.id);
  el.workspace.style.setProperty("--pattern", colour);
  const name = open === null ? "" : open.name;
  el.patternTab.textContent = name;
  el.patternTab.title = name;
}

function openPattern(id) {
  if (patternById(id) === null) return;
  forgetPicked();
  state.open = id;
  state.selected = id;
  state.roll = null;
  state.sound = null;
  el.steps.value = String(stepsOf(id));
  showPatternColour();
  showView();
  invoke("open_pattern", { id });
  drawPatternPanel();
  // Which rows are piano rolls is this pattern's own business, so the instrument column is
  // built again for the pattern being opened.
  drawTrackHeaders();
  resize();
}

function closePattern() {
  forgetPicked();
  // The pattern you were in stays picked out, so the song shows you where it plays.
  if (state.open !== null) state.selected = state.open;
  state.open = null;
  state.roll = null;
  state.sound = null;
  showView();
  invoke("close_pattern");
  drawPatternPanel();
  resize();
}

/*
 * Clicking a pattern in the panel opens it, and clicking the open one closes it again. It
 * also picks that pattern out in the song, where its lane is lifted above the others, so
 * closing leaves you looking at where the pattern you were just in actually plays.
 */
function togglePattern(id) {
  if (state.open === id) {
    closePattern();
    return;
  }
  openPattern(id);
}

el.closePattern.addEventListener("click", closePattern);

/* The panel's heading is the way back to the song, from wherever you are. */
el.songMode.addEventListener("click", (e) => {
  // The second click of a double click is the start of a rename, not a second trip home.
  if (e.detail > 1) return;
  if (state.open !== null) closePattern();
});

/*
 * Double click the song's name to change it, the same as a pattern's. The folder is the
 * project, so this renames the folder: Rust writes it out first, so nothing is in flight
 * while it moves.
 */
el.songMode.addEventListener("dblclick", () => {
  if (el.songMode.querySelector(".rename")) return;
  const shown = el.songName;
  const input = document.createElement("input");
  input.className = "rename";
  input.type = "text";
  input.value = shown.textContent;
  input.maxLength = 60;
  shown.replaceWith(input);
  input.focus();
  input.select();

  let done = false;
  const finish = async (keep) => {
    if (done) return;
    done = true;
    input.replaceWith(shown);
    if (!keep || input.value.trim() === shown.textContent) return;
    try {
      shown.textContent = await invoke("rename_project", { name: input.value });
    } catch (e) {
      showError(e);
    }
  };
  input.addEventListener("click", (e) => e.stopPropagation());
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") finish(true);
    if (e.key === "Escape") {
      e.stopPropagation();
      finish(false);
    }
  });
  input.addEventListener("blur", () => finish(true));
});

// --- dragging a row up and down its list ----------------------------------

/*
 * Reordering, for the two lists that have an order: the patterns down the left, which is
 * also which lane each one is in the song, and the instruments in a pattern.
 *
 * Both are a column of rows all the same height, so where a row would land is arithmetic
 * rather than hit testing, and the rows it passes slide out of its way with a transform —
 * which is the one thing a browser can move without laying the page out again.
 *
 * Neither list's order means anything to the engine. A pattern and a track are both known
 * everywhere else by their id, which is also their slot on the audio thread, so this is
 * only about which row a thing is drawn on.
 */
const REORDER_GRIP = 4; // pixels of travel before a press is a drag rather than a click

let reordering = null; // { row, kind, from, to, height, rows, startY }

/*
 * The list in the order it is being shown, which is not the order it is in while one of its
 * rows is in your hand: the rows have slid out of the way, and what is drawn beside them —
 * the step grid, the song's lanes — has to agree, or the notes are under somebody else's
 * name for as long as the drag lasts.
 */
function shownIn(list, kind) {
  const drag = reordering;
  if (!drag || !drag.moved || drag.kind !== kind || drag.to === drag.from) return list;
  const order = [...list];
  const [one] = order.splice(drag.from, 1);
  order.splice(drag.to, 0, one);
  return order;
}

const shownTracks = () => shownIn(state.tracks, "tracks");
const shownPatterns = () => shownIn(state.patterns, "patterns");

function reorderable(row, { at, kind, height, onDrop }) {
  row.addEventListener("pointerdown", (e) => {
    // A button, a fader or the rename box is itself, not a handle for the row.
    if (e.button !== 0 || e.target.closest("button, input, .rename")) return;
    const rows = [...row.parentElement.children];
    reordering = {
      row,
      kind,
      from: at,
      to: at,
      height,
      rows,
      startY: e.clientY,
      onDrop,
      moved: false,
    };
    row.setPointerCapture(e.pointerId);
  });

  row.addEventListener("pointermove", (e) => {
    const drag = reordering;
    if (!drag || drag.row !== row) return;
    const dy = e.clientY - drag.startY;
    if (!drag.moved && Math.abs(dy) < REORDER_GRIP) return;
    if (!drag.moved) {
      drag.moved = true;
      row.classList.add("lifting");
    }
    drag.to = Math.max(0, Math.min(drag.rows.length - 1, drag.from + Math.round(dy / height)));
    // The row never leaves its list, however far past the ends your hand goes: there is
    // nowhere above the first row for it to be, and it would be over the heading.
    const held = Math.max(
      -drag.from * height,
      Math.min((drag.rows.length - 1 - drag.from) * height, dy),
    );
    row.style.transform = `translateY(${held}px)`;
    // Everything the row has passed shuffles up or down by one, so the gap it would land in
    // is where you can see it.
    for (const [place, other] of drag.rows.entries()) {
      if (other === row) continue;
      const shift =
        place > drag.from && place <= drag.to
          ? -height
          : place < drag.from && place >= drag.to
            ? height
            : 0;
      other.style.transform = shift ? `translateY(${shift}px)` : "";
    }
    state.needsDraw = true;
  });

  const drop = () => {
    const drag = reordering;
    if (!drag || drag.row !== row) return;
    reordering = null;
    row.classList.remove("lifting");
    for (const other of drag.rows) other.style.transform = "";
    state.needsDraw = true;
    if (!drag.moved) return;
    // The click at the end of a drag is the end of a drag, not a click: without this,
    // letting go of a pattern would also open it.
    swallowNextClick();
    if (drag.to !== drag.from) drag.onDrop(drag.to);
  };
  row.addEventListener("pointerup", drop);
  row.addEventListener("pointercancel", drop);
}

function swallowNextClick() {
  const swallow = (e) => {
    e.stopPropagation();
    e.preventDefault();
  };
  window.addEventListener("click", swallow, { capture: true, once: true });
  // And if no click follows — a drag out of the window, say — it is not left waiting.
  setTimeout(() => window.removeEventListener("click", swallow, true), 0);
}

// --- the patterns panel ---------------------------------------------------

function drawPatternPanel() {
  el.patternList.replaceChildren(
    ...state.patterns.map((pattern) => {
      const row = document.createElement("div");
      row.className = "prow";
      row.dataset.id = String(pattern.id);

      // The colour its blocks are drawn in, and the way to change it. The row is tinted
      // with it too when the pattern is open, so the panel, the song and the editor are all
      // saying the same thing.
      const colour = blockColour(pattern, state.patterns.indexOf(pattern));
      row.style.setProperty("--pattern", colour);
      row.style.setProperty("--pattern-soft", tint(colour, 0.2));
      const swatch = document.createElement("button");
      swatch.className = "swatch";
      swatch.style.background = colour;
      swatch.title = "The colour this pattern is in the song";
      swatch.addEventListener("click", (e) => {
        e.stopPropagation();
        pickColour(swatch, pattern);
      });
      row.append(swatch);

      const name = document.createElement("span");
      name.className = "pname";
      name.textContent = pattern.name;
      row.append(name);

      const len = document.createElement("span");
      len.className = "plen";
      len.textContent = String(pattern.steps);
      len.title = `${pattern.steps} steps`;
      row.append(len);

      // The pattern's own mute. Not the one on a track's row, which is one track inside
      // one pattern: this one is the pattern itself, so every block of it in the song goes
      // quiet at once. Which is how you listen to a song without the hats without taking
      // the hats out.
      const hush = document.createElement("button");
      hush.className = `tick mute${pattern.muted ? " on" : ""}`;
      hush.replaceChildren(icon("mute"));
      hush.title = mutingSays(pattern.muted);
      hush.setAttribute("aria-label", hush.title);
      hush.addEventListener("click", (e) => {
        e.stopPropagation();
        mutePattern(pattern, !pattern.muted);
      });
      row.append(hush);

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

      // Drag it up or down to reorder the patterns, which is also which lane it is in the
      // song. Nothing moves but the order: the song says which pattern plays where by id.
      reorderable(row, {
        at: state.patterns.indexOf(pattern),
        kind: "patterns",
        height: LANE,
        onDrop: (to) => {
          rearrange("move_pattern", { id: pattern.id, to });
        },
      });
      return row;
    }),
  );
  markPatternRows();
  showPatternColour();
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
    const muted = patternById(id)?.muted ?? false;
    row.classList.toggle("open", state.open === id);
    row.classList.toggle("picked", state.open !== id && state.selected === id);
    row.classList.toggle("playing", state.playing && isSounding(id));
    // A silenced pattern says so without the panel being rebuilt, so the speaker can be
    // pressed while the song runs and the row goes faint under your finger.
    row.classList.toggle("muted", muted);
    const hush = row.querySelector(".tick.mute");
    if (hush) {
      hush.classList.toggle("on", muted);
      hush.title = mutingSays(muted);
      hush.setAttribute("aria-label", hush.title);
    }
  }
}

const mutingSays = (muted) =>
  muted ? "Silent — click to hear this pattern again" : "Silence this pattern, wherever it plays";

/* Turn a whole pattern off, or on again. Rust holds it; the audio thread fades it out. */
function mutePattern(pattern, muted) {
  pattern.muted = muted;
  invoke("mute_pattern", { id: pattern.id, muted });
  markPatternRows();
  state.needsDraw = true;
}

/* The engine reports what is sounding as one bit per pattern. */
function isSounding(id) {
  return ((state.sounding >>> id) & 1) === 1;
}

/*
 * The colours to pick from. A set rather than a colour wheel: eight that are all light
 * enough to read a pattern's name on, which a free choice would not be.
 */
let openColours = null;
let closeColoursOn = null;

function pickColour(near, pattern) {
  closeColours();
  const box = document.createElement("div");
  box.className = "colours";
  const spot = near.getBoundingClientRect();
  box.style.left = `${Math.round(spot.left)}px`;
  box.style.top = `${Math.round(spot.bottom + 6)}px`;

  BLOCK_COLOURS.forEach((colour, index) => {
    const pick = document.createElement("button");
    pick.style.background = colour;
    pick.title = `Colour ${index + 1}`;
    pick.addEventListener("click", async () => {
      closeColours();
      pattern.colour = index;
      state.needsDraw = true;
      drawPatternPanel();
      await invoke("set_pattern_colour", { id: pattern.id, colour: index });
    });
    box.append(pick);
  });

  document.body.append(box);
  openColours = box;
  // Anything else you do puts it away, which is what a popover is for — but pressing
  // inside it is picking a colour, and taking it away before the click landed would mean
  // nothing in here could ever be clicked.
  closeColoursOn = (e) => {
    if (!box.contains(e.target)) closeColours();
  };
  setTimeout(() => window.addEventListener("pointerdown", closeColoursOn, true));
}

function closeColours() {
  if (closeColoursOn) window.removeEventListener("pointerdown", closeColoursOn, true);
  closeColoursOn = null;
  if (openColours) openColours.remove();
  openColours = null;
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
  forgetPicked();
  try {
    const after = await invoke(command, args);
    state.patterns = after.patterns.map(readPattern);
    state.song = after.song.map((one) => ({ ...one }));
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
el.songScroll.addEventListener("scroll", () => {
  syncScroll(el.songScroll, el.patternList);
  // The song canvases only draw what is in the window, so scrolling sideways is a redraw.
  state.needsDraw = true;
});

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
  const open = openPatternNow();
  if (!open) return;
  const asked = Math.max(1, Math.min(MAX_STEPS, Math.round(steps) || 1));
  if (asked === open.steps) return;
  // Rust hands back the length it settled on and the whole arrangement: notes that no
  // longer fit are shortened or dropped, and there is no telling which from here. The song
  // is left alone — a block in it is as long as it was drawn, whatever its pattern does.
  forgetPicked();
  const [actual, after] = await invoke("set_pattern_steps", { id: open.id, steps: asked });
  state.patterns = after.patterns.map(readPattern);
  state.song = after.song.map((one) => ({ ...one }));
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
  el.addPlugin.disabled = true;
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
    el.addPlugin.disabled = false;
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

      // The picture of the sound is the way to the sound: clicking a track's name plays
      // it, clicking its picture asks what it is. A plugin has no waveform to show — it is
      // an instrument, not a file — so it wears a plug instead.
      const wave = document.createElement("button");
      wave.className = "wave";
      wave.title = track.plugin
        ? `${track.plugin.name} — click for its own window, shift click for our controls`
        : `${track.name} — click to shape this sound`;
      wave.setAttribute("aria-label", wave.title);
      if (track.plugin) {
        wave.classList.add("plugged");
        // Lit while its window is up, so the plug says which way the next press goes.
        wave.classList.toggle("on", plugins.windows.has(track.id));
        wave.append(icon("plug"));
      } else {
        const drawing = document.createElement("canvas");
        drawing.width = 68;
        drawing.height = 36;
        drawWaveform(drawing, track.peaks);
        wave.append(drawing);
      }
      wave.addEventListener("click", (e) => {
        // A plug goes to the plugin's own window: Surge XT as its own designers drew it,
        // which is the thing you came for. What we have to say about a plugin track is a
        // shift click away — the track's level, and the plugin's parameters — and is where
        // a plugin with no window of its own lands you anyway.
        if (track.plugin && !e.shiftKey) {
          togglePluginWindow(track.id);
          return;
        }
        openSound(track.id);
      });
      row.append(wave);

      // The whole row belongs to the pattern that is open: how loud, what is heard, and
      // whether it is an instrument.
      const mix = mixOf(track.id);
      const solo = toggle("solo", "Solo in this pattern", mix.soloed, (on) => {
        setMix(track.id, { soloed: on }, "set_pattern_soloed", { soloed: on });
      });
      const mute = toggle("mute", "Mute in this pattern", mix.muted, (on) => {
        setMix(track.id, { muted: on }, "set_pattern_muted", { muted: on });
        // Mute wins, so a muted track's solo is showing something that is not happening.
        beaten(solo, on);
      });
      beaten(solo, mix.muted);

      const gain = document.createElement("input");
      gain.type = "range";
      gain.min = "0";
      gain.max = "120";
      gain.step = "1";
      gain.value = String(Math.round(mix.gain * 100));
      gain.title = "Volume in this pattern";
      gain.addEventListener("input", () => {
        const level = Number(gain.value) / 100;
        setMix(track.id, { gain: level }, "set_pattern_gain", { gain: level });
      });

      // Turns the row into a piano roll and back. Nothing is thrown away either way: the
      // boxes and the roll are two views of one lane of notes.
      const pitched = isPitched(track.id);
      const roll = document.createElement("button");
      roll.className = `tick keys-on${pitched ? " on" : ""}`;
      roll.replaceChildren(icon("keys"));
      roll.title = pitched
        ? "Back to the boxes in this pattern, and back to a one-shot"
        : "Piano roll: in this pattern, play this one pitched and show its notes";
      roll.setAttribute("aria-label", roll.title);
      roll.addEventListener("click", () => setPitched(track.id, !isPitched(track.id)));

      const kill = document.createElement("button");
      kill.className = "tick kill";
      kill.textContent = "×";
      kill.title = "Delete this track";
      kill.addEventListener("click", () => removeTrack(track.id));

      row.append(roll, mute, solo, gain, kill);

      // And the same for the instruments: drag one up or down to move its row. Its notes go
      // with it, because they were never kept by row — a track is known by its id.
      reorderable(row, {
        at: state.tracks.indexOf(track),
        kind: "tracks",
        height: ROW,
        onDrop: (to) => moveTrack(track.id, to),
      });
      return row;
    }),
  );
}

function toggle(name, title, on, onChange) {
  const button = document.createElement("button");
  button.className = `tick ${name}${on ? " on" : ""}`;
  button.replaceChildren(icon(name));
  button.title = title;
  button.setAttribute("aria-label", title);
  button.addEventListener("click", () => {
    const next = !button.classList.contains("on");
    button.classList.toggle("on", next);
    onChange(next);
  });
  return button;
}

/*
 * Solo, shown as something a mute is overruling. Mute is the switch that always means
 * silence, so a track that is both is quiet however it is soloed — and a solo button lit up
 * on a silent track would be saying the opposite. It stays lit, because turning the mute off
 * brings it straight back, and goes faint to say it is not the one being listened to.
 */
function beaten(solo, muted) {
  solo.classList.toggle("beaten", muted);
  solo.title = muted
    ? "Solo in this pattern — the mute wins while it is on"
    : "Solo in this pattern";
  solo.setAttribute("aria-label", solo.title);
}

/*
 * Move an instrument's row. Rust hands back the ids in the order they are now in, which is
 * all this needs: the rows are already here, with the waveforms they were drawn with.
 */
async function moveTrack(id, to) {
  try {
    const order = await invoke("move_track", { id, to });
    state.tracks.sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
  } catch (e) {
    showError(e);
  }
  drawTrackHeaders();
  state.needsDraw = true;
}

/* Deleting a track takes its notes out of every pattern, and its sample out of the folder. */
function removeTrack(id) {
  forgetPicked();
  invoke("remove_track", { id });
  state.tracks = state.tracks.filter((track) => track.id !== id);
  for (const pattern of state.patterns) {
    pattern.notes.delete(id);
  }
  if (state.roll === id) closeRoll();
  if (state.sound === id) closeSound();
  plugins.params.delete(id);
  plugins.windows.delete(id);
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
// Undo and redo from the menu bar. A different event from opening a project, because this
// one leaves you where you are.
listen("stepped", (event) => stepped(event.payload));
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

/* And how tall, for the ones whose height is the window's rather than the music's. */
function drawnHeight(canvas) {
  return parseInt(canvas.style.height, 10) || 0;
}

function gridWidth() {
  return stepsOf(state.open) * CELL;
}

function gridHeight() {
  return Math.max(ROW, state.tracks.length * ROW);
}

/*
 * Bars the song is long: what is in it, one spare on the end, and enough to fill the window.
 * Empty bars are as good a place to put a pattern as any, so they are all there to click.
 */
function songBars() {
  const room = Math.ceil(el.songScroll.clientWidth / Math.max(1, barPx()));
  return Math.max(songSteps() / STEPS_PER_BAR + 1, room);
}

/* How wide the song is in total: the scroll, not the canvas. */
function songWidth() {
  return Math.round(songBars() * barPx());
}

/* The roll draws past the end of the pattern, because you can put a note there. */
function rollSteps() {
  return stepsOf(state.open) + ROLL_SPARE;
}

/*
 * Three views, three shapes, kept apart on purpose. Handing a canvas a new width throws
 * its backing store away, and doing that to eight canvases sixty times a second — which is
 * what a trackpad pinch asks for — is the difference between a zoom that glides and one
 * that stutters. So each of these only touches its own.
 */
function resizeEditor() {
  size(el.grid, gridWidth(), gridHeight());
  size(el.ruler, gridWidth(), HEAD - HEAD_LINE);
  state.needsDraw = true;
  drawRuler();
}

function resizeRoll() {
  const width = rollSteps() * rollCell();
  const height = PITCHES * semitone();
  size(el.notes, width, height);
  size(el.rollRuler, width, HEAD - HEAD_LINE);
  size(el.keys, KEYS, height);
  size(el.velocity, width, VELOCITY);
  // The stylesheet draws a line under every semitone across the whole width, so it has to
  // be told when a semitone changes size.
  document.documentElement.style.setProperty("--semitone", `${semitone()}px`);
  el.rollZoomRead.textContent = `${Math.round(state.rollZoom * 100) / 100}×`;
  state.needsDraw = true;
}

function resizeSong() {
  // The canvases are only as wide as the window; the grid around them carries the song's
  // real width, which is what makes the zoom cheap however long the song is.
  const window_ = Math.max(1, el.songScroll.clientWidth);
  el.songGrid.style.width = `${songWidth()}px`;
  size(el.lanes, window_, Math.max(LANE, state.patterns.length * LANE));
  size(el.scrubber, window_, HEAD - 1);
  el.songHint.classList.toggle("hidden", state.song.length > 0);
  el.zoomRead.textContent = `${Math.round(state.zoom * 100) / 100}×`;
  state.needsDraw = true;
}

/*
 * The sound editor is the one view whose canvases are the shape of the window rather than
 * the shape of the music, so it is measured off the panels it sits in.
 */
function resizeSound() {
  if (state.sound === null) return;
  if (isPlugin(state.sound)) {
    drawSound();
    return;
  }
  const wave = el.soundWave.parentElement.clientWidth;
  size(el.soundWave, Math.max(80, wave), SOUND_WAVE_HEIGHT);
  const shape = el.soundEnv.parentElement.clientWidth;
  size(el.soundEnv, Math.max(80, shape), SOUND_ENV_HEIGHT);
  drawSound();
}

function resize() {
  resizeEditor();
  resizeRoll();
  resizeSong();
  resizeSound();
}

/*
 * Zooming the song does not change the size of a single canvas — they are the window's
 * width whatever the zoom — so all it takes is the grid's width and a redraw.
 */
function relayoutSong() {
  el.songGrid.style.width = `${songWidth()}px`;
  el.zoomRead.textContent = `${Math.round(state.zoom * 100) / 100}×`;
  state.needsDraw = true;
}

/* The song got longer or shorter, so only resize when it actually changed shape. */
function songChanged() {
  if (el.songGrid.style.width !== `${songWidth()}px`) {
    resizeSong();
  } else {
    el.songHint.classList.toggle("hidden", state.song.length > 0);
    state.needsDraw = true;
  }
}

window.addEventListener("resize", resize);

// --- dragging past the edge -----------------------------------------------

/*
 * A drag that reaches the edge of the window scrolls the view to follow it.
 *
 * Stretching a note or a block is the case that needs it: the thing you are dragging is the
 * right hand end, so the moment it reaches the edge there is nowhere left to pull it to and
 * you have to let go, scroll, and pick it up again. The view moves instead.
 *
 * Nothing here knows what is being dragged. It scrolls, and then hands the drag the last
 * place the pointer was — which is now over a different step — so whatever was following the
 * pointer carries on following it. One scroll per frame, from the same loop that draws.
 */
const EDGE_SCROLL = 32; // how close to an edge the pointer has to get to start it
const EDGE_SCROLL_MAX = 26; // and how fast it goes hard against it, in pixels a frame

let edgeScroll = null; // { scroller, axes, point, apply }

/*
 * Follow this drag. Called on every move rather than once at the start, because the whole
 * job is knowing where the pointer is now.
 */
function followEdge(scroller, event, axes, apply) {
  edgeScroll = {
    scroller,
    axes,
    point: { clientX: event.clientX, clientY: event.clientY },
    apply,
  };
}

function stopFollowing() {
  edgeScroll = null;
}

/* How fast to scroll for a pointer this far into an edge: nothing until it is close. */
function edgePush(at, low, high) {
  if (at < low + EDGE_SCROLL) {
    return -Math.min(1, (low + EDGE_SCROLL - at) / EDGE_SCROLL) * EDGE_SCROLL_MAX;
  }
  if (at > high - EDGE_SCROLL) {
    return Math.min(1, (at - (high - EDGE_SCROLL)) / EDGE_SCROLL) * EDGE_SCROLL_MAX;
  }
  return 0;
}

/* One frame of it. Does nothing at all unless a drag has asked to be followed. */
function runEdgeScroll() {
  const drag = edgeScroll;
  if (!drag) return;
  const box = drag.scroller.getBoundingClientRect();
  const dx = drag.axes.includes("x") ? edgePush(drag.point.clientX, box.left, box.right) : 0;
  const dy = drag.axes.includes("y") ? edgePush(drag.point.clientY, box.top, box.bottom) : 0;
  if (!dx && !dy) return;
  const wasX = drag.scroller.scrollLeft;
  const wasY = drag.scroller.scrollTop;
  drag.scroller.scrollLeft = Math.max(0, wasX + dx);
  drag.scroller.scrollTop = Math.max(0, wasY + dy);
  // Already at the end of what there is to scroll, so the drag has nothing new to hear.
  if (drag.scroller.scrollLeft === wasX && drag.scroller.scrollTop === wasY) return;
  drag.apply(drag.point);
}

// --- the pattern editor ---------------------------------------------------

function drawRuler() {
  const ctx = el.ruler.getContext("2d");
  const steps = stepsOf(state.open);
  const width = drawnWidth(el.ruler);
  ctx.clearRect(0, 0, width, HEAD);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";
  for (let step = 0; step < steps; step++) {
    const onBeat = step % STEPS_PER_BEAT === 0;
    ctx.fillStyle = onBeat ? PALETTE.dim : PALETTE.line;
    if (onBeat) {
      ctx.fillText(String(step / STEPS_PER_BEAT + 1), step * CELL + GAP + 2, 14);
    } else {
      ctx.fillRect(step * CELL + GAP, 13, 3, 1);
    }
  }
}

function drawGrid() {
  const pattern = openPatternNow();
  if (!pattern) return;
  // Everything drawn in here is the colour of the block this pattern is in the song, so a
  // pattern and its blocks are plainly the same thing seen two ways.
  const ink = colourOf(pattern.id);
  const ctx = el.grid.getContext("2d");
  const width = drawnWidth(el.grid);
  const height = gridHeight();
  ctx.clearRect(0, 0, width, height);

  // The column the playhead is in, drawn under the steps so lit boxes stay readable.
  if (state.playing) {
    ctx.fillStyle = "rgba(255,215,94,0.09)";
    ctx.fillRect(state.step * CELL, 0, CELL, height);
  }

  const rows = shownTracks();
  for (let row = 0; row < rows.length; row++) {
    const track = rows[row];
    const y = row * ROW;
    // An instrument's row is its notes rather than a line of boxes: boxes cannot say what
    // pitch or how long, which is the whole point of turning it into one.
    if (pattern.mix.get(track.id)?.pitched) {
      drawMiniRoll(ctx, pattern, track, y);
      continue;
    }
    // Only the notes a box can mean: the sampler's own pitch. Anything drawn in the piano
    // roll lives in the same lane and is left to the roll to show.
    const ticked = new Set();
    const picked = new Set();
    for (const note of pattern.notes.get(track.id) ?? []) {
      if (note.pitch !== DEFAULT_PITCH) continue;
      ticked.add(note.step);
      if (isPicked(track.id, note)) picked.add(note.step);
    }

    for (let step = 0; step < pattern.steps; step++) {
      const x = step * CELL + GAP;
      const w = CELL - GAP * 2;
      const h = ROW - BOX_INSET * 2 - 1;
      const on = ticked.has(step);
      const onBeat = step % STEPS_PER_BEAT === 0;

      if (on) {
        const live = state.playing && state.step === step;
        ctx.fillStyle = live ? PALETTE.lit : ink;
      } else {
        ctx.fillStyle = onBeat ? "#272132" : "#201c29";
      }
      roundRect(ctx, x, y + BOX_INSET, w, h, 4);
      ctx.fill();
      if (on && picked.has(step)) {
        outlinePicked(ctx, () => roundRect(ctx, x, y + BOX_INSET, w, h, 4));
      }
    }
  }

  if (state.marquee && state.marquee.where === "grid") drawMarquee(ctx, state.marquee);
}

/*
 * One instrument's notes, in the room a row of boxes would have taken. Click it to open the
 * roll proper; the keyboard button on the row puts the boxes back, and neither throws
 * anything away — the boxes and the roll have always been the same lane of notes.
 *
 * The pitches are scaled to what is actually in there, never less than an octave, so a bass
 * line that stays inside a fifth still uses the height rather than hugging the middle.
 */
function drawMiniRoll(ctx, pattern, track, y) {
  const notes = pattern.notes.get(track.id) ?? [];
  const ink = colourOf(pattern.id);
  const width = pattern.steps * CELL;
  const top = y + GAP;
  const height = ROW - GAP * 2 - 1;

  ctx.fillStyle = "#1b1724";
  roundRect(ctx, GAP, top, Math.max(4, width - GAP * 2), height, 5);
  ctx.fill();
  ctx.save();
  roundRect(ctx, GAP, top, Math.max(4, width - GAP * 2), height, 5);
  ctx.clip();

  for (let step = 0; step < pattern.steps; step++) {
    if (step % STEPS_PER_BEAT !== 0) continue;
    ctx.fillStyle = step % STEPS_PER_BAR === 0 ? "rgba(0,0,0,0.5)" : "rgba(0,0,0,0.25)";
    ctx.fillRect(step * CELL, top, 1, height);
  }

  if (!notes.length) {
    ctx.fillStyle = PALETTE.dim;
    ctx.font = "11px ui-sans-serif, system-ui, sans-serif";
    ctx.textBaseline = "middle";
    ctx.fillText("piano roll — click to open", GAP + 10, top + height / 2);
    ctx.restore();
    return;
  }

  let low = 127;
  let high = 0;
  for (const note of notes) {
    low = Math.min(low, note.pitch);
    high = Math.max(high, note.pitch);
  }
  const span = Math.max(11, high - low);
  const middle = (low + high) / 2;
  const bottom = middle - span / 2;
  const bar = 3;
  for (const note of notes) {
    const x = note.step * CELL + 1;
    const w = Math.max(3, Math.max(1, note.length) * CELL - 2);
    const up = ((note.pitch - bottom) / span) * (height - bar - 4);
    const live =
      state.playing &&
      state.step >= note.step &&
      state.step < note.step + Math.max(1, note.length);
    ctx.fillStyle = live ? PALETTE.lit : ink;
    ctx.fillRect(x, top + height - 2 - bar - up, w, bar);
    if (isPicked(track.id, note)) {
      outlinePicked(ctx, () => {
        ctx.beginPath();
        ctx.rect(x - 0.5, top + height - 2.5 - bar - up, w + 1, bar + 1);
      });
    }
  }
  ctx.restore();
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
  if ((stepNote(cell.pattern, cell.track, cell.step) !== undefined) === on) return;
  setStepLocally(cell, on);
  state.needsDraw = true;
  invoke("set_step", {
    pattern: cell.pattern.id,
    track: cell.track,
    step: cell.step,
    on,
  }).then((actual) => {
    // Rust has the final say: a track that is full will not take another note.
    if (actual !== on) {
      setStepLocally(cell, actual);
      state.needsDraw = true;
    }
  });
}

function setStepLocally(cell, on) {
  const notes = notesFor(cell.pattern, cell.track);
  const at = notes.findIndex(
    (note) => note.step === cell.step && note.pitch === DEFAULT_PITCH,
  );
  if (on && at < 0) {
    notes.push({ step: cell.step, pitch: DEFAULT_PITCH, velocity: 100, length: 1 });
  }
  if (!on && at >= 0) notes.splice(at, 1);
}

/* True when a press is the right button, which always rubs out rather than draws. */
function erasing(event) {
  return event.button === 2 || (event.buttons & 2) === 2;
}

/*
 * Where a pointer is in the grid as a plain point, clamped to it. What the box you drag
 * round some notes is measured in, the same as in the roll.
 */
function gridPoint(event) {
  const rect = el.grid.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(gridWidth(), event.clientX - rect.left)),
    y: Math.max(0, Math.min(gridHeight(), event.clientY - rect.top)),
  };
}

/* Which row of the grid a pointer is over, or null for none. Where a paste lands. */
function rowAt(event) {
  const row = Math.floor((event.clientY - el.grid.getBoundingClientRect().top) / ROW);
  return row >= 0 && row < state.tracks.length ? row : null;
}

/*
 * Every note a box dragged over the grid touches. It runs across rows, which is the point:
 * a set picked out here can be copied out of one instrument and pasted into another.
 */
function notesInGridBox(box) {
  const pattern = openPatternNow();
  if (!pattern) return [];
  const left = Math.min(box.from.x, box.to.x);
  const right = Math.max(box.from.x, box.to.x);
  const top = Math.min(box.from.y, box.to.y);
  const bottom = Math.max(box.from.y, box.to.y);
  const found = [];
  for (let row = 0; row < state.tracks.length; row++) {
    if (row * ROW + ROW < top || row * ROW > bottom) continue;
    const track = state.tracks[row].id;
    const pitched = Boolean(pattern.mix.get(track)?.pitched);
    for (const note of pattern.notes.get(track) ?? []) {
      // What plays is what you can see, and what a box takes is the same: a row of boxes
      // only shows the notes a box can mean.
      if (!pitched && note.pitch !== DEFAULT_PITCH) continue;
      const x = note.step * CELL;
      const w = (pitched ? Math.max(1, note.length) : 1) * CELL;
      if (x + w < left || x > right) continue;
      found.push({ track, note });
    }
  }
  return found;
}

let gridBox = false; // a box being dragged round some notes rather than a run of painting

el.grid.addEventListener("pointerdown", (e) => {
  /*
   * Shift drags a box round notes instead of painting. It has to be a modifier: pressing on
   * a box already means "tick it and keep painting", which is the whole way you write a
   * beat and not something to give up for a selection box.
   */
  if (e.shiftKey) {
    const corner = gridPoint(e);
    el.grid.setPointerCapture(e.pointerId);
    gridBox = true;
    state.marquee = { where: "grid", from: corner, to: corner };
    state.needsDraw = true;
    return;
  }
  const cell = cellAt(e);
  if (!cell) return;
  clearPicked();
  // An instrument's row is a piano roll, and a piano roll opens rather than being ticked.
  if (isPitched(cell.track)) {
    openRoll(cell.track);
    return;
  }
  // Drag across boxes to paint, like FL Studio: what the first box becomes is what the
  // rest become, so a drag never toggles boxes back and forth under your finger. The right
  // button always rubs out, which saves aiming at the box you meant to remove.
  painting = erasing(e)
    ? false
    : stepNote(cell.pattern, cell.track, cell.step) === undefined;
  el.grid.setPointerCapture(e.pointerId);
  paint(cell, painting);
});

el.grid.addEventListener("pointermove", (e) => {
  if (gridBox) {
    followEdge(el.editorScroll, e, "xy", gridDragTo);
    gridDragTo(e);
    return;
  }
  if (painting === null) {
    // Which row the pointer is over is where a paste goes, so it is worth keeping even
    // when nothing is being dragged.
    state.overRow = rowAt(e);
    const cell = cellAt(e);
    const cursor = e.shiftKey
      ? "crosshair"
      : !cell
        ? "default"
        : isPitched(cell.track)
          ? "pointer"
          : "cell";
    if (el.grid.style.cursor !== cursor) el.grid.style.cursor = cursor;
    return;
  }
  paint(cellAt(e), painting);
});

/* Where the box has got to, taken apart so a scroll under a still hand can put it through. */
function gridDragTo(point) {
  if (!gridBox || !state.marquee) return;
  state.marquee.to = gridPoint(point);
  state.overRow = rowAt(point);
  pickNotes(notesInGridBox(state.marquee), true);
}

const stopPainting = () => {
  painting = null;
  stopFollowing();
  if (gridBox) {
    gridBox = false;
    state.marquee = null;
    state.needsDraw = true;
    sayPicked();
  }
};
el.grid.addEventListener("pointerup", stopPainting);
el.grid.addEventListener("pointercancel", stopPainting);
el.grid.addEventListener("pointerleave", () => {
  if (!gridBox) state.overRow = null;
});

// --- the piano roll -------------------------------------------------------

/*
 * The same pattern as the step grid, seen as notes: the keyboard down the left, the notes in
 * the middle, how hard each is hit underneath. Nothing here is a different kind of data —
 * a box is a note at the sampler's own pitch, one step long, and this is the editor that
 * lets you put one anywhere.
 */
function openRoll(track) {
  if (state.open === null || trackById(track) === null) return;
  // The grid and the roll each pick their own notes out; going between them starts again.
  forgetPicked();
  state.roll = track;
  showView();
  // Notes only mean pitch and length on an instrument, so opening the roll makes it one in
  // this pattern. The keyboard button on the row is there to change your mind.
  if (!isPitched(track)) setPitched(track, true);
  showPitched();
  resize();
  // Land on the sampler's own pitch, which is where the notes will be.
  el.rollScroll.scrollTop = Math.max(
    0,
    (HIGH_PITCH - DEFAULT_PITCH - 6) * semitone(),
  );
}

function closeRoll() {
  forgetPicked();
  state.roll = null;
  showView();
  resize();
}

el.closeRoll.addEventListener("click", closeRoll);

function rollNotes() {
  const pattern = openPatternNow();
  if (!pattern || state.roll === null) return [];
  return notesFor(pattern, state.roll);
}

/*
 * One flag does two things, because they are the same thing: an instrument's notes mean a
 * pitch and a length, so it is played pitched and its row shows the notes. A one-shot's
 * notes mean "here", so it rings out and its row is a line of boxes.
 *
 * It belongs to the pattern, so turning it off here says nothing about any other pattern.
 */
function setPitched(track, pitched) {
  // The row is about to show a different set of notes, and what is picked out is the notes
  // themselves.
  forgetPicked();
  setMix(track, { pitched }, "set_pattern_pitched", { pitched });
  if (!pitched && state.roll === track) closeRoll();
  drawTrackHeaders();
  showPitched();
}

function showPitched() {
  const instrument = state.roll === null ? null : trackById(state.roll);
  if (!instrument) return;
  // The track's name, worn in the colour of the pattern it belongs to.
  el.rollName.textContent = instrument.name;
  el.rollName.title = instrument.name;
}

const isBlack = (pitch) => BLACK_KEYS.includes(((pitch % 12) + 12) % 12);
const pitchName = (pitch) =>
  ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][
    ((pitch % 12) + 12) % 12
  ] + String(Math.floor(pitch / 12) - 1);

/* One step and one semitone, at the zoom the roll is drawn at. */
function rollCell() {
  return ROLL_CELL * state.rollZoom;
}

function semitone() {
  return SEMITONE * state.rollZoom;
}

/* Rows run high to low, the way a keyboard stands up. */
const pitchRow = (pitch) => HIGH_PITCH - pitch;
const rowPitch = (row) => HIGH_PITCH - row;

function drawRoll() {
  drawKeys();
  drawRollRuler();
  drawNotes();
  drawVelocity();
}

function drawKeys() {
  const ctx = el.keys.getContext("2d");
  const height = PITCHES * semitone();
  ctx.clearRect(0, 0, KEYS, height);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";

  for (let row = 0; row < PITCHES; row++) {
    const pitch = rowPitch(row);
    const y = row * semitone();
    // Black keys are drawn short, so the column reads as a keyboard rather than a list.
    const black = isBlack(pitch);
    const w = black ? KEYS * 0.62 : KEYS - 1;
    ctx.fillStyle = black ? "#15121c" : "#2b2636";
    ctx.fillRect(0, y, w, semitone() - 1);
    // The sampler's own pitch is the one that plays the sample as it was recorded.
    if (pitch === DEFAULT_PITCH) {
      ctx.fillStyle = "rgba(255,77,135,0.4)";
      ctx.fillRect(0, y, w, semitone() - 1);
    }
    if (pitch % 12 === 0 || pitch === DEFAULT_PITCH) {
      ctx.fillStyle = PALETTE.dim;
      ctx.fillText(pitchName(pitch), KEYS - 30, y + semitone() / 2);
    }
  }
}

function drawRollRuler() {
  const ctx = el.rollRuler.getContext("2d");
  const steps = rollSteps();
  const inPattern = stepsOf(state.open);
  const width = drawnWidth(el.rollRuler);
  ctx.clearRect(0, 0, width, HEAD);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";
  for (let step = 0; step < steps; step++) {
    const onBeat = step % STEPS_PER_BEAT === 0;
    const past = step >= inPattern;
    ctx.globalAlpha = past ? 0.4 : 1;
    ctx.fillStyle = onBeat ? PALETTE.dim : PALETTE.line;
    if (onBeat) {
      ctx.fillText(String(step / STEPS_PER_BEAT + 1), step * rollCell() + 3, 14);
    } else {
      ctx.fillRect(step * rollCell(), 13, 2, 1);
    }
  }
  ctx.globalAlpha = 1;
}

function drawNotes() {
  const ctx = el.notes.getContext("2d");
  const steps = rollSteps();
  const inPattern = stepsOf(state.open);
  const width = steps * rollCell();
  const height = PITCHES * semitone();
  ctx.clearRect(0, 0, width, height);

  // The keyboard's own stripes, so you can tell a C from an F at a glance.
  for (let row = 0; row < PITCHES; row++) {
    const pitch = rowPitch(row);
    ctx.fillStyle = isBlack(pitch) ? "#191621" : "#201c29";
    ctx.fillRect(0, row * semitone(), width, semitone() - 1);
    if (pitch % 12 === 0) {
      ctx.fillStyle = "rgba(0,0,0,0.35)";
      ctx.fillRect(0, row * semitone() + semitone() - 1, width, 1);
    }
  }

  // Past the end of the pattern, where a note can still go: it makes the pattern longer.
  // Shaded rather than fenced off, because drawing off the end is how a bar becomes two.
  const endX = inPattern * rollCell();
  ctx.fillStyle = "rgba(0,0,0,0.4)";
  ctx.fillRect(endX, 0, width - endX, height);

  // Beats and bars over the top.
  for (let step = 0; step <= steps; step++) {
    if (step % STEPS_PER_BEAT !== 0) continue;
    ctx.fillStyle = step % STEPS_PER_BAR === 0 ? "rgba(0,0,0,0.5)" : "rgba(0,0,0,0.25)";
    ctx.fillRect(step * rollCell(), 0, 1, height);
  }

  // And where the pattern ends, so you can see what you are about to lengthen.
  ctx.fillStyle = PALETTE.dim;
  ctx.fillRect(endX - 1, 0, 2, height);

  if (state.playing) {
    ctx.fillStyle = "rgba(255,215,94,0.10)";
    ctx.fillRect(state.step * rollCell(), 0, rollCell(), height);
  }

  const ink = colourOf(state.open);
  for (const note of rollNotes()) {
    if (note.pitch < LOW_PITCH || note.pitch > HIGH_PITCH) continue;
    const x = note.step * rollCell();
    const y = pitchRow(note.pitch) * semitone();
    const w = Math.max(4, note.length * rollCell() - 2);
    const live = state.playing && state.step >= note.step && state.step < note.step + note.length;
    ctx.fillStyle = live ? PALETTE.lit : ink;
    roundRect(ctx, x + 1, y + 1, w, semitone() - 3, 3);
    ctx.fill();
    // The right hand edge is the handle for how long it is, so it says so.
    ctx.fillStyle = "rgba(0,0,0,0.25)";
    ctx.fillRect(x + w - 2, y + 1, 2, semitone() - 3);
    // And an outline for one that has been picked out, because the next thing you press
    // happens to all of them.
    if (isPicked(state.roll, note)) {
      outlinePicked(ctx, () => roundRect(ctx, x + 1, y + 1, w, semitone() - 3, 3));
    }
  }

  if (state.marquee && state.marquee.where === "roll") drawMarquee(ctx, state.marquee);
}

function drawVelocity() {
  const ctx = el.velocity.getContext("2d");
  const width = rollSteps() * rollCell();
  ctx.clearRect(0, 0, width, VELOCITY);
  const endX = stepsOf(state.open) * rollCell();
  ctx.fillStyle = "rgba(0,0,0,0.35)";
  ctx.fillRect(endX, 0, width - endX, VELOCITY);
  const ink = colourOf(state.open);
  for (const note of rollNotes()) {
    const x = note.step * rollCell();
    const h = Math.max(2, (note.velocity / 127) * (VELOCITY - 8));
    ctx.fillStyle = ink;
    ctx.fillRect(x + 1, VELOCITY - h - 3, Math.max(3, rollCell() - 3), h);
  }
}

// --- drawing notes --------------------------------------------------------

/* Where in the roll a pointer is. */
function rollAt(event) {
  const rect = el.notes.getBoundingClientRect();
  const x = event.clientX - rect.left;
  const step = Math.floor(x / rollCell());
  const row = Math.floor((event.clientY - rect.top) / semitone());
  // Past the end of the pattern still counts: putting a note there lengthens the pattern.
  if (step < 0 || step >= rollSteps()) return null;
  if (row < 0 || row >= PITCHES) return null;
  return { step, pitch: rowPitch(row), x };
}

/* The note under a pointer, and whether it is being held by its right hand edge. */
function noteUnder(at) {
  for (const note of rollNotes()) {
    if (note.pitch !== at.pitch) continue;
    if (at.step < note.step || at.step >= note.step + note.length) continue;
    const end = (note.step + note.length) * rollCell();
    return { note, edge: end - at.x <= 7 };
  }
  return null;
}

/*
 * Where a pointer is in the roll as a plain point, clamped to the canvas. What the box you
 * drag round a set of notes is measured in: a box has to keep a corner where you started it
 * even once your hand has gone off the end of the notes.
 */
function rollPoint(event) {
  const rect = el.notes.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(rollSteps() * rollCell(), event.clientX - rect.left)),
    y: Math.max(0, Math.min(PITCHES * semitone(), event.clientY - rect.top)),
  };
}

/* Every note the box drawn in the roll touches. Touching counts, the way it does anywhere. */
function notesInBox(box) {
  const left = Math.min(box.from.x, box.to.x);
  const right = Math.max(box.from.x, box.to.x);
  const top = Math.min(box.from.y, box.to.y);
  const bottom = Math.max(box.from.y, box.to.y);
  const found = [];
  for (const note of rollNotes()) {
    const x = note.step * rollCell();
    const w = Math.max(1, note.length) * rollCell();
    const y = pitchRow(note.pitch) * semitone();
    if (x + w < left || x > right) continue;
    if (y + semitone() < top || y > bottom) continue;
    found.push({ track: state.roll, note });
  }
  return found;
}

let dragging = null;

el.notes.addEventListener("pointerdown", (e) => {
  const at = rollAt(e);
  if (!at) return;
  const pattern = openPatternNow();
  const under = noteUnder(at);

  if (erasing(e)) {
    // The right button rubs out. A note that is one of a set picked out takes the whole set
    // with it, which is what picking them out was for.
    if (under && isPicked(state.roll, under.note)) removePicked();
    else if (under) remove(under.note);
    return;
  }

  el.notes.setPointerCapture(e.pointerId);

  /*
   * Shift drags a box round notes rather than drawing one. It has to be shift: pressing on
   * an empty square already means "put a note there and drag out how long it is", which is
   * the best gesture in the roll and not one to give up for a selection box.
   */
  if (e.shiftKey) {
    if (under) {
      togglePicked(state.roll, under.note);
      return;
    }
    const corner = rollPoint(e);
    dragging = { mode: "box" };
    state.marquee = { where: "roll", from: corner, to: corner };
    state.needsDraw = true;
    return;
  }

  if (under && under.edge) {
    // Grabbed by the end: this is how long it is.
    dragging = { mode: "length", note: under.note, was: { ...under.note } };
    return;
  }
  if (under) {
    // A note out of a picked set moves the whole set; one that is not clears the set, so a
    // plain press is always about the note you pressed on.
    if (!isPicked(state.roll, under.note)) clearPicked();
    // Alt leaves the originals where they are and drags copies away, which is how one bar
    // becomes two without going near the clipboard.
    const copying = e.altKey;
    const moving = state.picked.length ? state.picked.map((one) => one.note) : [under.note];
    const held = copying ? copyNotesInPlace(pattern, state.roll, moving) : moving;
    const grabbed = copying ? held[moving.indexOf(under.note)] ?? held[0] : under.note;
    if (copying) pickNotes(held.map((note) => ({ track: state.roll, note })));
    dragging = {
      mode: "move",
      note: grabbed,
      was: { ...grabbed },
      copying,
      moving: held.map((note) => ({ note, was: { ...note } })),
      grab: { step: at.step - grabbed.step, pitch: at.pitch - grabbed.pitch },
    };
    invoke("audition", { id: state.roll, pitch: grabbed.pitch });
    return;
  }

  // Nothing there, so draw one — and keep hold of its end, so dragging straight on sets
  // how long it is.
  clearPicked();
  const note = {
    step: at.step,
    pitch: at.pitch,
    velocity: 100,
    length: Math.max(1, Math.min(state.drawLength, MAX_STEPS - at.step)),
  };
  notesFor(pattern, state.roll).push(note);
  state.needsDraw = true;
  invoke("audition", { id: state.roll, pitch: note.pitch });
  send(note);
  dragging = { mode: "length", note, was: { ...note }, fresh: true };
});

el.notes.addEventListener("pointermove", (e) => {
  if (!dragging) {
    // The same cursors as the song: the end of a note is a handle, the middle picks it up.
    const at = rollAt(e);
    const under = at && noteUnder(at);
    const cursor = e.shiftKey ? "crosshair" : !under ? "cell" : under.edge ? "ew-resize" : "grab";
    if (el.notes.style.cursor !== cursor) el.notes.style.cursor = cursor;
    return;
  }
  // Stretching a note into the edge of the window scrolls the roll along under it, so the
  // drag never runs out of room. Moving one does it both ways, because it can go anywhere.
  followEdge(el.rollScroll, e, dragging.mode === "length" ? "x" : "xy", rollDragTo);
  rollDragTo(e);
});

/*
 * Where the drag has got to. Taken apart from the event so that scrolling the view under a
 * still hand can put the drag through again: the pointer has not moved, but what it is over
 * has.
 */
function rollDragTo(point) {
  if (!dragging) return;

  if (dragging.mode === "box") {
    state.marquee.to = rollPoint(point);
    pickNotes(notesInBox(state.marquee), true);
    return;
  }

  const at = rollAt(point);
  if (!at) return;
  const note = dragging.note;

  if (dragging.mode === "length") {
    const length = Math.max(1, Math.min(at.step - note.step + 1, MAX_STEPS - note.step));
    if (length !== note.length) {
      note.length = length;
      state.needsDraw = true;
    }
    return;
  }

  // Moving, one note or a whole set of them: the one you grabbed follows the pointer and
  // the rest keep their places around it, so the shape of what you picked out is kept.
  const held = dragging.moving ?? [{ note, was: dragging.was }];
  let step = Math.max(0, Math.min(at.step - dragging.grab.step, MAX_STEPS - note.length));
  let pitch = Math.max(LOW_PITCH, Math.min(HIGH_PITCH, at.pitch - dragging.grab.pitch));
  let byStep = step - dragging.was.step;
  let byPitch = pitch - dragging.was.pitch;
  // No note in the set may be pushed off an end, so the whole set stops where the first of
  // them would have.
  for (const one of held) {
    byStep = Math.max(byStep, -one.was.step);
    byStep = Math.min(byStep, MAX_STEPS - one.was.length - one.was.step);
    byPitch = Math.max(byPitch, LOW_PITCH - one.was.pitch);
    byPitch = Math.min(byPitch, HIGH_PITCH - one.was.pitch);
  }
  if (byStep === note.step - dragging.was.step && byPitch === note.pitch - dragging.was.pitch) {
    return;
  }
  const heard = dragging.was.pitch + byPitch;
  if (heard !== note.pitch) invoke("audition", { id: state.roll, pitch: heard });
  for (const one of held) {
    one.note.step = one.was.step + byStep;
    one.note.pitch = one.was.pitch + byPitch;
  }
  state.needsDraw = true;
}

const dropNote = () => {
  if (!dragging) return;
  const drag = dragging;
  dragging = null;
  stopFollowing();
  const { note, was, mode } = drag;

  if (mode === "box") {
    state.marquee = null;
    state.needsDraw = true;
    sayPicked();
    return;
  }
  if (mode === "length") {
    state.drawLength = note.length;
    send(note);
    return;
  }
  const held = drag.moving ?? [{ note, was }];
  // Copies dropped where they were made are the notes that are already there, so nothing
  // has happened and the copies go away again.
  if (note.step === was.step && note.pitch === was.pitch) {
    if (drag.copying) undoCopyInPlace(held);
    return;
  }
  const landed = held.map((one) => one.note);
  // A note dropped on top of another takes its place, which is what Rust does with it too.
  tidyLane(state.roll, landed);
  if (drag.copying) {
    // Nothing to take out: the originals are still where they were, and these are new.
    sendNoteEdit(state.roll, [], landed);
    return;
  }
  if (held.length > 1) {
    // The old places out and the new notes in, in one go, so a note landing where another
    // has just left cannot be rubbed out by the one that left.
    sendNoteEdit(state.roll, held.map((one) => placeOf(one.was)), landed);
    return;
  }
  // One trip rather than two, so the note is never briefly nowhere.
  invoke("move_note", {
    pattern: state.open,
    track: state.roll,
    at: { step: was.step, pitch: was.pitch },
    to: { step: note.step, pitch: note.pitch },
  }).then((moved) => {
    if (!moved) {
      Object.assign(note, was);
      state.needsDraw = true;
    }
  });
};
el.notes.addEventListener("pointerup", dropNote);
el.notes.addEventListener("pointercancel", dropNote);
el.notes.addEventListener("pointerleave", () => {
  el.notes.style.cursor = "default";
});

/* Put a note where Rust can see it. Adding and changing one are the same thing. */
function send(note) {
  const pattern = state.open;
  invoke("set_note", {
    pattern,
    track: state.roll,
    at: { step: note.step, pitch: note.pitch },
    velocity: note.velocity,
    length: note.length,
  }).then((put) => {
    if (!put.fits) {
      remove(note, true);
      showError("that track is as full of notes as the engine will hold");
      return;
    }
    grewTo(pattern, put.steps);
  });
}

function remove(note, alreadyGone) {
  const notes = rollNotes();
  const at = notes.indexOf(note);
  if (at >= 0) notes.splice(at, 1);
  state.needsDraw = true;
  if (!alreadyGone) {
    invoke("clear_note", {
      pattern: state.open,
      track: state.roll,
      at: { step: note.step, pitch: note.pitch },
    });
  }
}

/* How hard each note is hit, dragged in the lane underneath. */
let velocityDrag = false;

function setVelocity(event) {
  const rect = el.velocity.getBoundingClientRect();
  const step = Math.floor((event.clientX - rect.left) / rollCell());
  const from = Math.round((1 - (event.clientY - rect.top) / (VELOCITY - 8)) * 127);
  const velocity = Math.max(1, Math.min(127, from));
  let changed = false;
  for (const note of rollNotes()) {
    if (note.step !== step) continue;
    if (note.velocity === velocity) continue;
    note.velocity = velocity;
    changed = true;
    send(note);
  }
  if (changed) state.needsDraw = true;
}

el.velocity.addEventListener("pointerdown", (e) => {
  velocityDrag = true;
  el.velocity.setPointerCapture(e.pointerId);
  setVelocity(e);
});
el.velocity.addEventListener("pointermove", (e) => {
  if (velocityDrag) setVelocity(e);
});
const stopVelocity = () => {
  velocityDrag = false;
};
el.velocity.addEventListener("pointerup", stopVelocity);
el.velocity.addEventListener("pointercancel", stopVelocity);

/* Click a key to hear the sample at that pitch. */
el.keys.addEventListener("pointerdown", (e) => {
  if (state.roll === null) return;
  const rect = el.keys.getBoundingClientRect();
  const row = Math.floor((e.clientY - rect.top) / semitone());
  if (row < 0 || row >= PITCHES) return;
  invoke("audition", { id: state.roll, pitch: rowPitch(row) });
});

// --- picking notes out ----------------------------------------------------

/*
 * A set of notes, picked out by dragging a box round them, and the four things worth doing
 * to a set: moving it, copying it, pasting it somewhere else, rubbing it out.
 *
 * It is held as the note objects themselves rather than as places, so a note that has been
 * dragged is still the same note afterwards. Which also means a set cannot outlive the
 * project it was picked out of: an undo hands over a whole new one, and everything picked
 * out is dropped along with the notes it pointed at.
 *
 * The same set works in both editors. In the roll it is the notes of the one instrument you
 * are looking at; in the step grid it can run across as many rows as the box covers, and
 * that is how a part gets from one instrument to another.
 */

function isPicked(track, note) {
  return state.picked.some((one) => one.track === track && one.note === note);
}

/* Pick out this set instead of whatever was picked out before. */
function pickNotes(picked, quietly) {
  state.picked = picked;
  state.needsDraw = true;
  if (!quietly) sayPicked();
}

/* Nothing picked out, and nothing part way through being picked out. */
function forgetPicked() {
  state.marquee = null;
  clearPicked();
  clearPickedBlocks();
}

function clearPicked() {
  if (!state.picked.length) return;
  state.picked = [];
  state.needsDraw = true;
}

/* Shift clicking one note puts it in the set, or takes it back out. */
function togglePicked(track, note) {
  const at = state.picked.findIndex((one) => one.track === track && one.note === note);
  if (at >= 0) state.picked.splice(at, 1);
  else state.picked.push({ track, note });
  state.needsDraw = true;
  sayPicked();
}

/*
 * What can be done with them now, said in the one place this app says anything.
 *
 * Plain letters rather than cmd-C and cmd-V, and that is not laziness: on a Mac the menu
 * bar's own Copy and Paste are handled before the window ever sees those keys — the same
 * trap cmd-Z falls into, which is why undo is a menu item — and taking them off the menu
 * would break copying and pasting in the one text field in the app.
 */
function sayPicked() {
  if (!state.picked.length) return;
  showNote(
    `${count(state.picked.length, "note")} picked out — c to copy, v to paste, delete to rub out`,
  );
}

const count = (many, thing) => `${many} ${thing}${many === 1 ? "" : "s"}`;

/* Where a note is: the two things that say which note it is inside a lane. */
const placeOf = (note) => ({ step: note.step, pitch: note.pitch });

/* The picked notes, gathered by the track they are in. */
function pickedByTrack() {
  const rows = new Map();
  for (const one of state.picked) {
    if (!rows.has(one.track)) rows.set(one.track, []);
    rows.get(one.track).push(one.note);
  }
  return rows;
}

/* Everything in the editor you are looking at, picked out at once. */
function pickEverything() {
  const pattern = openPatternNow();
  if (!pattern) return;
  if (state.roll !== null) {
    pickNotes(rollNotes().map((note) => ({ track: state.roll, note })));
    return;
  }
  const all = [];
  for (const track of state.tracks) {
    const pitched = pattern.mix.get(track.id)?.pitched;
    for (const note of pattern.notes.get(track.id) ?? []) {
      // What plays is what you can see, and what a box round them takes is the same: a row
      // of boxes only shows the notes a box can mean.
      if (!pitched && note.pitch !== DEFAULT_PITCH) continue;
      all.push({ track: track.id, note });
    }
  }
  pickNotes(all);
}

/* Rub the lot out, a lane at a time. */
function removePicked() {
  const open = openPatternNow();
  if (!open || !state.picked.length) return;
  const many = state.picked.length;
  for (const [track, notes] of pickedByTrack()) {
    const lane = notesFor(open, track);
    for (const gone of notes) {
      const at = lane.indexOf(gone);
      if (at >= 0) lane.splice(at, 1);
    }
    sendNoteEdit(track, notes.map(placeOf), []);
  }
  state.picked = [];
  state.needsDraw = true;
  showNote(`${count(many, "note")} rubbed out`);
}

/*
 * Copy what is picked out, and cut it if asked.
 *
 * Kept as rows rather than as track ids, and as copies of the notes rather than the notes:
 * what is remembered is how far down the set sat and what was in it, not which instrument it
 * came out of. That is the whole of pasting a drum part into a bass part — and of pasting it
 * into a different pattern, which the same clipboard does for free.
 */
function copyPicked(cut) {
  const open = openPatternNow();
  if (!open || !state.picked.length) return;
  const rows = new Map();
  for (const one of state.picked) {
    const row = state.tracks.findIndex((track) => track.id === one.track);
    if (row < 0) continue;
    if (!rows.has(row)) rows.set(row, []);
    rows.get(row).push({ ...one.note });
  }
  if (!rows.size) return;
  const base = Math.min(...rows.keys());
  const many = state.picked.length;
  state.clip = {
    pattern: open.id,
    base,
    rows: [...rows].map(([row, notes]) => ({ down: row - base, notes })),
  };
  if (cut) removePicked();
  showNote(
    state.roll !== null
      ? `${count(many, "note")} copied — open another instrument's roll and press v`
      : `${count(many, "note")} copied — point at an instrument's row and press v`,
  );
}

/* How much room the copied notes take, from the first of them to the end of the last. */
function clipSpan(clip) {
  let from = Infinity;
  let to = 0;
  for (const row of clip.rows) {
    for (const note of row.notes) {
      from = Math.min(from, note.step);
      to = Math.max(to, note.step + Math.max(1, note.length));
    }
  }
  return Math.max(1, to - from);
}

/*
 * Put the copy down. The notes keep their own steps and pitches: pasted into another
 * instrument they land in the same places in the bar, which is what copying a rhythm from
 * one instrument to another means.
 *
 * Straight back where it came from is the one case that would do nothing at all, so there it
 * lands after itself instead — copy a bar, press v, and there is the next one.
 */
function pasteNotes() {
  const open = openPatternNow();
  if (!open || !state.clip) return;
  // Where it lands: the instrument whose roll is open, or the row the pointer is over in the
  // step grid, or back where it was copied from.
  const landing =
    state.roll !== null
      ? state.tracks.findIndex((track) => track.id === state.roll)
      : (state.overRow ?? state.clip.base);
  if (landing < 0) return;
  const home = state.clip.pattern === open.id && landing === state.clip.base;
  const along = home ? clipSpan(state.clip) : 0;

  const together = new Map();
  for (const row of state.clip.rows) {
    // The roll has one lane to land in, so everything copied goes into that one.
    const at = state.roll !== null ? landing : landing + row.down;
    const track = state.tracks[at]?.id;
    if (track === undefined) continue;
    if (!together.has(track)) together.set(track, []);
    together.get(track).push(...row.notes.map((note) => ({ ...note, step: note.step + along })));
  }
  const { put, hidden } = putNotes(together);
  if (!put) return;
  showNote(
    hidden
      ? `${count(put, "note")} pasted — ${hidden} of them only show in the piano roll`
      : `${count(put, "note")} pasted`,
  );
}

/*
 * Put notes into lanes, and pick out what has just landed: what you have just put down is
 * what you are working on. Says how many of them a row of boxes cannot show, because a note
 * off middle C in a row of boxes is silent and invisible and that is worth knowing.
 */
function putNotes(together) {
  const open = openPatternNow();
  if (!open) return { put: 0, hidden: 0 };
  let hidden = 0;
  const landed = [];
  for (const [track, notes] of together) {
    const lane = notesFor(open, track);
    const fresh = notes.filter(
      (one) => !lane.some((note) => note.step === one.step && note.pitch === one.pitch),
    ).length;
    if (lane.length + fresh > MAX_NOTES) {
      showError("that track is as full of notes as the engine will hold");
      continue;
    }
    const made = [];
    for (const one of notes) {
      const copy = { ...one };
      const at = lane.findIndex((note) => note.step === copy.step && note.pitch === copy.pitch);
      // A note put where one already is takes its place, the same as drawing one there.
      if (at >= 0) lane[at] = copy;
      else lane.push(copy);
      made.push(copy);
      if (!isPitched(track) && copy.pitch !== DEFAULT_PITCH) hidden += 1;
    }
    sendNoteEdit(track, [], made);
    landed.push(...made.map((note) => ({ track, note })));
  }
  if (!landed.length) return { put: 0, hidden: 0 };
  pickNotes(landed, true);
  return { put: landed.length, hidden };
}

/* Copies of the picked notes, straight after themselves. */
function duplicatePicked() {
  if (!openPatternNow() || !state.picked.length) return;
  const notes = state.picked.map((one) => one.note);
  const from = Math.min(...notes.map((note) => note.step));
  const along = Math.max(
    1,
    Math.max(...notes.map((note) => note.step + Math.max(1, note.length))) - from,
  );
  const together = new Map();
  for (const [track, lot] of pickedByTrack()) {
    together.set(
      track,
      lot.map((note) => ({ ...note, step: note.step + along })),
    );
  }
  const { put } = putNotes(together);
  if (put) showNote(`${count(put, "note")} duplicated`);
}

/*
 * Copies of some notes, made in the same lane on top of the originals and ready to be
 * dragged off them. Nothing goes to Rust until they land somewhere.
 */
function copyNotesInPlace(pattern, track, notes) {
  const made = notes.map((one) => ({ ...one }));
  notesFor(pattern, track).push(...made);
  return made;
}

/* Copies dropped exactly where they were made are the notes that are already there. */
function undoCopyInPlace(held) {
  const open = openPatternNow();
  if (!open || state.roll === null) return;
  const lane = notesFor(open, state.roll);
  for (const one of held) {
    const at = lane.indexOf(one.note);
    if (at >= 0) lane.splice(at, 1);
  }
  state.picked = [];
  state.needsDraw = true;
}

/*
 * A lane holds one note per step and pitch, so notes dropped on top of others take their
 * place. Rust does this for itself when the notes arrive; this keeps the copy in here in
 * step with it.
 */
function tidyLane(track, keep) {
  const open = openPatternNow();
  if (!open) return;
  const lane = notesFor(open, track);
  const kept = new Set(keep);
  const taken = new Set(keep.map((note) => `${note.step}:${note.pitch}`));
  for (let at = lane.length - 1; at >= 0; at--) {
    if (kept.has(lane[at])) continue;
    if (taken.has(`${lane[at].step}:${lane[at].pitch}`)) lane.splice(at, 1);
  }
}

/*
 * Notes out and notes in, in one trip.
 *
 * Every edit to more than one note at a time goes through here. Rust takes the old ones out
 * before it puts the new ones in, so a note landing where another has just left cannot be
 * rubbed out by the one that left — and the whole thing is one step to take back rather
 * than one per note.
 */
function sendNoteEdit(track, remove, add) {
  const pattern = state.open;
  if (pattern === null) return;
  if (!remove.length && !add.length) return;
  invoke("edit_notes", {
    pattern,
    track,
    remove,
    add: add.map((note) => ({
      step: note.step,
      pitch: note.pitch,
      velocity: note.velocity,
      length: note.length,
    })),
  }).then((put) => {
    if (!put.fits) showError("that track is as full of notes as the engine will hold");
    grewTo(pattern, put.steps);
  });
}

/*
 * A note past the end made the pattern longer, so everything drawn from its length is drawn
 * again: the roll, the boxes, and the count in the panel.
 */
function grewTo(pattern, steps) {
  const grew = patternById(pattern);
  if (!grew || !steps || grew.steps === steps) return;
  grew.steps = steps;
  if (state.open === pattern) el.steps.value = String(steps);
  drawPatternPanel();
  resize();
}

/* The box being dragged round some notes, in either editor. */
function drawMarquee(ctx, box, scrolled = 0) {
  const x = Math.min(box.from.x, box.to.x) - scrolled;
  const y = Math.min(box.from.y, box.to.y);
  const w = Math.abs(box.to.x - box.from.x);
  const h = Math.abs(box.to.y - box.from.y);
  ctx.fillStyle = "rgba(255,255,255,0.07)";
  ctx.fillRect(x, y, w, h);
  ctx.strokeStyle = "rgba(255,255,255,0.5)";
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.strokeRect(x + 0.5, y + 0.5, w, h);
  ctx.setLineDash([]);
}

/*
 * And the outline that says a note is one of the ones picked out. Takes something that lays
 * a path down and strokes it, so the shape of the outline is the shape of what it is round.
 */
function outlinePicked(ctx, path) {
  ctx.strokeStyle = "rgba(255,255,255,0.92)";
  ctx.lineWidth = 1.5;
  path();
  ctx.stroke();
}

// --- the sound editor -----------------------------------------------------

/*
 * One track's instrument, rather than one pattern's part.
 *
 * The row in a pattern says how loud this sound is *there* and whether it is heard *there*.
 * All of that is writing the part, so it belongs to the pattern. What is in here is what the
 * sound *is* — how sharply it starts, how long it hangs on, where it sits between the
 * speakers, how it is tuned, how much of the file a note uses — and that is the same
 * wherever it is played, so it belongs to the track. Change the snare's tail here and every
 * pattern using the snare hears it.
 *
 * You get in through the little waveform on the row, because the waveform is the sound.
 * Clicking a track's name plays it; clicking its picture asks about it.
 */

/*
 * The controls, as data. Each one says what it reads and writes on the voicing, how its
 * slider maps onto that, and how to say the value out loud.
 *
 * Times get a squared slider. Attack is the difference between nought and thirty
 * milliseconds far more often than between one second and two, and a linear slider spends
 * nine tenths of its travel on the part nobody wants.
 */
const SOUND_MAX_TIME = 4; // seconds, the longest any one stage of the envelope goes

const squared = (max) => ({
  toValue: (t) => t * t * max,
  toSlider: (v) => Math.sqrt(Math.max(0, v) / max),
});
const straight = (min, max) => ({
  toValue: (t) => min + t * (max - min),
  toSlider: (v) => (v - min) / (max - min),
});

const millis = (v) => (v < 0.1 ? `${Math.round(v * 1000)} ms` : `${v.toFixed(2)} s`);
const percent = (v) => `${Math.round(v * 100)}%`;

const SOUND_CONTROLS = {
  shape: [
    { key: "attack", label: "attack", ...squared(SOUND_MAX_TIME), say: millis,
      hint: "How long a note takes to come up. Nothing is instant: a hard start clicks." },
    { key: "decay", label: "decay", ...squared(SOUND_MAX_TIME), say: millis,
      hint: "How long it takes to fall from full to where it sits." },
    { key: "sustain", label: "sustain", ...straight(0, 1), say: percent,
      hint: "Where it sits while the note is held. Full means the decay does nothing." },
    { key: "release", label: "release", ...squared(SOUND_MAX_TIME), say: millis,
      hint: "How long the tail is once the note ends." },
  ],
  tone: [
    {
      key: "pan",
      label: "pan",
      ...straight(-1, 1),
      say: (v) =>
        Math.abs(v) < 0.02
          ? "middle"
          : `${Math.round(Math.abs(v) * 100)}% ${v < 0 ? "left" : "right"}`,
      hint: "Where it sits between the speakers.",
    },
    {
      key: "tune",
      label: "tune",
      ...straight(-24, 24),
      say: (v) => (Math.abs(v) < 0.005 ? "as recorded" : `${v > 0 ? "+" : ""}${v.toFixed(2)} st`),
      hint: "Semitones up or down, on top of whatever pitch a note is.",
    },
    { key: "level", label: "level", ...straight(0, 2), say: percent,
      hint: "How loud the sound itself is, wherever it is played." },
  ],
  trim: [
    { key: "start", label: "starts at", ...straight(0, 1), say: percent,
      hint: "How far into the file a note starts. Drag the left hand end of the waveform too." },
    { key: "end", label: "ends at", ...straight(0, 1), say: percent,
      hint: "And where it stops. Drag the right hand end of the waveform too." },
  ],
};

/*
 * The one control of ours a plugin track keeps: how loud the track is. Everything else in a
 * voicing — the envelope, the tuning, the panning — the plugin has its own of, and two sets
 * of them would only fight. Named apart from the plugin's own "level", which is a different
 * knob a few rows down.
 */
const TRACK_LEVEL = {
  ...SOUND_CONTROLS.tone.find((one) => one.key === "level"),
  label: "track level",
  hint: "How loud this track is, wherever it is played. Not one of the plugin's own controls.",
};

/* The voicing of whichever track the editor has, or a default one when it has none. */
function voicingOf(track) {
  return trackById(track)?.voicing ?? DEFAULT_VOICING();
}

/* What a track that nobody has shaped sounds like. Must match Voicing::default in Rust. */
const DEFAULT_VOICING = () => ({
  attack: 0.002,
  decay: 0,
  sustain: 1,
  release: 0.003,
  pan: 0,
  tune: 0,
  level: 1,
  start: 0,
  end: 1,
});

function openSound(track) {
  if (state.open === null || trackById(track) === null) return;
  state.sound = track;
  showView();
  // Through resize, because the two pictures in here are the shape of the window and this
  // is the first moment there is a window to measure.
  resize();
  // What a plugin's controls are is the plugin's to say, and it may have moved them since
  // we last looked — and so is whether its window is still up.
  if (isPlugin(track)) {
    refreshParams(track);
    refreshWindow(track);
  }
}

function closeSound() {
  state.sound = null;
  state.trimming = null;
  showView();
  resize();
}

el.closeSound.addEventListener("click", closeSound);
el.hearSound.addEventListener("click", () => {
  if (state.sound !== null) invoke("audition", { id: state.sound });
});

/*
 * Change one thing about the sound, here and in Rust.
 *
 * Rust hands back what it settled on rather than what was asked for — the ends of a trim
 * cannot cross, the times have a ceiling — so the controls show what the sound actually is
 * and not what you tried to make it.
 */
async function setVoicing(change) {
  const track = trackById(state.sound);
  if (!track) return;
  const asked = { ...voicingOf(state.sound), ...change };
  // Shown straight away, so a dragged slider is never a frame behind your hand.
  track.voicing = asked;
  drawSound();
  const settled = await invoke("set_voicing", { id: track.id, voicing: asked });
  const still = trackById(track.id);
  if (!still) return;
  still.voicing = settled;
  // Only redraw for a value Rust would not have: a redraw per frame of a drag would fight
  // the slider you are holding.
  if (SOUND_KEYS.some((key) => Math.abs(settled[key] - asked[key]) > 1e-6)) drawSound();
}

const SOUND_KEYS = Object.keys(DEFAULT_VOICING());

/* Every control, in the panel it belongs to. Built once per track the editor opens. */
function buildSoundControls() {
  for (const [group, where] of [
    ["shape", el.soundShapeKnobs],
    ["tone", el.soundToneKnobs],
    ["trim", el.soundTrimKnobs],
  ]) {
    where.replaceChildren(...SOUND_CONTROLS[group].map(soundControl));
  }
}

/*
 * One labelled slider with its value beside it. The value is a read-out rather than a field:
 * "40 ms" and "70% left" say what the number means, and nobody types a pan.
 */
function soundControl(spec) {
  const row = document.createElement("label");
  row.className = "knob";
  row.title = spec.hint;

  const label = document.createElement("span");
  label.className = "knob-label";
  label.textContent = spec.label;

  const slider = document.createElement("input");
  slider.type = "range";
  slider.min = "0";
  slider.max = "1000";
  slider.step = "1";
  slider.dataset.key = spec.key;

  const read = document.createElement("span");
  read.className = "knob-read";
  read.dataset.read = spec.key;

  slider.addEventListener("input", () => {
    setVoicing({ [spec.key]: spec.toValue(Number(slider.value) / 1000) });
  });
  // Double click puts one control back where it started, which is the only way back to
  // "exactly none of this" once you have moved it.
  row.addEventListener("dblclick", () => {
    setVoicing({ [spec.key]: DEFAULT_VOICING()[spec.key] });
  });

  row.append(label, slider, read);
  return row;
}

/*
 * The whole editor. Two halves, one at a time: a sampler's file and shape, or a plugin's own
 * controls. A synth has an envelope, a tuning and a panning of its own, and a second set of
 * ours next to them would only be two things fighting over one sound.
 */
function drawSound() {
  const track = trackById(state.sound);
  if (!track) return;
  el.soundName.textContent = track.plugin ? track.plugin.name : track.name;
  el.soundName.title = track.plugin ? `${track.plugin.name} — ${track.plugin.path}` : track.name;
  if (el.soundShapeKnobs.childElementCount === 0) buildSoundControls();

  const plugged = Boolean(track.plugin);
  el.soundBody.classList.toggle("hidden", plugged);
  el.pluginBody.classList.toggle("hidden", !plugged);
  // Auditioning a plugin means playing it a note, which it has no way to refuse; the sampler
  // it was written for is what "hear it" means. A plugin is played from the pattern instead.
  el.hearSound.classList.toggle("hidden", plugged);
  if (plugged) {
    // How loud the track is still belongs to us: it is about the track, not about the synth.
    // Named for what it is, because the plugin almost certainly has a "level" of its own a
    // few rows below and the two are not the same knob.
    if (el.pluginLevelKnobs.childElementCount === 0) {
      el.pluginLevelKnobs.replaceChildren(soundControl(TRACK_LEVEL));
    }
    const value = voicingOf(state.sound)[TRACK_LEVEL.key];
    const slider = el.pluginLevelKnobs.querySelector(`input[data-key="${TRACK_LEVEL.key}"]`);
    const read = el.pluginLevelKnobs.querySelector(`[data-read="${TRACK_LEVEL.key}"]`);
    if (slider && document.activeElement !== slider) {
      slider.value = String(Math.round(TRACK_LEVEL.toSlider(value) * 1000));
    }
    if (read) read.textContent = TRACK_LEVEL.say(value);
    drawWindowButton(state.sound);
    drawPluginEditor(state.sound);
    return;
  }

  const voicing = voicingOf(state.sound);
  for (const group of Object.values(SOUND_CONTROLS)) {
    for (const spec of group) {
      const slider = el.soundBody.querySelector(`input[data-key="${spec.key}"]`);
      const read = el.soundBody.querySelector(`[data-read="${spec.key}"]`);
      if (slider && document.activeElement !== slider) {
        slider.value = String(Math.round(spec.toSlider(voicing[spec.key]) * 1000));
      }
      if (read) read.textContent = spec.say(voicing[spec.key]);
    }
  }
  drawSoundWave();
  drawSoundEnvelope();
}

/*
 * The file, with the part a note actually reads picked out in the pattern's colour and the
 * trimmed off ends left dim. The handles are the edges of the lit part: there is nothing to
 * find, because the thing you drag is the thing you can see.
 */
function drawSoundWave() {
  const track = trackById(state.sound);
  if (!track) return;
  const ctx = el.soundWave.getContext("2d");
  const w = drawnWidth(el.soundWave);
  const h = drawnHeight(el.soundWave);
  const voicing = voicingOf(state.sound);
  ctx.clearRect(0, 0, w, h);

  const peaks = track.peaks ?? [];
  const from = voicing.start * w;
  const to = voicing.end * w;
  const colour = colourOf(state.open);
  const step = w / Math.max(1, peaks.length);
  for (let i = 0; i < peaks.length; i++) {
    const x = i * step;
    const inside = x + step / 2 >= from && x + step / 2 <= to;
    ctx.fillStyle = inside ? colour : PALETTE.line;
    const bar = Math.max(1, peaks[i] * (h - 16));
    ctx.fillRect(x, (h - bar) / 2, Math.max(1, step - 1), bar);
  }

  // The two ends, as full height grips.
  ctx.fillStyle = colour;
  for (const x of [from, to]) {
    ctx.fillRect(Math.min(w - 3, Math.max(0, x - 1.5)), 0, 3, h);
  }
}

/*
 * The envelope, drawn as the shape it is.
 *
 * Four numbers do not tell you what a sound will do and a shape does, so this is the real
 * read-out and the sliders under it are how you move it. The hold in the middle is a fixed
 * slice of the width rather than a real length, because how long a note is held is the
 * pattern's business and this is only about the shape.
 */
const ENVELOPE_HOLD = 0.22; // of the width, given over to the sustain

function drawSoundEnvelope() {
  const ctx = el.soundEnv.getContext("2d");
  const w = drawnWidth(el.soundEnv);
  const h = drawnHeight(el.soundEnv);
  ctx.clearRect(0, 0, w, h);
  const voicing = voicingOf(state.sound);

  const pad = 8;
  const floor = h - pad;
  const ceiling = pad;
  const level = (v) => floor - v * (floor - ceiling);

  // The three timed stages share what is left after the hold, in proportion to how long they
  // are — so a long release really does look longer than a short attack.
  const times = [voicing.attack, voicing.decay, voicing.release];
  const total = times.reduce((a, b) => a + b, 0);
  const room = w - pad * 2;
  const spread = room * (1 - ENVELOPE_HOLD);
  const widths = total > 0 ? times.map((t) => (t / total) * spread) : [0, 0, 0];

  const points = [];
  let x = pad;
  points.push([x, level(0)]);
  x += widths[0];
  points.push([x, level(1)]);
  x += widths[1];
  points.push([x, level(voicing.sustain)]);
  x += room * ENVELOPE_HOLD;
  points.push([x, level(voicing.sustain)]);
  x += widths[2];
  points.push([x, level(0)]);

  const colour = colourOf(state.open);
  ctx.beginPath();
  ctx.moveTo(points[0][0], floor);
  for (const [px, py] of points) ctx.lineTo(px, py);
  ctx.lineTo(points[points.length - 1][0], floor);
  ctx.closePath();
  ctx.fillStyle = tint(colour, 0.22);
  ctx.fill();

  ctx.beginPath();
  ctx.moveTo(points[0][0], points[0][1]);
  for (const [px, py] of points.slice(1)) ctx.lineTo(px, py);
  ctx.strokeStyle = colour;
  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  ctx.stroke();

  // The line the note is let go on, which is where the release starts.
  const releaseAt = points[3][0];
  ctx.setLineDash([3, 3]);
  ctx.beginPath();
  ctx.moveTo(releaseAt, ceiling - 4);
  ctx.lineTo(releaseAt, floor);
  ctx.strokeStyle = PALETTE.dim;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.setLineDash([]);
}

/*
 * Trimming, by dragging either end of the lit part. Anywhere else in the waveform plays the
 * sound as it now is, because the whole point of trimming is hearing where it starts.
 */
const TRIM_GRAB = 8; // pixels either side of an end that count as grabbing it

el.soundWave.addEventListener("pointerdown", (e) => {
  if (state.sound === null) return;
  const w = drawnWidth(el.soundWave);
  const at = (e.clientX - el.soundWave.getBoundingClientRect().left) / Math.max(1, w);
  const voicing = voicingOf(state.sound);
  const grab = TRIM_GRAB / Math.max(1, w);
  const ends = [
    ["start", voicing.start],
    ["end", voicing.end],
  ]
    .map(([which, value]) => [which, Math.abs(value - at)])
    .filter(([, away]) => away <= grab)
    .sort((a, b) => a[1] - b[1]);

  if (!ends.length) {
    invoke("audition", { id: state.sound });
    return;
  }
  state.trimming = ends[0][0];
  el.soundWave.setPointerCapture(e.pointerId);
});

el.soundWave.addEventListener("pointermove", (e) => {
  if (!state.trimming) {
    // A pointer near an end says so before you press, which is the only way to know the
    // ends are draggable at all.
    const w = drawnWidth(el.soundWave);
    const at = (e.clientX - el.soundWave.getBoundingClientRect().left) / Math.max(1, w);
    const voicing = voicingOf(state.sound);
    const near =
      Math.min(Math.abs(voicing.start - at), Math.abs(voicing.end - at)) <=
      TRIM_GRAB / Math.max(1, w);
    el.soundWave.classList.toggle("grabbing", near);
    return;
  }
  const w = drawnWidth(el.soundWave);
  const at = (e.clientX - el.soundWave.getBoundingClientRect().left) / Math.max(1, w);
  setVoicing({ [state.trimming]: Math.min(1, Math.max(0, at)) });
});

const stopTrimming = () => {
  if (!state.trimming) return;
  state.trimming = null;
  // Rust may have pushed an end back off the other one; show where they really are.
  drawSound();
};
el.soundWave.addEventListener("pointerup", stopTrimming);
el.soundWave.addEventListener("pointercancel", stopTrimming);

// --- CLAP plugins ---------------------------------------------------------

/*
 * A track's sound can be a CLAP instrument — Surge XT, say — instead of a file.
 *
 * The picker is a list rather than a file dialog: a plugin is not a file you go and find,
 * it is installed software, and a host is supposed to know where those live. Rust scans the
 * standard folders once and remembers what it found.
 *
 * Everything after that is the same as any other track. Notes go in, sound comes out, and
 * the pattern's row says how loud it is here and whether it is heard here.
 */

/* How many parameters the editor will draw at once. Surge XT has thousands. */
const MAX_SHOWN_PARAMS = 120;

/* How long after the last drag before asking the plugin what its controls now say. */
const PARAM_SETTLE = 250;

const plugins = {
  found: null, // what the last scan turned up, or null before the first one
  loading: false,
  filter: "",
  params: new Map(), // track id -> the plugin's parameters, as Rust last described them
  windows: new Set(), // tracks whose plugin has its own window up
  settling: null, // the timer waiting for a drag to finish
};

el.addPlugin.addEventListener("click", () => openPicker());
el.closePicker.addEventListener("click", closePicker);
el.rescan.addEventListener("click", () => openPicker(true));
el.pluginFilter.addEventListener("input", () => {
  plugins.filter = el.pluginFilter.value.trim().toLowerCase();
  drawPluginList();
});

async function openPicker(again = false) {
  el.picker.classList.remove("hidden");
  el.pluginFilter.focus();
  el.pluginFilter.select();
  if (plugins.found !== null && !again) {
    drawPluginList();
    return;
  }
  plugins.found = null;
  drawPluginList();
  try {
    plugins.found = await invoke("list_plugins", { again });
  } catch (e) {
    plugins.found = [];
    showError(e);
  }
  drawPluginList();
}

function closePicker() {
  el.picker.classList.add("hidden");
}

/*
 * The list, filtered. Effects are shown greyed out rather than left out: "why is my plugin
 * not here" deserves an answer, and "that one is an effect, and effects are stage five" is
 * a better one than an empty list.
 */
function drawPluginList() {
  if (plugins.found === null) {
    el.pluginList.replaceChildren();
    el.pickerNote.textContent = "Looking through the plugin folders…";
    return;
  }
  const wanted = plugins.found.filter((one) =>
    `${one.name} ${one.vendor}`.toLowerCase().includes(plugins.filter),
  );
  el.pluginList.replaceChildren(
    ...wanted.map((one) => {
      const row = document.createElement("button");
      row.className = `plugin-row${one.instrument ? "" : " not-ours"}`;
      row.disabled = !one.instrument || plugins.loading;

      const name = document.createElement("span");
      name.className = "plugin-name";
      name.textContent = one.name;

      const vendor = document.createElement("span");
      vendor.className = "plugin-vendor";
      vendor.textContent = one.instrument ? one.vendor : `${one.vendor} — not an instrument`;

      row.append(name, vendor);
      row.title = one.path;
      row.addEventListener("click", () => addPlugin(one));
      return row;
    }),
  );

  if (!plugins.found.length) {
    el.pickerNote.textContent =
      "No CLAP plugins found. They live in ~/Library/Audio/Plug-Ins/CLAP on a Mac.";
  } else if (!wanted.length) {
    el.pickerNote.textContent = `Nothing matching that, out of ${plugins.found.length}.`;
  } else {
    el.pickerNote.textContent = "";
  }
}

/* Load one onto a new track. Takes a moment: a big synth is a lot to read off the disk. */
async function addPlugin(one) {
  if (plugins.loading) return;
  plugins.loading = true;
  drawPluginList();
  showWarning(`loading ${one.name}…`);
  try {
    const made = await invoke("add_plugin", { path: one.path, id: one.id });
    state.tracks.push({ ...made.track, peaks: made.peaks });
    closePicker();
    clearStatus();
    drawTrackHeaders();
    resize();
  } catch (e) {
    showError(e);
  } finally {
    plugins.loading = false;
    drawPluginList();
  }
}

const isPlugin = (track) => Boolean(trackById(track)?.plugin);

/*
 * The plugin's own window: Surge XT's real interface, in a window of its own.
 *
 * Rust owns whether it is up, because the window has a close box of its own — the plugin's or
 * ours — and clicking it need not reach us until the next time round. So this asks rather than
 * remembers, whenever there is a reason to think the answer may have changed.
 */
async function refreshWindow(track) {
  try {
    const open = await invoke("plugin_window_open", { id: track });
    showWindowState(track, open);
  } catch (e) {
    showError(e);
  }
}

function showWindowState(track, open) {
  if (open) plugins.windows.add(track);
  else plugins.windows.delete(track);
  if (state.sound === track) drawWindowButton(track);
}

/*
 * The plug on a track's row, pressed: the plugin's own window up, or down again.
 *
 * Rust is asked first rather than trusted from here, because the window has a close box of
 * its own and using it need not reach us until somebody asks. A plugin that has no window at
 * all says so, and then our own controls are all there is, so that is where you land.
 */
async function togglePluginWindow(track) {
  try {
    const up = await invoke("plugin_window_open", { id: track });
    showWindowState(track, await invoke("set_plugin_window", { id: track, open: !up }));
  } catch (e) {
    showError(e);
    showWindowState(track, false);
    openSound(track);
  }
  drawTrackHeaders();
}

function drawWindowButton(track) {
  const open = plugins.windows.has(track);
  el.pluginWindow.textContent = open ? "close its window" : "open its window";
  el.pluginWindow.classList.toggle("on", open);
}

el.pluginWindow.addEventListener("click", async () => {
  const track = state.sound;
  if (track === null || !isPlugin(track)) return;
  const wanted = !plugins.windows.has(track);
  el.pluginWindow.disabled = true;
  try {
    showWindowState(track, await invoke("set_plugin_window", { id: track, open: wanted }));
  } catch (e) {
    // "This plugin has no window of its own" is the usual one, and is worth saying plainly
    // rather than leaving a button that looks like it did nothing. The window may also be
    // one of ours, in which case it can fail to be made, which is worth saying too.
    showError(e);
    await refreshWindow(track);
  } finally {
    el.pluginWindow.disabled = false;
  }
});

/*
 * A plugin's parameters, from Rust. Asked for when the editor opens and again when a drag
 * has settled, because a plugin can move its own controls — loading a patch moves all of
 * them at once — and what it says they are is the truth.
 */
async function refreshParams(track) {
  try {
    const params = await invoke("plugin_params", { id: track });
    plugins.params.set(track, params);
    if (state.sound === track) drawSound();
  } catch (e) {
    showError(e);
  }
}

/*
 * Move one. Straight to the audio thread, so you hear it as you drag; the read-out shows the
 * plain number while you are moving it and goes back to what the plugin calls that value
 * once you let go, because only the plugin knows how to say it.
 */
function setParam(track, param, value) {
  invoke("set_plugin_param", { id: track, param, value });
  const params = plugins.params.get(track) ?? [];
  const found = params.find((one) => one.id === param);
  if (found) {
    found.value = value;
    found.text = "";
  }
  clearTimeout(plugins.settling);
  plugins.settling = setTimeout(() => {
    refreshParams(track);
    refreshWindow(track);
  }, PARAM_SETTLE);
}

/* One parameter: what it is called, a slider, and what the plugin calls the value. */
function paramControl(track, param) {
  const row = document.createElement("label");
  row.className = "knob";
  row.title = param.module ? `${param.module} / ${param.name}` : param.name;

  const label = document.createElement("span");
  label.className = "knob-label";
  label.textContent = param.name;

  const slider = document.createElement("input");
  slider.type = "range";
  slider.min = "0";
  slider.max = "1000";
  slider.step = "1";
  const span = param.max - param.min || 1;
  slider.value = String(Math.round(((param.value - param.min) / span) * 1000));

  const read = document.createElement("span");
  read.className = "knob-read";
  const say = () =>
    param.text || (Math.abs(param.value) >= 100 ? param.value.toFixed(0) : param.value.toFixed(3));
  read.textContent = say();

  slider.addEventListener("input", () => {
    let value = param.min + (Number(slider.value) / 1000) * span;
    // A stepped parameter is a menu or a switch: anything between two of its values is not
    // one of its values.
    if (param.stepped) value = Math.round(value);
    param.value = value;
    param.text = "";
    read.textContent = say();
    setParam(track, param.id, value);
  });

  row.append(label, slider, read);
  return row;
}

/*
 * The plugin half of the sound editor. The sampler's controls are put away: a synth has its
 * own envelope, its own tuning and its own panning, and a second set of ours would only
 * fight them. What is left of ours is how loud the track is, which is about the track.
 */
function drawPluginEditor(track) {
  const params = plugins.params.get(track);
  const filter = el.paramFilter.value.trim().toLowerCase();

  if (!params) {
    el.pluginParams.replaceChildren(hint("Asking the plugin what it has…"));
    return;
  }
  if (!params.length) {
    el.pluginParams.replaceChildren(
      hint("This plugin has no controls to show. Its own window is stage six."),
    );
    return;
  }
  const wanted = params.filter((one) =>
    `${one.module} ${one.name}`.toLowerCase().includes(filter),
  );
  const shown = wanted.slice(0, MAX_SHOWN_PARAMS);
  el.pluginParams.replaceChildren(...shown.map((one) => paramControl(track, one)));
  if (wanted.length > shown.length) {
    el.pluginParams.append(
      hint(`and ${wanted.length - shown.length} more — type above to narrow it down`),
    );
  }
  if (!wanted.length) {
    el.pluginParams.replaceChildren(hint(`Nothing matching that, out of ${params.length}.`));
  }
}

function hint(text) {
  const line = document.createElement("p");
  line.className = "small";
  line.textContent = text;
  return line;
}

el.paramFilter.addEventListener("input", () => {
  if (state.sound !== null && isPlugin(state.sound)) drawPluginEditor(state.sound);
});

// --- the song -------------------------------------------------------------

/*
 * A block is a pattern put somewhere in the song, for as long as you drag it out to be. It
 * starts wherever the snap puts it — not on a grid worked out from the pattern's length, so
 * a thirty two step pattern can start on a half bar — and a block longer than its pattern
 * repeats it. Drag the middle to move it, either end to change where that end is.
 *
 * That is also the fix for blocks that could not be clicked: a block is hit tested where it
 * actually is, over its whole length, rather than by working out which slot of a grid a
 * click landed in.
 */

/* One step of the song, in pixels, at the zoom it is drawn at. */
function songStep() {
  return SONG_STEP * state.zoom;
}

function barPx() {
  return STEPS_PER_BAR * songStep();
}

/*
 * Two ways of landing on the snap. Drawing a block means "in this bar", so it goes to the
 * start of the one you clicked in. Dragging an edge means "up to that line", so it goes to
 * the nearest — otherwise the far edge of a bar would be four pixels wide to aim at.
 */
function snapFloor(step) {
  const snap = Math.max(1, state.snap);
  return Math.max(0, Math.floor(step / snap) * snap);
}

function snapNear(step) {
  const snap = Math.max(1, state.snap);
  return Math.max(0, Math.round(step / snap) * snap);
}

/* The colour a pattern's blocks are drawn in: the one it was given, or one from its place. */
function blockColour(pattern, row) {
  const pick = pattern.colour ?? row;
  return BLOCK_COLOURS[((pick % BLOCK_COLOURS.length) + BLOCK_COLOURS.length) % BLOCK_COLOURS.length];
}

/*
 * The same colour, looked up by the pattern alone. What the editor and the roll draw in, so
 * the notes inside a block are the colour of the block they are in.
 */
function colourOf(id) {
  const at = state.patterns.findIndex((pattern) => pattern.id === id);
  return at < 0 ? PALETTE.accent : blockColour(state.patterns[at], at);
}

/* The same colour, lifted towards white. What a block that is sounding right now looks like. */
function lighten(hex, amount) {
  const n = parseInt(hex.slice(1), 16);
  const mix = (channel) => Math.round(channel + (255 - channel) * amount);
  return `rgb(${mix((n >> 16) & 255)},${mix((n >> 8) & 255)},${mix(n & 255)})`;
}

/* And barely there, for the row of a pattern that is open. */
function tint(hex, alpha) {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${alpha})`;
}

/* The block of a pattern covering a step, if there is one. */
function placementAt(pattern, step) {
  return (
    state.song.find(
      (one) => one.pattern === pattern && step >= one.step && step < one.step + Math.max(1, one.length),
    ) ?? null
  );
}

/* Where the song ends, rounded up to a bar so it loops somewhere musical. */
function songSteps() {
  let end = 0;
  for (const one of state.song) {
    end = Math.max(end, one.step + Math.max(1, one.length));
  }
  return Math.ceil(end / STEPS_PER_BAR) * STEPS_PER_BAR;
}

/*
 * Where the playhead is across the song, in pixels. Part way through a step only while it
 * is playing: stopped, it sits on the step it is on, which is where you dragged it to.
 */
function songPlayheadX() {
  return (state.step + (state.playing ? state.progress : 0)) * songStep();
}

function drawSong() {
  drawScrubber();
  drawLanes();
}

/*
 * The canvases are only as wide as the window, so everything is drawn relative to how far
 * the song has been scrolled. Off screen bars and blocks cost nothing.
 */
function songLeft() {
  return el.songScroll.scrollLeft;
}

function drawScrubber() {
  const ctx = el.scrubber.getContext("2d");
  const width = drawnWidth(el.scrubber);
  const height = HEAD - 1;
  const left = songLeft();
  ctx.clearRect(0, 0, width, height);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";

  const bars = songBars();
  const bar = barPx();
  const playingBar = Math.floor(state.step / STEPS_PER_BAR);
  // Zoomed a long way out there is no room to number every bar, so number the phrases.
  const every = bar < 26 ? 4 : 1;
  const first = Math.max(0, Math.floor(left / bar));
  const last = Math.min(bars, Math.ceil((left + width) / bar) + 1);
  for (let n = first; n < last; n++) {
    const x = n * bar - left;
    const phrase = n % 4 === 0;
    ctx.fillStyle = PALETTE.line;
    ctx.fillRect(x, phrase ? 8 : 13, 1, height - 8);
    if (n % every !== 0) continue;
    ctx.fillStyle = n === playingBar ? PALETTE.lit : PALETTE.dim;
    ctx.fillText(String(n + 1), x + 4, 15);
  }

  // The playhead, drawn whether or not anything is playing: it is a handle as well as a
  // read-out, and a handle you cannot see is not one.
  const x = songPlayheadX() - left;
  ctx.fillStyle = state.playing ? PALETTE.lit : PALETTE.dim;
  ctx.beginPath();
  ctx.moveTo(x - 5, 1);
  ctx.lineTo(x + 5, 1);
  ctx.lineTo(x, 10);
  ctx.closePath();
  ctx.fill();
  ctx.fillRect(x, 1, 1, height - 1);
}

function drawLanes() {
  const ctx = el.lanes.getContext("2d");
  const width = drawnWidth(el.lanes);
  const height = Math.max(LANE, state.patterns.length * LANE);
  const step = songStep();
  const bar = barPx();
  const left = songLeft();
  ctx.clearRect(0, 0, width, height);
  ctx.font = "11px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";

  // The grid every lane shares: the snap you are working at, with the bars over the top.
  const lanes = shownPatterns();
  const snapPx = Math.max(3, Math.max(1, state.snap) * step);
  const firstSnap = Math.floor(left / snapPx);
  const firstBar = Math.floor(left / bar);
  for (let row = 0; row < Math.max(1, lanes.length); row++) {
    const y = row * LANE;
    // The picked pattern's lane sits a shade above the rest, so a click in the panel shows
    // you where in the song that pattern lives.
    const picked = lanes[row]?.id === state.selected;
    for (let n = firstSnap; n * snapPx - left < width; n++) {
      ctx.fillStyle = picked
        ? n % 2 === 0
          ? "#2c2739"
          : "#272233"
        : n % 2 === 0
          ? "#221e2c"
          : "#1e1a27";
      ctx.fillRect(n * snapPx - left, y + 3, snapPx - 1, LANE - 7);
    }
    ctx.fillStyle = "rgba(0,0,0,0.35)";
    for (let n = firstBar; n * bar - left < width; n++) {
      ctx.fillRect(n * bar - left, y, 1, LANE - 1);
    }
  }

  // And the blocks on top, each as wide as it is long.
  for (const one of state.song) {
    const row = lanes.findIndex((pattern) => pattern.id === one.pattern);
    if (row < 0) continue;
    const pattern = lanes[row];
    const y = row * LANE;
    const x = one.step * step - left;
    const w = Math.max(3, Math.max(1, one.length) * step - 2);
    if (x + w < 0 || x > width) continue;
    const h = LANE - 7;
    const live =
      state.playing &&
      isSounding(pattern.id) &&
      state.step >= one.step &&
      state.step < one.step + Math.max(1, one.length);

    const base = blockColour(pattern, row);
    // A silenced pattern's blocks go faint rather than away: they still say where the part
    // would play, and pressing the speaker again brings them back.
    ctx.globalAlpha = pattern.muted ? 0.3 : 1;
    ctx.fillStyle = live ? lighten(base, 0.5) : base;
    roundRect(ctx, x + 1, y + 3, w, h, 4);
    ctx.fill();
    if (isBlockPicked(one)) {
      outlinePicked(ctx, () => roundRect(ctx, x + 1, y + 3, w, h, 4));
    }

    // The right hand edge is the handle for how long it is, so it is marked, the same way
    // a note in the piano roll is.
    if (w > 10) {
      ctx.fillStyle = "rgba(0,0,0,0.22)";
      ctx.fillRect(x + w - 2, y + 4, 3, h - 2);
    }
    if (w > 26) {
      ctx.save();
      roundRect(ctx, x + 1, y + 3, w, h, 4);
      ctx.clip();
      ctx.fillStyle = "#22101a";
      ctx.fillText(pattern.name, x + 6, y + LANE / 2 - 1);
      ctx.restore();
    }
    // Where the pattern comes round again inside a longer block, so a block that repeats
    // four times looks like four.
    const repeat = Math.max(1, pattern.steps) * step;
    if (repeat >= 6) {
      ctx.fillStyle = "rgba(0,0,0,0.28)";
      for (let at = repeat; at < w; at += repeat) {
        ctx.fillRect(x + at, y + 5, 1, h - 4);
      }
    }
    ctx.globalAlpha = 1;
  }

  ctx.fillStyle = state.playing ? PALETTE.lit : "rgba(139,131,153,0.6)";
  ctx.fillRect(songPlayheadX() - left, 0, 1, height);

  if (state.marquee && state.marquee.where === "song") {
    drawMarquee(ctx, state.marquee, left);
  }
}

// --- picking blocks out ---------------------------------------------------

/*
 * The same idea as picking notes out, one magnification up: shift drag a box round some
 * blocks and then move them, copy them, or rub them out together.
 *
 * Held as the entries of `state.song` themselves, the way notes are held. Rust hands the
 * whole song back after every edit, so the set is looked up again by where each block is
 * whenever that happens — see `songCommand`.
 */

function isBlockPicked(block) {
  return state.blocks.includes(block);
}

function pickBlocks(blocks, quietly) {
  state.blocks = blocks;
  state.needsDraw = true;
  if (!quietly) sayPickedBlocks();
}

function clearPickedBlocks() {
  if (!state.blocks.length) return;
  state.blocks = [];
  state.needsDraw = true;
}

function toggleBlockPicked(block) {
  const at = state.blocks.indexOf(block);
  if (at >= 0) state.blocks.splice(at, 1);
  else state.blocks.push(block);
  state.needsDraw = true;
  sayPickedBlocks();
}

function sayPickedBlocks() {
  if (!state.blocks.length) return;
  showNote(
    `${count(state.blocks.length, "block")} picked out — c to copy, v to paste, delete to rub out`,
  );
}

/* Which block is which, for finding the same ones again in a song Rust has handed back. */
const blockKey = (block) => ({ pattern: block.pattern, step: block.step });

function repickBlocks(keys) {
  if (!keys.length) return;
  state.blocks = keys
    .map((key) => state.song.find((one) => one.pattern === key.pattern && one.step === key.step))
    .filter(Boolean);
}

/* Every block a box dragged over the song touches. */
function blocksInBox(box) {
  const left = Math.min(box.from.x, box.to.x);
  const right = Math.max(box.from.x, box.to.x);
  const top = Math.min(box.from.y, box.to.y);
  const bottom = Math.max(box.from.y, box.to.y);
  const found = [];
  for (const one of state.song) {
    const row = state.patterns.findIndex((pattern) => pattern.id === one.pattern);
    if (row < 0) continue;
    if (row * LANE + LANE < top || row * LANE > bottom) continue;
    const x = one.step * songStep();
    const w = Math.max(1, one.length) * songStep();
    if (x + w < left || x > right) continue;
    found.push(one);
  }
  return found;
}

/* Every block in the song, which is what select all means when you are looking at it. */
function pickEveryBlock() {
  pickBlocks([...state.song]);
}

/* Rub the picked blocks out, in one step of the history. */
function removePickedBlocks() {
  if (!state.blocks.length) return;
  const many = state.blocks.length;
  const remove = state.blocks.map(blockKey);
  state.song = state.song.filter((one) => !state.blocks.includes(one));
  state.blocks = [];
  songChanged();
  songCommand("edit_placements", { remove, add: [] });
  showNote(`${count(many, "block")} rubbed out`);
}

/*
 * Copy the picked blocks. What is remembered is which pattern each one is and how far along
 * it sat from the first of them, so the set can be put down anywhere.
 */
function copyPickedBlocks(cut) {
  if (!state.blocks.length) return;
  const base = Math.min(...state.blocks.map((one) => one.step));
  const many = state.blocks.length;
  state.clipBlocks = {
    base,
    blocks: state.blocks.map((one) => ({
      pattern: one.pattern,
      along: one.step - base,
      length: Math.max(1, one.length),
    })),
  };
  if (cut) removePickedBlocks();
  showNote(`${count(many, "block")} copied — point along the song and press v`);
}

/* How much song the copied blocks take, from the first of them to the end of the last. */
function clipBlockSpan(clip) {
  return Math.max(1, ...clip.blocks.map((one) => one.along + one.length));
}

/*
 * Put the copy down, at the bar the pointer is over. With the pointer somewhere else it
 * lands after itself, so pressing v twice lays a phrase out twice.
 */
function pasteBlocks() {
  const clip = state.clipBlocks;
  if (!clip) return;
  const at =
    state.overStep !== null ? snapFloor(state.overStep) : clip.base + clipBlockSpan(clip);
  putBlocks(
    clip.blocks.map((one) => ({
      pattern: one.pattern,
      step: at + one.along,
      length: one.length,
    })),
  );
  showNote(`${count(clip.blocks.length, "block")} pasted`);
}

/* Duplicate the picked blocks, straight after themselves. */
function duplicatePickedBlocks() {
  if (!state.blocks.length) return;
  const base = Math.min(...state.blocks.map((one) => one.step));
  const along = Math.max(
    1,
    ...state.blocks.map((one) => one.step - base + Math.max(1, one.length)),
  );
  putBlocks(
    state.blocks.map((one) => ({
      pattern: one.pattern,
      step: one.step + along,
      length: Math.max(1, one.length),
    })),
  );
  showNote(`${count(state.blocks.length, "block")} duplicated`);
}

/*
 * Put a set of blocks in the song, and pick out what has just landed. Anything of the same
 * pattern they land on makes way for them, the same as dropping one block on another.
 */
function putBlocks(blocks) {
  const wanted = blocks.filter((one) => patternById(one.pattern) !== null && one.step >= 0);
  if (!wanted.length) return;
  for (const one of wanted) {
    state.song = state.song.filter(
      (was) =>
        !(
          was.pattern === one.pattern &&
          was.step < one.step + Math.max(1, one.length) &&
          one.step < was.step + Math.max(1, was.length)
        ),
    );
    state.song.push({ ...one });
  }
  state.song.sort((a, b) => a.step - b.step || a.pattern - b.pattern);
  state.blocks = state.song.filter((one) =>
    wanted.some((made) => made.pattern === one.pattern && made.step === one.step),
  );
  songChanged();
  songCommand("edit_placements", { remove: [], add: wanted });
}

// --- putting blocks in the song -------------------------------------------

/*
 * Where a pointer is in the song: which pattern's lane, which step, and what it is over.
 * `zone` is what a press there would do — grab an edge, pick the block up, or draw a new one.
 */
function songAt(event) {
  const rect = el.lanes.getBoundingClientRect();
  const row = Math.floor((event.clientY - rect.top) / LANE);
  if (row < 0 || row >= state.patterns.length) return null;
  const pattern = state.patterns[row];
  const x = event.clientX - rect.left + songLeft();
  if (x < 0) return null;
  const step = Math.floor(x / songStep());
  const block = placementAt(pattern.id, step);
  let zone = "empty";
  if (block) {
    const from = block.step * songStep();
    const to = (block.step + Math.max(1, block.length)) * songStep();
    // On a very short block the edges would leave nothing to pick it up by, so the left
    // hand two thirds moves it and only the far end resizes.
    const grab = Math.min(EDGE, (to - from) / 3);
    if (x >= to - grab) zone = "end";
    else if (x <= from + grab) zone = "start";
    else zone = "body";
  }
  return { row, pattern: pattern.id, step, x, block, zone };
}

/*
 * Where a pointer is in the song as a plain point: across in song pixels rather than window
 * ones, so a box keeps its corner where you started it however far the song is scrolled.
 */
function songPoint(event) {
  const rect = el.lanes.getBoundingClientRect();
  return {
    x: Math.max(0, event.clientX - rect.left + songLeft()),
    y: Math.max(0, Math.min(state.patterns.length * LANE, event.clientY - rect.top)),
  };
}

/*
 * Rust owns the song, so what it hands back is what gets drawn — which means every entry in
 * `state.song` is a new object afterwards. Anything picked out is found again by where it
 * is, so a set of blocks survives its own move.
 */
async function songCommand(command, args) {
  const keys = state.blocks.map(blockKey);
  try {
    state.song = (await invoke(command, args)).map((one) => ({ ...one }));
  } catch (e) {
    showError(e);
  }
  repickBlocks(keys);
  songChanged();
}

let songDrag = null;

el.lanes.addEventListener("pointerdown", (e) => {
  const at = songAt(e);
  if (!at) return;

  // The right button rubs out, all the way along a drag, the same as it does in a pattern.
  // A block that is one of a set picked out takes the set with it.
  if (erasing(e)) {
    el.lanes.setPointerCapture(e.pointerId);
    if (at.block && isBlockPicked(at.block)) {
      removePickedBlocks();
      return;
    }
    songDrag = { mode: "erase" };
    if (at.block) rubOut(at.block);
    return;
  }

  el.lanes.setPointerCapture(e.pointerId);

  /*
   * Shift drags a box round blocks rather than drawing one, the same as it does round notes.
   * A press on an empty lane already means "put this pattern here and keep painting", which
   * is how a song gets written and not something to give up for a selection box.
   */
  if (e.shiftKey) {
    if (at.block) {
      toggleBlockPicked(at.block);
      return;
    }
    const corner = songPoint(e);
    songDrag = { mode: "box" };
    state.marquee = { where: "song", from: corner, to: corner };
    state.needsDraw = true;
    return;
  }

  if (at.block && at.zone === "end") {
    songDrag = { mode: "end", block: at.block, was: { ...at.block } };
    return;
  }
  if (at.block && at.zone === "start") {
    songDrag = { mode: "start", block: at.block, was: { ...at.block } };
    return;
  }
  if (at.block) {
    // A block out of a picked set moves the whole set; one that is not clears the set, so a
    // plain press is always about the block you pressed on.
    if (!isBlockPicked(at.block)) clearPickedBlocks();
    // Alt leaves the originals where they are and drags copies away.
    const copying = e.altKey;
    const moving = state.blocks.length ? [...state.blocks] : [at.block];
    const held = copying ? copyBlocksInPlace(moving) : moving;
    const grabbed = copying ? (held[moving.indexOf(at.block)] ?? held[0]) : at.block;
    if (copying) pickBlocks(held, true);
    songDrag = {
      mode: "move",
      block: grabbed,
      was: { ...grabbed },
      copying,
      moving: held.map((block) => ({ block, was: { ...block } })),
      grab: at.step - grabbed.step,
    };
    return;
  }

  // Nothing there: draw one, as long as its pattern, and keep painting along the drag. The
  // drag stays in the lane it started in, so a diagonal sweep does not scribble in every
  // pattern it passes.
  clearPickedBlocks();
  songDrag = { mode: "paint", pattern: at.pattern };
  put(at.pattern, snapFloor(at.step));
});

el.lanes.addEventListener("pointermove", (e) => {
  if (!songDrag) {
    const at = songAt(e);
    // Where along the song the pointer is, which is where a paste goes.
    state.overStep = at ? at.step : null;
    showSongCursor(at, e.shiftKey);
    return;
  }
  // Dragging a block's end into the edge of the window scrolls the song along under it, so
  // stretching a block out never runs out of room to stretch it in.
  followEdge(el.songScroll, e, "x", songDragTo);
  songDragTo(e);
});

/* Copies of some blocks, put in the song on top of the originals, ready to be dragged off. */
function copyBlocksInPlace(blocks) {
  const made = blocks.map((one) => ({ ...one }));
  state.song.push(...made);
  return made;
}

/*
 * Where the drag has got to. Taken apart from the event so that scrolling the song under a
 * still hand can put the drag through again: the pointer has not moved, but the step it is
 * over has.
 */
function songDragTo(point) {
  if (!songDrag) return;

  if (songDrag.mode === "box") {
    state.marquee.to = songPoint(point);
    pickBlocks(blocksInBox(state.marquee), true);
    return;
  }

  const at = songAt(point);
  if (!at) return;
  state.overStep = at.step;

  if (songDrag.mode === "erase") {
    if (at.block) rubOut(at.block);
    return;
  }
  if (songDrag.mode === "paint") {
    // Only where there is room: with a snap shorter than the pattern, every step of the
    // drag would otherwise land on top of the block the last one made.
    const pattern = songDrag.pattern;
    const step = snapFloor(at.step);
    if (roomFor(pattern, step, stepsOf(pattern))) put(pattern, step);
    return;
  }

  const block = songDrag.block;
  if (songDrag.mode === "end") {
    // The end lands on the snap, and a block is never shorter than one step.
    const end = Math.max(block.step + 1, snapNear(at.step + 1));
    const length = end - block.step;
    if (length !== block.length) {
      block.length = length;
      songChanged();
    }
    return;
  }
  if (songDrag.mode === "start") {
    // Dragging the left hand edge moves where it starts and leaves where it ends alone.
    const end = songDrag.was.step + Math.max(1, songDrag.was.length);
    const start = Math.min(end - 1, snapNear(at.step));
    if (start !== block.step) {
      block.step = start;
      block.length = end - start;
      songChanged();
    }
    return;
  }
  // Moving, one block or a whole set of them: the one you grabbed follows the pointer and
  // the rest keep their places around it, so the shape of what you picked out is kept.
  const held = songDrag.moving ?? [{ block, was: songDrag.was }];
  const start = Math.max(0, snapNear(at.step - songDrag.grab));
  let by = start - songDrag.was.step;
  // Nothing in the set may be pushed off the front, so the whole set stops where the first
  // of them would have.
  for (const one of held) by = Math.max(by, -one.was.step);
  if (by === block.step - songDrag.was.step) return;
  for (const one of held) one.block.step = one.was.step + by;
  songChanged();
}

/* True when a block of this pattern would sit here without landing on another of its own. */
function roomFor(pattern, step, length) {
  const end = step + Math.max(1, length);
  return !state.song.some(
    (one) =>
      one.pattern === pattern && one.step < end && step < one.step + Math.max(1, one.length),
  );
}

/* Draw a new block, as long as its pattern, and tell Rust. Anything of the same pattern it
 * lands on makes way for it, which is what dropping a thing on a thing does everywhere. */
function put(pattern, step) {
  const steps = stepsOf(pattern);
  state.song = state.song.filter(
    (one) =>
      !(
        one.pattern === pattern &&
        one.step < step + steps &&
        step < one.step + Math.max(1, one.length)
      ),
  );
  state.song.push({ pattern, step, length: steps });
  state.song.sort((a, b) => a.step - b.step || a.pattern - b.pattern);
  songChanged();
  songCommand("place_pattern", { pattern, step, length: steps, on: true });
}

function rubOut(block) {
  state.song = state.song.filter((one) => one !== block);
  songChanged();
  songCommand("place_pattern", {
    pattern: block.pattern,
    step: block.step,
    length: 0,
    on: false,
  });
}

/* A drag is over, so tell Rust where things ended up. */
const dropBlock = () => {
  const drag = songDrag;
  songDrag = null;
  stopFollowing();

  if (drag && drag.mode === "box") {
    state.marquee = null;
    state.needsDraw = true;
    sayPickedBlocks();
    return;
  }
  if (!drag || !drag.block) return;
  const { block, was } = drag;
  const held = drag.moving ?? [{ block, was }];
  if (block.step === was.step && block.length === was.length) {
    // Copies dropped where they were made are the blocks that are already there.
    if (drag.copying) {
      state.song = state.song.filter((one) => !held.some((copy) => copy.block === one));
      state.blocks = [];
      songChanged();
    }
    return;
  }
  if (drag.copying || held.length > 1) {
    // The old places out and the new ones in, in one go, so a block sliding onto where
    // another has just left is not taken out by the one that left. A copy takes nothing out.
    songCommand("edit_placements", {
      remove: drag.copying ? [] : held.map((one) => blockKey(one.was)),
      add: held.map((one) => ({
        pattern: one.block.pattern,
        step: one.block.step,
        length: Math.max(1, one.block.length),
      })),
    });
    return;
  }
  if (block.step === was.step) {
    songCommand("resize_placement", {
      pattern: block.pattern,
      step: block.step,
      length: block.length,
    });
    return;
  }
  // The start moved, which is a move and possibly a resize: send it as one so the block is
  // never briefly nowhere, then the new length after it.
  const step = block.step;
  const length = block.length;
  songCommand("move_placement", { pattern: block.pattern, from: was.step, to: step }).then(
    () => {
      const now = placementAt(block.pattern, step);
      if (now && now.length !== length) {
        songCommand("resize_placement", { pattern: block.pattern, step, length });
      }
    },
  );
};
el.lanes.addEventListener("pointerup", dropBlock);
el.lanes.addEventListener("pointercancel", dropBlock);

/*
 * Double click a block to edit its pattern. This is the way into the editor: a click in the
 * patterns panel picks a pattern out and nothing more, so nothing you do in that list can
 * take you off the song by accident.
 *
 * A dblclick rather than counting presses, because a pointerdown's own click count is
 * always nought — that is what the spec says — and the presses underneath have already
 * been dropped as a drag that went nowhere by the time this arrives.
 */
el.lanes.addEventListener("dblclick", (e) => {
  const at = songAt(e);
  if (at && at.block) openPattern(at.pattern);
});

/* The pointer says what a press would do before you press it. */
function showSongCursor(at, boxing) {
  const cursor =
    at === null
      ? "default"
      : boxing
        ? "crosshair"
        : at.zone === "end" || at.zone === "start"
          ? "ew-resize"
          : at.zone === "body"
            ? "grab"
            : "cell";
  if (el.lanes.style.cursor !== cursor) el.lanes.style.cursor = cursor;
}

el.lanes.addEventListener("pointerleave", () => {
  el.lanes.style.cursor = "default";
  if (!songDrag) state.overStep = null;
});

// --- snap and zoom --------------------------------------------------------

numberField(el.snap, {
  min: 1,
  max: MAX_SNAP,
  onChange: (snap) => {
    state.snap = snap;
    state.needsDraw = true;
  },
});

/*
 * Zoom about a point, so whatever is under the cursor stays under it. Without that,
 * zooming in on bar sixty puts you back at bar one.
 *
 * Nothing here resizes a canvas: they are the window's width at every zoom, so all that
 * changes is how wide the song says it is and where the scroll sits.
 */
function setZoom(zoom, anchorX) {
  const was = state.zoom;
  const next = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, zoom));
  if (next === was) return;
  const at = anchorX ?? el.songScroll.clientWidth / 2;
  const step = (el.songScroll.scrollLeft + at) / (SONG_STEP * was);
  state.zoom = next;
  relayoutSong();
  el.songScroll.scrollLeft = Math.max(0, step * SONG_STEP * next - at);
}

el.zoomIn.addEventListener("click", () => setZoom(state.zoom * ZOOM_STEP));
el.zoomOut.addEventListener("click", () => setZoom(state.zoom / ZOOM_STEP));
el.zoomRead.addEventListener("click", () => setZoom(1));

/*
 * Zoom the roll about a point, both ways at once, the way pinching a map works. The keys
 * column and the velocity lane are fixed furniture, so the point is measured from the
 * corner where the notes start.
 */
function setRollZoom(zoom, anchorX, anchorY) {
  const was = state.rollZoom;
  const next = Math.max(MIN_ROLL_ZOOM, Math.min(MAX_ROLL_ZOOM, zoom));
  if (next === was) return;
  const ax = anchorX ?? el.rollScroll.clientWidth / 2;
  const ay = anchorY ?? el.rollScroll.clientHeight / 2;
  // Where in the notes the point is, in unzoomed pixels.
  const x = (el.rollScroll.scrollLeft + ax - KEYS) / was;
  const y = (el.rollScroll.scrollTop + ay - HEAD) / was;
  state.rollZoom = next;
  resizeRoll();
  el.rollScroll.scrollLeft = Math.max(0, KEYS + x * next - ax);
  el.rollScroll.scrollTop = Math.max(0, HEAD + y * next - ay);
}

el.rollZoomIn.addEventListener("click", () => setRollZoom(state.rollZoom * ZOOM_STEP));
el.rollZoomOut.addEventListener("click", () => setRollZoom(state.rollZoom / ZOOM_STEP));
el.rollZoomRead.addEventListener("click", () => setRollZoom(1));

/*
 * A trackpad pinch arrives as a wheel event with ctrlKey set — that is what the webview
 * turns the gesture into — so the same handler does pinch and ctrl-scroll. A plain wheel is
 * left alone: that is scrolling.
 *
 * A pinch fires far faster than the screen refreshes, and each one used to zoom, relayout
 * and set the scroll on the spot: several layouts a frame, which is what made it judder.
 * They are added up here and applied once, in the frame that draws.
 */
function pinching(where, event, box) {
  event.preventDefault();
  const x = event.clientX - box.left;
  const y = event.clientY - box.top;
  if (state.pinch && state.pinch.where === where) {
    state.pinch.delta += event.deltaY;
    state.pinch.x = x;
    state.pinch.y = y;
  } else {
    state.pinch = { where, delta: event.deltaY, x, y };
  }
}

/* One zoom per frame, however many wheel events the trackpad sent. */
function applyPinch() {
  const pinch = state.pinch;
  if (!pinch) return;
  state.pinch = null;
  const by = Math.exp(-pinch.delta / 180);
  if (pinch.where === "song") setZoom(state.zoom * by, pinch.x);
  else setRollZoom(state.rollZoom * by, pinch.x, pinch.y);
}

el.songScroll.addEventListener(
  "wheel",
  (e) => {
    if (!e.ctrlKey && !e.metaKey) return;
    pinching("song", e, el.songScroll.getBoundingClientRect());
  },
  { passive: false },
);

el.rollScroll.addEventListener(
  "wheel",
  (e) => {
    if (!e.ctrlKey && !e.metaKey) return;
    pinching("roll", e, el.rollScroll.getBoundingClientRect());
  },
  { passive: false },
);

// --- the scrubber ---------------------------------------------------------

/*
 * Drag along the top to move the playhead, playing or not. Snap decides where it lands, so
 * at the default it moves a bar at a time and at one it goes anywhere.
 */
function scrub(event, force) {
  const rect = el.scrubber.getBoundingClientRect();
  const under = Math.floor((event.clientX - rect.left + songLeft()) / songStep());
  if (under < 0) return;
  // Nowhere to be but the top of an empty song, and never past the end of a real one.
  const last = Math.max(0, songSteps() - 1);
  const wanted = Math.min(snapNear(under), last);
  if (wanted === state.step && !force) return;
  state.step = wanted;
  state.progress = 0;
  state.needsDraw = true;
  invoke("seek_song", { step: wanted });
}

let scrubbing = false;

el.scrubber.addEventListener("pointerdown", async (e) => {
  // The right button empties the bar: everything that starts in it, gone. Patterns are all
  // different lengths, so shuffling the rest of the song up would only break their grids.
  if (erasing(e)) {
    const rect = el.scrubber.getBoundingClientRect();
    const bar = Math.floor((e.clientX - rect.left + songLeft()) / barPx());
    if (bar < 0) return;
    await songCommand("clear_song_bar", { bar });
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

/* The keys the Edit menu's items answer to, and the two that only exist here. */
const EDIT_KEYS = { a: "select_all", c: "copy", x: "cut", v: "paste", d: "duplicate" };
const NUDGE_KEYS = {
  ArrowLeft: [-1, 0],
  ArrowRight: [1, 0],
  ArrowUp: [0, 1],
  ArrowDown: [0, -1],
};
const ZOOM_KEYS = { "=": 1, "+": 1, "-": -1, _: -1, 0: 0 };

window.addEventListener("keydown", (e) => {
  // Only a text field has any use for a space. A focused number does not, and having it
  // swallow the play key after you nudge the tempo is maddening.
  const typing = e.target.matches("input:not(.number), textarea, [contenteditable]");

  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "z" && !typing) {
    e.preventDefault();
    stepHistory(e.shiftKey);
    return;
  }

  /*
   * The Edit menu's keys. On a Mac these are handled by the menu bar before the window sees
   * them and arrive as an event instead, so most of the time this does not run — but it is
   * what makes them work everywhere else, and delete has no menu item of its own.
   *
   * The bare letters do the same, which is what the roll had before the menu could: they
   * only count when nothing is being typed in, and having them means a hand already on the
   * canvas does not have to reach for a modifier.
   */
  const combo = (e.metaKey || e.ctrlKey) && !e.altKey;
  const pressed = EDIT_KEYS[e.key.toLowerCase()];
  if (pressed && (combo || (!typedIn() && !e.altKey))) {
    e.preventDefault();
    editCommand(pressed);
    return;
  }
  if ((e.key === "Backspace" || e.key === "Delete") && !typedIn()) {
    e.preventDefault();
    editCommand("delete");
    return;
  }
  // Arrows nudge whatever is picked out, and do nothing at all when nothing is.
  const push = NUDGE_KEYS[e.key];
  if (push && !typedIn() && nudge(push[0], push[1], e.shiftKey)) {
    e.preventDefault();
    return;
  }
  const zoom = ZOOM_KEYS[e.key];
  if (zoom !== undefined && !typedIn() && zoomBy(zoom)) {
    e.preventDefault();
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
    if (!el.picker.classList.contains("hidden")) {
      closePicker();
    } else if (state.picked.length || state.blocks.length) {
      // Whatever is picked out is a thing to be out of, and the nearest one, so it goes
      // first: escape again closes the view.
      forgetPicked();
    } else if (state.sound !== null) {
      closeSound();
    } else if (state.roll !== null) {
      closeRoll();
    } else if (state.open !== null) {
      closePattern();
    } else {
      invoke("panic_stop");
      setPlaying(false);
    }
  }
});

// --- cut, copy, paste, and the rest of the Edit menu ------------------------

/*
 * One place that says what cut, copy, paste, duplicate, select all and delete mean.
 *
 * They arrive two ways and mean the same thing either way. From the Edit menu, because on a
 * Mac the menu bar gets a key equivalent before the window ever sees the key — which is the
 * whole reason those items are ours rather than the standard ones — and from the keyboard
 * here, for the platforms where the keys do reach the window and for delete, which no menu
 * item has.
 *
 * What each one acts on is whatever you are looking at: the text you are typing in, the
 * blocks in the song, or the notes in a pattern.
 */
listen("edit", (event) => editCommand(event.payload));

/*
 * The same press can arrive twice — once as a menu event and once as a key the menu did not
 * take — and which of those happens is the platform's business, not ours. A second one this
 * close behind the first is that press, not another: nobody pastes twice in a sixteenth of a
 * second, and a double paste would be a real edit to take back.
 */
const SAME_PRESS = 60; // milliseconds
let lastEdit = { what: null, at: -SAME_PRESS };

function editCommand(what) {
  const now = performance.now();
  if (what === lastEdit.what && now - lastEdit.at < SAME_PRESS) return;
  lastEdit = { what, at: now };
  const field = typedIn();
  if (field) {
    editText(field, what);
    return;
  }
  // The sound editor is one track's instrument rather than anybody's notes, so there is
  // nothing in it to cut or paste.
  if (state.sound !== null) return;
  if (state.open === null) editBlocks(what);
  else editNotes(what);
}

function editNotes(what) {
  if (what === "copy") copyPicked(false);
  else if (what === "cut") copyPicked(true);
  else if (what === "paste") pasteNotes();
  else if (what === "duplicate") duplicatePicked();
  else if (what === "select_all") pickEverything();
  else if (what === "delete") removePicked();
}

function editBlocks(what) {
  if (what === "copy") copyPickedBlocks(false);
  else if (what === "cut") copyPickedBlocks(true);
  else if (what === "paste") pasteBlocks();
  else if (what === "duplicate") duplicatePickedBlocks();
  else if (what === "select_all") pickEveryBlock();
  else if (what === "delete") removePickedBlocks();
}

/*
 * The field being typed in, or null for none. Which is what decides where an edit goes.
 *
 * Only fields with text in them. A slider is an input too, and a track's fader keeps the
 * focus after you drag it — copy would stop working for the rest of the session if that
 * counted as typing.
 */
const TEXT_FIELDS = ["text", "search", "url", "tel", "email", "password", "number"];

function typedIn() {
  const at = document.activeElement;
  if (!(at instanceof HTMLElement)) return null;
  if (at.isContentEditable || at.matches("textarea")) return at;
  return at instanceof HTMLInputElement && TEXT_FIELDS.includes(at.type) ? at : null;
}

/*
 * And the same six things in a text field.
 *
 * Ours to do, because the standard menu items that would have done them are not there any
 * more. Selecting and deleting are the field's own business and always work; the clipboard
 * is the webview's and may say no, which is worth saying out loud rather than looking
 * like a key that did nothing.
 */
async function editText(field, what) {
  if (what === "select_all") {
    field.select?.();
    return;
  }
  if (what === "duplicate") return;
  const value = String(field.value ?? "");
  const from = field.selectionStart ?? 0;
  const to = field.selectionEnd ?? 0;
  if (what === "delete") {
    replaceInField(field, from, to === from ? Math.min(value.length, from + 1) : to, "");
    return;
  }
  if (what === "copy" || what === "cut") {
    const taken = value.slice(from, to);
    if (!taken) return;
    if (!(await putOnClipboard(taken))) {
      showWarning("could not reach the clipboard");
      return;
    }
    if (what === "cut") replaceInField(field, from, to, "");
    return;
  }
  if (what === "paste") {
    try {
      const text = await navigator.clipboard.readText();
      if (text) replaceInField(field, from, to, text);
    } catch {
      showWarning("could not reach the clipboard");
    }
  }
}

async function putOnClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // The older way, which works off whatever is selected rather than off a string.
    return document.execCommand("copy");
  }
}

/* Put text in a field where the caret is, as typing it would. */
function replaceInField(field, from, to, text) {
  const value = String(field.value ?? "");
  field.value = value.slice(0, from) + text + value.slice(to);
  const caret = from + text.length;
  field.setSelectionRange?.(caret, caret);
  // The filters and the number fields listen for this, so a paste has to look like typing.
  field.dispatchEvent(new Event("input", { bubbles: true }));
}

/*
 * Arrow keys nudge whatever is picked out: a step or a semitone at a time, a bar or an
 * octave with shift held. Nothing picked out and the arrows do nothing, so they are never
 * in the way.
 */
function nudge(along, up, big) {
  if (state.open === null) {
    return nudgeBlocks(along * (big ? STEPS_PER_BAR : Math.max(1, state.snap)));
  }
  if (state.sound !== null) return false;
  // The step grid has no pitch to move a note up: that is what the roll is for.
  const semitones = state.roll === null ? 0 : up * (big ? 12 : 1);
  return nudgeNotes(along * (big ? STEPS_PER_BAR : 1), semitones);
}

function nudgeNotes(bySteps, byPitch) {
  const open = openPatternNow();
  if (!open || !state.picked.length) return false;
  // Nothing may be pushed off an end, so the whole set stops where the first of them would.
  let along = bySteps;
  let up = byPitch;
  for (const { note } of state.picked) {
    along = Math.max(along, -note.step);
    along = Math.min(along, MAX_STEPS - Math.max(1, note.length) - note.step);
    up = Math.max(up, LOW_PITCH - note.pitch);
    up = Math.min(up, HIGH_PITCH - note.pitch);
  }
  if (!along && !up) return true;
  for (const [track, notes] of pickedByTrack()) {
    const was = notes.map(placeOf);
    for (const note of notes) {
      note.step += along;
      note.pitch += up;
    }
    tidyLane(track, notes);
    sendNoteEdit(track, was, notes);
  }
  state.needsDraw = true;
  return true;
}

function nudgeBlocks(bySteps) {
  if (!state.blocks.length) return false;
  let along = bySteps;
  for (const one of state.blocks) along = Math.max(along, -one.step);
  if (!along) return true;
  const remove = state.blocks.map(blockKey);
  for (const one of state.blocks) one.step += along;
  songChanged();
  songCommand("edit_placements", {
    remove,
    add: state.blocks.map((one) => ({
      pattern: one.pattern,
      step: one.step,
      length: Math.max(1, one.length),
    })),
  });
  return true;
}

/* And the zoom keys, for whichever of the two views has a zoom. */
function zoomBy(how) {
  if (state.open === null) {
    if (how === 0) setZoom(1);
    else setZoom(how > 0 ? state.zoom * ZOOM_STEP : state.zoom / ZOOM_STEP);
    return true;
  }
  if (state.roll === null || state.sound !== null) return false;
  if (how === 0) setRollZoom(1);
  else setRollZoom(how > 0 ? state.rollZoom * ZOOM_STEP : state.rollZoom / ZOOM_STEP);
  return true;
}

// --- undo and redo --------------------------------------------------------

/*
 * Rust keeps the history, because Rust owns the project: a step back is a whole project
 * handed over, and drawing one of those is something the front end already knows how to do.
 *
 * On macOS the menu bar gets cmd-Z before the window does, so most of the time this runs
 * from a menu event rather than from here. The key is handled anyway, for everywhere else.
 */
async function stepHistory(forward) {
  try {
    const now = await invoke(forward ? "redo" : "undo");
    if (now) stepped(now);
    else showWarning(forward ? "nothing to redo" : "nothing to undo");
  } catch (e) {
    showError(e);
  }
}

/*
 * A step back or forward. Rust hands the whole project to the audio thread again, and that
 * includes which pattern is the live one — it has no idea which one you are looking at — so
 * the view says so again afterwards. Without this, undoing while editing a pattern would
 * leave the window in the editor and the engine playing the song.
 */
function stepped(now) {
  applyProject(now);
  if (state.open !== null) invoke("open_pattern", { id: state.open });
  else invoke("close_pattern");
  // A sample a step back could not find, most likely. Never nothing.
  if (now.message) showError(now.message);
}

// --- the playhead ---------------------------------------------------------

/*
 * Polled, not pushed. Tauri's messaging is not real time, so an event per step would
 * arrive in clumps and the playhead would judder. The audio thread writes its position
 * into an atomic and this reads it whenever the browser is about to paint.
 */
let lastPoll = 0;
let deafPolls = 0;

/*
 * How far up the meter a peak goes, from nothing to one. Decibels, not the raw number: a
 * linear meter spends nearly all of its travel in the top six decibels, so a mix sitting at
 * a sensible level barely moves it while a raw one-shot played on its own slams it. That is
 * what made the meter look like it only worked on the audition button.
 */
const METER_FLOOR_DB = -48;

function meterLevel(peak) {
  if (!(peak > 0)) return 0;
  const db = 20 * Math.log10(peak);
  return Math.max(0, Math.min(1, (db - METER_FLOOR_DB) / -METER_FLOOR_DB));
}

async function tick(now) {
  requestAnimationFrame(tick);

  // A pinch that came in since the last frame, applied once, here, where it is about to be
  // drawn anyway. And a drag that has reached the edge of the window, which moves the view
  // along under it.
  applyPinch();
  runEdgeScroll();

  // No point asking sixty times a second when nothing is moving.
  const interval = state.playing ? 0 : 200;
  if (now - lastPoll >= interval) {
    lastPoll = now;
    try {
      const p = await invoke("playhead");
      deafPolls = 0;
      // The meter first: it is the one thing here that has to be right every frame, and
      // anything below it that threw used to take the meter down with it, silently.
      el.meterMask.style.transform = `scaleX(${1 - meterLevel(p.peak)})`;
      const wasSounding = state.sounding;
      state.step = p.step;
      state.progress = p.progress;
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
      if (p.saveError) showError(`could not save: ${p.saveError}`);
      else if (p.streamErrors > 0) showWarning(`${p.streamErrors} audio dropouts`);
    } catch (e) {
      // One failed poll is not worth a dialog; the next one will do. A hundred of them in
      // a row is, because it means the playhead has stopped and nobody said so.
      deafPolls += 1;
      if (deafPolls === 100) showError(`lost touch with the audio thread: ${e}`);
    }
  }

  if (state.needsDraw) {
    state.needsDraw = false;
    if (state.open === null) drawSong();
    else if (state.roll !== null) drawRoll();
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

/* Something that has just happened and is worth a moment. Not trouble, so it is not red. */
function showNote(text) {
  note(escapeText(text), 2500);
}

/* Take a notice down before its time is up, for one that has stopped being true. */
function clearStatus() {
  el.status.innerHTML = "";
  noticeUntil = 0;
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
