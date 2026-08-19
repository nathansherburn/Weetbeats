/*
 * Weetbeats front end.
 *
 * Draws the grid, handles clicks, and polls the playhead. It holds a copy of the pattern
 * so a click can light a box up straight away, but Rust owns the project: every change
 * goes there too, and the audio thread hears about it from Rust.
 *
 * The grid is one canvas rather than a box per step. Sixteen steps by a few tracks would
 * survive as elements, but the piano roll in stage 4 will not, and this is the same
 * drawing code either way.
 */

const { invoke } = window.__TAURI__.core;

const CELL = 42; // width of one step
const GAP = 4; // gap between steps
const ROW = 46; // must match --row in the stylesheet
const STEPS_PER_BAR = 4; // sixteenth notes, so four steps to a beat

const state = {
  steps: 16,
  bpm: 120,
  tracks: [], // { id, name, gain, muted, soloed, steps: Set<number>, peaks: [] }
  playing: false,
  step: 0,
  progress: 0,
  samples: [],
  filter: "",
  audio: null,
  needsDraw: true,
};

const el = {
  play: document.getElementById("play"),
  bpm: document.getElementById("bpm"),
  bpmValue: document.getElementById("bpmValue"),
  master: document.getElementById("master"),
  meter: document.getElementById("meterFill"),
  status: document.getElementById("status"),
  choose: document.getElementById("choose"),
  filter: document.getElementById("filter"),
  samples: document.getElementById("samples"),
  browserRoot: document.getElementById("browserRoot"),
  headers: document.getElementById("trackHeaders"),
  grid: document.getElementById("grid"),
  ruler: document.getElementById("ruler"),
  pattern: document.getElementById("pattern"),
  empty: document.getElementById("empty"),
};

const css = getComputedStyle(document.documentElement);
const colour = (name, fallback) =>
  (css.getPropertyValue(name) || fallback).trim() || fallback;

const PALETTE = {
  bg: colour("--bg", "#131118"),
  panel: colour("--panel-2", "#221e2c"),
  line: colour("--line", "#2e2839"),
  dim: colour("--dim", "#8b8399"),
  accent: colour("--accent", "#ff4d87"),
  lit: colour("--lit", "#ffd75e"),
};

// --- start up -------------------------------------------------------------

async function boot() {
  const startup = await invoke("startup");
  state.audio = startup.audio;
  state.bpm = startup.project.bpm;
  state.steps = startup.project.pattern.steps;
  setSamples(startup.samples);

  el.bpm.value = String(Math.round(state.bpm));
  el.bpmValue.textContent = String(Math.round(state.bpm));
  el.master.value = String(Math.round(startup.project.masterGain * 100));

  resize();
  requestAnimationFrame(tick);
}

function setSamples(listing) {
  state.samples = listing.entries;
  // Only the tail of the path is worth the space; the whole thing is in the tooltip.
  const tail = (listing.root || "").split("/").filter(Boolean).slice(-2).join("/");
  el.browserRoot.textContent = listing.truncated
    ? `${tail} — first ${listing.entries.length}`
    : tail;
  el.browserRoot.title = listing.root || "";
  drawSampleList();
}

// --- sample browser -------------------------------------------------------

function drawSampleList() {
  const needle = state.filter.toLowerCase();
  const shown = needle
    ? state.samples.filter(
        (s) =>
          s.name.toLowerCase().includes(needle) ||
          s.folder.toLowerCase().includes(needle),
      )
    : state.samples;

  el.samples.replaceChildren(
    ...shown.map((sample) => {
      const li = document.createElement("li");
      li.draggable = true;
      li.title = sample.path;

      const name = document.createElement("span");
      name.textContent = sample.name;
      li.append(name);

      if (sample.folder) {
        const folder = document.createElement("span");
        folder.className = "folder";
        folder.textContent = sample.folder;
        li.append(folder);
      }

      li.addEventListener("click", () => {
        invoke("preview", { path: sample.path }).catch(showError);
        li.classList.add("playing");
        setTimeout(() => li.classList.remove("playing"), 180);
      });
      li.addEventListener("dragstart", (e) => {
        e.dataTransfer.setData("text/weetbeats-sample", sample.path);
        e.dataTransfer.effectAllowed = "copy";
      });
      return li;
    }),
  );
}

el.filter.addEventListener("input", () => {
  state.filter = el.filter.value;
  drawSampleList();
});

el.choose.addEventListener("click", async () => {
  const listing = await invoke("choose_folder").catch(showError);
  if (listing) {
    state.filter = "";
    el.filter.value = "";
    setSamples(listing);
  }
});

// --- tracks ---------------------------------------------------------------

async function addTrack(path) {
  try {
    const added = await invoke("add_track", { path });
    state.tracks.push({
      id: added.track.id,
      name: added.track.name,
      gain: added.track.gain,
      muted: added.track.muted,
      soloed: added.track.soloed,
      steps: new Set(added.track.notes.map((n) => n.step)),
      peaks: added.peaks,
    });
    drawTrackHeaders();
    resize();
  } catch (e) {
    showError(e);
  }
}

