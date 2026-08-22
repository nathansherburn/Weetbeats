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
 * ♪ button switches between them without anything being lost.
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
const STEPS_PER_BEAT = 4; // sixteenth notes
const STEPS_PER_BAR = 16; // a bar of the song, and the length of a new pattern
const MAX_STEPS = 256; // as far as the engine will play

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

// Pixels of drag per step of a number field.
const DRAG_PIXELS = 3;

const state = {
  bpm: 120,
  tracks: [], // { id, name, gain, muted, soloed, peaks }
  patterns: [], // { id, name, steps, notes: Map<trackId, Set<step>> }
  song: [], // { pattern, step, length }, sorted: what plays where
  open: null, // the pattern the editor has, or null for the song view
  roll: null, // the track the piano roll has, or null for the boxes
  drawLength: 1, // how long the last note drawn was, so the next one matches
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
  } else if (state.roll !== null && !trackById(state.roll)?.pitched) {
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

// --- the two views --------------------------------------------------------

/* One of the three at a time: the song, a pattern's boxes, or a pattern's piano roll. */
function showView() {
  const inPattern = state.open !== null;
  const inRoll = inPattern && state.roll !== null;
  el.song.classList.toggle("hidden", inPattern);
  el.editor.classList.toggle("hidden", !inPattern || inRoll);
  el.roll.classList.toggle("hidden", !inRoll);
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
  state.open = id;
  state.selected = id;
  state.roll = null;
  el.steps.value = String(stepsOf(id));
  showPatternColour();
  showView();
  invoke("open_pattern", { id });
  drawPatternPanel();
  resize();
}

function closePattern() {
  // The pattern you were in stays picked out, so the song shows you where it plays.
  if (state.open !== null) state.selected = state.open;
  state.open = null;
  state.roll = null;
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
    row.classList.toggle("open", state.open === id);
    row.classList.toggle("picked", state.open !== id && state.selected === id);
    row.classList.toggle("playing", state.playing && isSounding(id));
  }
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

      // Turns the row into a piano roll and back. Nothing is thrown away either way: the
      // boxes and the roll are two views of one lane of notes.
      const roll = document.createElement("button");
      roll.className = `tick keys-on${track.pitched ? " on" : ""}`;
      roll.textContent = "♪";
      roll.title = track.pitched
        ? "Back to the boxes, and back to a one-shot"
        : "Piano roll: play this one pitched, and show its notes";
      roll.addEventListener("click", () => setPitched(track.id, !track.pitched));

      const kill = document.createElement("button");
      kill.className = "tick kill";
      kill.textContent = "×";
      kill.title = "Delete this track";
      kill.addEventListener("click", () => removeTrack(track.id));

      row.append(roll, mute, solo, gain, kill);
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
  if (state.roll === id) closeRoll();
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
  size(el.ruler, gridWidth(), HEAD - 1);
  state.needsDraw = true;
  drawRuler();
}