function drawTrackHeaders() {
  el.empty.classList.toggle("hidden", state.tracks.length > 0);

  el.headers.replaceChildren(
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

function removeTrack(id) {
  invoke("remove_track", { id });
  state.tracks = state.tracks.filter((t) => t.id !== id);
  drawTrackHeaders();
  resize();
}

function drawWaveform(canvas, peaks) {
  const ctx = canvas.getContext("2d");
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = PALETTE.dim;
  const step = w / peaks.length;
  for (let i = 0; i < peaks.length; i++) {
    const bar = Math.max(1, peaks[i] * h);
    ctx.fillRect(i * step, (h - bar) / 2, Math.max(1, step - 0.5), bar);
  }
}

// --- dragging a sample into the pattern -----------------------------------

el.pattern.addEventListener("dragover", (e) => {
  if (e.dataTransfer.types.includes("text/weetbeats-sample")) {
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    el.pattern.classList.add("drop-target");
  }
});
el.pattern.addEventListener("dragleave", (e) => {
  if (!el.pattern.contains(e.relatedTarget)) {
    el.pattern.classList.remove("drop-target");
  }
});
el.pattern.addEventListener("drop", (e) => {
  const path = e.dataTransfer.getData("text/weetbeats-sample");
  el.pattern.classList.remove("drop-target");
  if (path) {
    e.preventDefault();
    addTrack(path);
  }
});

// --- the grid -------------------------------------------------------------

function gridWidth() {
  return state.steps * CELL;
}

function resize() {
  const dpr = window.devicePixelRatio || 1;
  const height = Math.max(ROW, state.tracks.length * ROW);

  for (const [canvas, w, h] of [
    [el.grid, gridWidth(), height],
    [el.ruler, gridWidth(), 29],
  ]) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    canvas.getContext("2d").setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  state.needsDraw = true;
  drawRuler();
}

function drawRuler() {
  const ctx = el.ruler.getContext("2d");
  ctx.clearRect(0, 0, gridWidth(), 29);
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";
  for (let step = 0; step < state.steps; step++) {
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
  const ctx = el.grid.getContext("2d");
  const width = gridWidth();
  const height = Math.max(ROW, state.tracks.length * ROW);
  ctx.clearRect(0, 0, width, height);

  // The column the playhead is in, drawn under the steps so lit boxes stay readable.
  if (state.playing) {
    ctx.fillStyle = "rgba(255,215,94,0.09)";
    ctx.fillRect(state.step * CELL, 0, CELL, height);
  }

  for (let row = 0; row < state.tracks.length; row++) {
    const track = state.tracks[row];
    const y = row * ROW;

    ctx.fillStyle = PALETTE.line;
    ctx.fillRect(0, y + ROW - 1, width, 1);

    for (let step = 0; step < state.steps; step++) {
      const x = step * CELL + GAP;
      const w = CELL - GAP * 2;
      const h = ROW - GAP * 2 - 1;
      const on = track.steps.has(step);
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
  const rect = el.grid.getBoundingClientRect();
  const step = Math.floor((event.clientX - rect.left) / CELL);
  const row = Math.floor((event.clientY - rect.top) / ROW);
  if (step < 0 || step >= state.steps) return null;
  if (row < 0 || row >= state.tracks.length) return null;
  return { track: state.tracks[row], step };
}

function paint(cell, on) {
  if (!cell) return;
  if (cell.track.steps.has(cell.step) === on) return;
  if (on) {
    cell.track.steps.add(cell.step);
  } else {
    cell.track.steps.delete(cell.step);
  }
  state.needsDraw = true;
  invoke("set_step", { id: cell.track.id, step: cell.step, on }).then((actual) => {
    // Rust has the final say: a track that is full will not take another note.
    if (actual !== on) {
      if (actual) cell.track.steps.add(cell.step);
      else cell.track.steps.delete(cell.step);
      state.needsDraw = true;
    }
  });
}

el.grid.addEventListener("pointerdown", (e) => {
  const cell = cellAt(e);
  if (!cell) return;
  // Drag across boxes to paint, like FL Studio: what the first box becomes is what the
  // rest become, so a drag never toggles boxes back and forth under your finger.
  painting = !cell.track.steps.has(cell.step);
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
  const typing = e.target.matches("input, textarea");
  if (e.code === "Space" && !typing) {
    e.preventDefault();
    setPlaying(!state.playing);
  }
  if (e.code === "Escape") {
    invoke("panic_stop");
    setPlaying(false);
  }
});

window.addEventListener("resize", resize);

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
      if (p.step !== state.step) {
        state.step = p.step;
        state.needsDraw = true;
      }
      if (p.playing !== state.playing) {
        state.playing = p.playing;
        el.play.classList.toggle("on", p.playing);
        state.needsDraw = true;
      }
      state.progress = p.progress;
      el.meter.style.width = `${Math.min(100, p.peak * 100)}%`;
      showStatus(p);
    } catch {
      // A failed poll is not worth a dialog; the next one will do.
    }
  }

  if (state.needsDraw) {
    state.needsDraw = false;
    drawGrid();
  }
}

function showStatus(p) {
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
  el.status.innerHTML = `<span class="warn">${String(e)}</span>`;
  setTimeout(() => {
    el.status.textContent = "";
  }, 4000);
}

boot();