function resizeRoll() {
  const width = rollSteps() * rollCell();
  const height = PITCHES * semitone();
  size(el.notes, width, height);
  size(el.rollRuler, width, HEAD - 1);
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

function resize() {
  resizeEditor();
  resizeRoll();
  resizeSong();
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
  // The pattern's colour along the bottom of the strip, running the whole way across:
  // the name tab at one end, the close button at the other, and this joining them.
  ctx.fillStyle = colourOf(state.open);
  ctx.fillRect(0, HEAD - 4, width, 3);
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

  for (let row = 0; row < state.tracks.length; row++) {
    const track = state.tracks[row];
    const y = row * ROW;
    // An instrument's row is its notes rather than a line of boxes: boxes cannot say what
    // pitch or how long, which is the whole point of turning it into one.
    if (track.pitched) {
      drawMiniRoll(ctx, pattern, track, y);
      continue;
    }
    // Only the notes a box can mean: the sampler's own pitch. Anything drawn in the piano
    // roll lives in the same lane and is left to the roll to show.
    const ticked = new Set();
    for (const note of pattern.notes.get(track.id) ?? []) {
      if (note.pitch === DEFAULT_PITCH) ticked.add(note.step);
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
    }
  }
}

/*
 * One instrument's notes, in the room a row of boxes would have taken. Click it to open the
 * roll proper; the ♪ button in the header puts the boxes back, and neither throws anything
 * away — the boxes and the roll have always been the same lane of notes.
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

el.grid.addEventListener("pointerdown", (e) => {
  const cell = cellAt(e);
  if (!cell) return;
  // An instrument's row is a piano roll, and a piano roll opens rather than being ticked.
  if (trackById(cell.track)?.pitched) {
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
  if (painting === null) {
    const cell = cellAt(e);
    const cursor = !cell ? "default" : trackById(cell.track)?.pitched ? "pointer" : "cell";
    if (el.grid.style.cursor !== cursor) el.grid.style.cursor = cursor;
    return;
  }
  paint(cellAt(e), painting);
});

const stopPainting = () => {
  painting = null;
};
el.grid.addEventListener("pointerup", stopPainting);
el.grid.addEventListener("pointercancel", stopPainting);

// --- the piano roll -------------------------------------------------------

/*
 * The same pattern as the step grid, seen as notes: the keyboard down the left, the notes in
 * the middle, how hard each is hit underneath. Nothing here is a different kind of data —
 * a box is a note at the sampler's own pitch, one step long, and this is the editor that
 * lets you put one anywhere.
 */
function openRoll(track) {
  if (state.open === null || trackById(track) === null) return;
  state.roll = track;
  showView();
  // Notes only mean pitch and length on an instrument, so opening the roll makes it one.
  // The button in the corner is there to change your mind.
  const instrument = trackById(track);
  if (!instrument.pitched) setPitched(track, true);
  showPitched();
  resize();
  // Land on the sampler's own pitch, which is where the notes will be.
  el.rollScroll.scrollTop = Math.max(
    0,
    (HIGH_PITCH - DEFAULT_PITCH - 6) * semitone(),
  );
}

function closeRoll() {
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
 */
function setPitched(track, pitched) {
  const instrument = trackById(track);
  if (!instrument) return;
  instrument.pitched = pitched;
  invoke("set_track_pitched", { id: track, pitched });
  if (!pitched && state.roll === track) closeRoll();
  drawTrackHeaders();
  showPitched();
  state.needsDraw = true;
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
  ctx.fillStyle = colourOf(state.open);
  ctx.fillRect(0, HEAD - 4, width, 3);
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
  }
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

let dragging = null;

el.notes.addEventListener("pointerdown", (e) => {
  const at = rollAt(e);
  if (!at) return;
  const pattern = openPatternNow();
  const under = noteUnder(at);

  if (erasing(e)) {
    if (under) {
      remove(under.note);
    }
    return;
  }

  el.notes.setPointerCapture(e.pointerId);
  if (under && under.edge) {
    // Grabbed by the end: this is how long it is.
    dragging = { mode: "length", note: under.note, was: { ...under.note } };
    return;
  }
  if (under) {
    dragging = {
      mode: "move",
      note: under.note,
      was: { ...under.note },
      grab: { step: at.step - under.note.step, pitch: at.pitch - under.note.pitch },
    };
    invoke("audition", { id: state.roll, pitch: under.note.pitch });
    return;
  }

  // Nothing there, so draw one — and keep hold of its end, so dragging straight on sets
  // how long it is.
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
    const cursor = !under ? "cell" : under.edge ? "ew-resize" : "grab";
    if (el.notes.style.cursor !== cursor) el.notes.style.cursor = cursor;
    return;
  }
  const at = rollAt(e);
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

  const step = Math.max(0, Math.min(at.step - dragging.grab.step, MAX_STEPS - note.length));
  const pitch = Math.max(LOW_PITCH, Math.min(HIGH_PITCH, at.pitch - dragging.grab.pitch));
  if (step === note.step && pitch === note.pitch) return;
  if (pitch !== note.pitch) invoke("audition", { id: state.roll, pitch });
  note.step = step;
  note.pitch = pitch;
  state.needsDraw = true;
});

const dropNote = () => {
  if (!dragging) return;
  const { note, was, mode } = dragging;
  dragging = null;
  if (mode === "length") {
    state.drawLength = note.length;
    send(note);
    return;
  }
  if (note.step === was.step && note.pitch === was.pitch) return;
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
    // A note past the end made the pattern longer, so everything that is drawn from its
    // length has to be drawn again: the roll, the boxes, and the count in the panel.
    const grew = patternById(pattern);
    if (grew && grew.steps !== put.steps) {
      grew.steps = put.steps;
      if (state.open === pattern) el.steps.value = String(put.steps);
      drawPatternPanel();
      resize();
    }
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
  const snapPx = Math.max(3, Math.max(1, state.snap) * step);
  const firstSnap = Math.floor(left / snapPx);
  const firstBar = Math.floor(left / bar);
  for (let row = 0; row < Math.max(1, state.patterns.length); row++) {
    const y = row * LANE;
    // The picked pattern's lane sits a shade above the rest, so a click in the panel shows
    // you where in the song that pattern lives.
    const picked = state.patterns[row]?.id === state.selected;
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
    const row = state.patterns.findIndex((pattern) => pattern.id === one.pattern);
    if (row < 0) continue;
    const pattern = state.patterns[row];
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
    ctx.fillStyle = live ? lighten(base, 0.5) : base;
    roundRect(ctx, x + 1, y + 3, w, h, 4);
    ctx.fill();

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
  }

  ctx.fillStyle = state.playing ? PALETTE.lit : "rgba(139,131,153,0.6)";
  ctx.fillRect(songPlayheadX() - left, 0, 1, height);
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

/* Rust owns the song, so what it hands back is what gets drawn. */
async function songCommand(command, args) {
  try {
    state.song = (await invoke(command, args)).map((one) => ({ ...one }));
  } catch (e) {
    showError(e);
  }
  songChanged();
}

let songDrag = null;

el.lanes.addEventListener("pointerdown", (e) => {
  const at = songAt(e);
  if (!at) return;

  // The right button rubs out, all the way along a drag, the same as it does in a pattern.
  if (erasing(e)) {
    el.lanes.setPointerCapture(e.pointerId);
    songDrag = { mode: "erase" };
    if (at.block) rubOut(at.block);
    return;
  }

  el.lanes.setPointerCapture(e.pointerId);

  if (at.block && at.zone === "end") {
    songDrag = { mode: "end", block: at.block, was: { ...at.block } };
    return;
  }
  if (at.block && at.zone === "start") {
    songDrag = { mode: "start", block: at.block, was: { ...at.block } };
    return;
  }
  if (at.block) {
    songDrag = { mode: "move", block: at.block, was: { ...at.block }, grab: at.step - at.block.step };
    return;
  }

  // Nothing there: draw one, as long as its pattern, and keep painting along the drag. The
  // drag stays in the lane it started in, so a diagonal sweep does not scribble in every
  // pattern it passes.
  songDrag = { mode: "paint", pattern: at.pattern };
  put(at.pattern, snapFloor(at.step));
});

el.lanes.addEventListener("pointermove", (e) => {
  if (!songDrag) {
    showSongCursor(songAt(e));
    return;
  }
  const at = songAt(e);
  if (!at) return;

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
  // Moving: the block keeps its length and follows wherever you grabbed it.
  const start = Math.max(0, snapNear(at.step - songDrag.grab));
  if (start !== block.step) {
    block.step = start;
    songChanged();
  }
});

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
  if (!drag || !drag.block) return;
  const { block, was } = drag;
  if (block.step === was.step && block.length === was.length) return;
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
function showSongCursor(at) {
  const cursor =
    at === null
      ? "default"
      : at.zone === "end" || at.zone === "start"
        ? "ew-resize"
        : at.zone === "body"
          ? "grab"
          : "cell";
  if (el.lanes.style.cursor !== cursor) el.lanes.style.cursor = cursor;
}

el.lanes.addEventListener("pointerleave", () => {
  el.lanes.style.cursor = "default";
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

window.addEventListener("keydown", (e) => {
  // Only a text field has any use for a space. A focused number does not, and having it
  // swallow the play key after you nudge the tempo is maddening.
  const typing = e.target.matches("input:not(.number), textarea, [contenteditable]");

  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "z" && !typing) {
    e.preventDefault();
    stepHistory(e.shiftKey);
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
    if (state.roll !== null) {
      closeRoll();
    } else if (state.open !== null) {
      closePattern();
    } else {
      invoke("panic_stop");
      setPlaying(false);
    }
  }
});

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
  // drawn anyway.
  applyPinch();

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
