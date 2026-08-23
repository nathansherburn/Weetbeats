/*
 * Stands in for the Rust side so the front end can be driven in a browser.
 * Mirrors what each Tauri command actually does, including what it refuses and the casing
 * it answers in. Where the real thing has a rule — a block is as long as it was drawn, a
 * shortened pattern loses the notes off its end, a note past the end lengthens the pattern
 * — the rule is here too, so a test that passes here is testing the same behaviour.
 */
window.__weetbeats_calls = [];

const MAX_PATTERNS = 32;
const MAX_TRACKS = 32;
const MAX_STEPS = 256;
const MAX_SONG_BARS = 256;
const MAX_NOTES = 256;

const fake = {
  bpm: 120,
  masterGain: 0.9,
  playing: false,
  step: 0,
  active: 0,
  songMode: false,
  nextTrackId: 0,
  tracks: new Map(),
  patterns: [{ id: 0, name: "Pattern 1", steps: 16, mix: [], lanes: [] }],
  // { step, pattern, length }: what plays where, and for how long.
  song: [],
  name: "Untitled",
  folder: "/tmp/Untitled.beat",
  saves: 0,
  peaks: Array.from({ length: 96 }, (_, i) => Math.abs(Math.sin(i / 7)) * 0.9),
  // What each plugin track's controls are set to, which is the plugin's business and not
  // the project's.
  pluginParams: new Map(),
};

// What the file picker "returns". A test sets this before clicking Add.
fake.picks = ["/pack/01 kick.wav"];
// And what a CLAP scan "finds". Two instruments and an effect, which is the case the picker
// has to get right: effects are shown but cannot be added.
fake.plugins = [
  {
    path: "/Library/Audio/Plug-Ins/CLAP/Surge XT.clap",
    id: "org.surge-synth-team.surge-xt",
    name: "Surge XT",
    vendor: "Surge Synth Team",
    instrument: true,
  },
  {
    path: "/Library/Audio/Plug-Ins/CLAP/Vital.clap",
    id: "audio.vital.synth",
    name: "Vital",
    vendor: "Vital Audio",
    instrument: true,
  },
  {
    path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap",
    id: "org.surge-synth-team.surge-xt-fx",
    name: "Surge XT Effects",
    vendor: "Surge Synth Team",
    instrument: false,
  },
];
// How many times a proper look through the plugin folders has been asked for.
fake.scans = 0;
/* What a plugin says its controls are. Mirrors what the params extension hands back. */
fake.params = [
  { id: 0, name: "Level", module: "Output", min: 0, max: 1, value: 0.5, text: "50%", stepped: false },
  { id: 1, name: "Cutoff", module: "Filter 1", min: 0, max: 1, value: 0.8, text: "8.0 kHz", stepped: false },
  { id: 2, name: "Wave", module: "Osc 1", min: 0, max: 3, value: 0, text: "Sine", stepped: true },
];
// What the save and open dialogs "return". Null stands for cancelling.
fake.saveAs = "/elsewhere/Newer.beat";
fake.openFolder = null;

const pattern = (id) => fake.patterns.find((p) => p.id === id);

const lane = (p, track) => {
  let found = p.lanes.find((l) => l.track === track);
  if (!found) {
    found = { track, notes: [] };
    p.lanes.push(found);
  }
  return found;
};

function freeId(taken) {
  for (let id = 0; id < MAX_PATTERNS; id++) {
    if (!taken.includes(id)) return id;
  }
  return null;
}

function nextName() {
  for (let n = 1; ; n++) {
    const name = `Pattern ${n}`;
    if (!fake.patterns.some((p) => p.name === name)) return name;
  }
}

function sortSong() {
  fake.song.sort((a, b) => a.step - b.step || a.pattern - b.pattern);
}

function trim(p) {
  for (const l of p.lanes) {
    l.notes = l.notes.filter((n) => n.step < p.steps);
  }
  p.lanes = p.lanes.filter((l) => l.notes.length);
}

const arrangement = () => ({ patterns: fake.patterns, song: fake.song });

/* How a track sits in a pattern, made on the spot if nobody has touched it yet. */
const DEFAULT_GAIN = 0.8;

/* And how its sound is played, which belongs to the track. Mirrors Voicing::default. */
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

/* Mirrors Voicing::settled: every control pushed back inside the range its editor offers. */
function settleVoicing(voicing) {
  const clamp = (v, low, high) => Math.min(high, Math.max(low, Number(v) || 0));
  const start = clamp(voicing.start, 0, 0.99);
  return {
    attack: clamp(voicing.attack, 0, 10),
    decay: clamp(voicing.decay, 0, 10),
    sustain: clamp(voicing.sustain, 0, 1),
    release: clamp(voicing.release, 0, 10),
    pan: clamp(voicing.pan, -1, 1),
    tune: clamp(voicing.tune, -24, 24),
    level: clamp(voicing.level, 0, 2),
    start,
    end: clamp(voicing.end, start + 0.01, 1),
  };
}

function setMix(id, track, change) {
  const p = pattern(id);
  if (!p) return null;
  p.mix = p.mix ?? [];
  let mix = p.mix.find((one) => one.track === track);
  if (!mix) {
    mix = { track, gain: DEFAULT_GAIN, muted: false, soloed: false, pitched: false };
    p.mix.push(mix);
    p.mix.sort((a, b) => a.track - b.track);
  }
  Object.assign(mix, change);
  // Nothing worth writing down about a track nobody has touched.
  p.mix = p.mix.filter(
    (one) => one.gain !== DEFAULT_GAIN || one.muted || one.soloed || one.pitched,
  );
  return null;
}

/* The block of a pattern that covers a step, which is what the song view hit tests. */
const covering = (id, step) =>
  fake.song.find(
    (one) => one.pattern === id && step >= one.step && step < one.step + Math.max(1, one.length),
  ) ?? null;

const startup = () => ({
  project: {
    version: 2,
    bpm: fake.bpm,
    masterGain: fake.masterGain,
    tracks: [...fake.tracks.values()],
    patterns: fake.patterns,
    song: fake.song,
  },
  name: fake.name,
  folder: fake.folder,
  waveforms: [...fake.tracks.values()].map((t) => ({ track: t.id, peaks: fake.peaks })),
  message: null,
});

// Mirrors add_all in commands.rs, including what it refuses and what it says about it.
function addAll(paths) {
  const added = { tracks: [], failed: [] };
  for (const path of paths) {
    const base = path.split("/").pop();
    if (!/\.(wav|mp3|flac|ogg|oga|aiff?|aifc|m4a|mp4|aac|caf|wave)$/i.test(path)) {
      added.failed.push(`${base} is not a sound file`);
      continue;
    }
    if (fake.tracks.size >= MAX_TRACKS) {
      added.failed.push(`that is ${MAX_TRACKS} tracks, which is all of them`);
      continue;
    }
    const id = fake.nextTrackId++;
    const name = base.replace(/\.[^.]+$/, "");
    const track = {
      id,
      name,
      // Rust copies the file into the project folder and refers to it from there.
      sample: { path: `samples/${base}`, name },
      voicing: DEFAULT_VOICING(),
    };
    fake.tracks.set(id, track);
    added.tracks.push({ track, peaks: fake.peaks });
  }
  return added;
}

const handlers = {
  startup,
  audition: () => null,
  add_instruments: () => addAll(fake.picks),
  add_dropped: ({ paths }) => addAll(paths),
  remove_track: ({ id }) => {
    fake.tracks.delete(id);
    for (const p of fake.patterns) {
      p.lanes = p.lanes.filter((l) => l.track !== id);
      p.mix = (p.mix ?? []).filter((one) => one.track !== id);
    }
    return null;
  },
  set_step: ({ pattern: id, track, step, on }) => {
    const p = pattern(id);
    if (!p || step >= p.steps) return false;
    const l = lane(p, track);
    const at = l.notes.findIndex((n) => n.step === step);
    if (on && at < 0) l.notes.push({ step, pitch: 60, velocity: 100, length: 1 });
    if (!on && at >= 0) l.notes.splice(at, 1);
    p.lanes = p.lanes.filter((one) => one.notes.length);
    return on;
  },
  // The whole mixer belongs to the pattern, not to the track: how loud, what is heard, and
  // which tracks are instruments. Mirrors Pattern::set_mix, including throwing the record
  // away again when it is back to how a new pattern starts.
  set_pattern_gain: ({ pattern: id, track, gain }) => setMix(id, track, { gain }),
  set_pattern_muted: ({ pattern: id, track, muted }) => setMix(id, track, { muted }),
  set_pattern_soloed: ({ pattern: id, track, soloed }) => setMix(id, track, { soloed }),
  set_pattern_pitched: ({ pattern: id, track, pitched }) => setMix(id, track, { pitched }),

  // --- CLAP plugins. A track's sound can be an instrument that is installed on the machine
  // rather than a file, and then the notes go to that instead.
  list_plugins: ({ again }) => {
    if (again) fake.scans += 1;
    return fake.plugins;
  },
  add_plugin: ({ path, id }) => {
    const found = fake.plugins.find((one) => one.id === id);
    if (!found) throw new Error(`${id} is not in ${path}`);
    if (!found.instrument) throw new Error(`${found.name} is not an instrument`);
    if (fake.tracks.size >= MAX_TRACKS) {
      throw new Error(`that is ${MAX_TRACKS} tracks, which is all of them`);
    }
    const track = {
      id: fake.nextTrackId++,
      name: found.name,
      sample: null,
      plugin: { path, id, name: found.name, state: null },
      voicing: DEFAULT_VOICING(),
    };
    fake.tracks.set(track.id, track);
    fake.pluginParams.set(track.id, fake.params.map((one) => ({ ...one })));
    // A plugin has no waveform: it is an instrument, not a file.
    return { track, peaks: [] };
  },
  plugin_params: ({ id }) => (fake.pluginParams.get(id) ?? []).map((one) => ({ ...one })),
  set_plugin_param: ({ id, param, value }) => {
    const found = (fake.pluginParams.get(id) ?? []).find((one) => one.id === param);
    if (found) {
      found.value = value;
      // The plugin says what its value means, so a moved control gets a new word for it.
      found.text = `${Math.round(value * 100)}%`;
    }
    return null;
  },

  // How the sound is played, which belongs to the track rather than to any pattern. Hands
  // back what it settled on, not what it was asked for, the same as Rust does.
  set_voicing: ({ id, voicing }) => {
    const settled = settleVoicing(voicing);
    const track = fake.tracks.get(id);
    if (track) track.voicing = settled;
    return settled;
  },

  // The piano roll's three commands. A note is identified by where it is.
  set_note: ({ pattern: id, track, at, velocity, length }) => {
    const p = pattern(id);
    if (!p) return { fits: false, steps: 0 };
    // A note past the end makes the pattern longer rather than being refused.
    const wanted = at.step + Math.max(1, length);
    if (wanted > p.steps) p.steps = Math.max(1, Math.min(MAX_STEPS, wanted));
    if (at.step >= p.steps) return { fits: false, steps: 0 };
    const l = lane(p, track);
    const note = {
      step: at.step,
      pitch: at.pitch,
      velocity: Math.max(1, Math.min(127, velocity)),
      length: Math.max(1, Math.min(length, p.steps - at.step)),
    };
    const was = l.notes.findIndex((n) => n.step === at.step && n.pitch === at.pitch);
    if (was >= 0) l.notes[was] = note;
    else if (l.notes.length >= MAX_NOTES) return { fits: false, steps: p.steps };
    else l.notes.push(note);
    return { fits: true, steps: p.steps };
  },
  clear_note: ({ pattern: id, track, at }) => {
    const p = pattern(id);
    if (!p) return null;
    const l = lane(p, track);
    l.notes = l.notes.filter((n) => !(n.step === at.step && n.pitch === at.pitch));
    p.lanes = p.lanes.filter((one) => one.notes.length);
    return null;
  },
  move_note: ({ pattern: id, track, at, to }) => {
    const p = pattern(id);
    if (!p || to.step >= p.steps) return false;
    const l = lane(p, track);
    const found = l.notes.find((n) => n.step === at.step && n.pitch === at.pitch);
    if (!found) return false;
    found.step = to.step;
    found.pitch = to.pitch;
    found.length = Math.max(1, Math.min(found.length, p.steps - to.step));
    return true;
  },

  add_pattern: () => {
    const id = freeId(fake.patterns.map((p) => p.id));
    if (id === null) throw new Error("that is as many patterns as there is room for");
    fake.patterns.push({ id, name: nextName(), steps: 16, mix: [], lanes: [] });
    return arrangement();
  },
  duplicate_pattern: ({ id }) => {
    const free = freeId(fake.patterns.map((p) => p.id));
    const at = fake.patterns.findIndex((p) => p.id === id);
    if (free === null || at < 0) throw new Error("that is as many patterns as there is room for");
    const copy = JSON.parse(JSON.stringify(fake.patterns[at]));
    copy.id = free;
    copy.name = nextName();
    fake.patterns.splice(at + 1, 0, copy);
    return arrangement();
  },
  remove_pattern: ({ id }) => {
    if (fake.patterns.length <= 1) throw new Error("a song needs at least one pattern");
    fake.patterns = fake.patterns.filter((p) => p.id !== id);
    fake.song = fake.song.filter((one) => one.pattern !== id);
    return arrangement();
  },
  rename_pattern: ({ id, name }) => {
    const p = pattern(id);
    if (!p) return "";
    const trimmed = name.trim();
    p.name = trimmed ? trimmed.slice(0, 40) : nextName();
    return p.name;
  },
  set_pattern_colour: ({ id, colour }) => {
    const p = pattern(id);
    if (!p) return false;
    p.colour = colour;
    return true;
  },
  rename_project: ({ name }) => {
    const wanted = name.trim().slice(0, 60);
    if (!wanted) throw new Error("a project needs a name");
    if (wanted.includes("/")) throw new Error(`${wanted} is not a name a folder can have`);
    fake.name = wanted;
    fake.folder = `/tmp/${wanted}.beat`;
    return fake.name;
  },
  // Mirrors Project::set_pattern_steps: the notes off the end go, and the song is left
  // alone — a block in it is as long as it was drawn, whatever its pattern does after.
  set_pattern_steps: ({ id, steps }) => {
    const p = pattern(id);
    if (!p) return [0, arrangement()];
    p.steps = Math.max(1, Math.min(MAX_STEPS, steps));
    trim(p);
    return [p.steps, arrangement()];
  },
  open_pattern: ({ id }) => { fake.active = id; fake.songMode = false; return null; },
  close_pattern: () => { fake.songMode = true; return null; },

  /*
   * Mirrors Project::place and Project::unplace. A block starts where it is put, is as long
   * as it is asked to be — zero meaning one play-through of the pattern — and anything of
   * the same pattern it lands on makes way for it.
   */
  place_pattern: ({ pattern: id, step, length, on }) => {
    const p = pattern(id);
    if (!p) return fake.song;
    if (!on) {
      fake.song = fake.song.filter((one) => !(one.pattern === id && one.step === step));
      return fake.song;
    }
    const want = length || Math.max(1, p.steps);
    if (step + want > MAX_SONG_BARS * 16) return fake.song;
    fake.song = fake.song.filter(
      (one) => !(one.pattern === id && one.step < step + want && step < one.step + one.length),
    );
    fake.song.push({ step, pattern: id, length: want });
    sortSong();
    return fake.song;
  },
  move_placement: ({ pattern: id, from, to }) => {
    const one = covering(id, from);
    if (!one) return fake.song;
    const length = one.length;
    fake.song = fake.song.filter((other) => other !== one);
    return handlers.place_pattern({ pattern: id, step: to, length, on: true });
  },
  resize_placement: ({ pattern: id, step, length }) => {
    const one = covering(id, step);
    if (!one) return fake.song;
    const at = one.step;
    fake.song = fake.song.filter((other) => other !== one);
    return handlers.place_pattern({
      pattern: id,
      step: at,
      length: Math.max(1, length),
      on: true,
    });
  },
  clear_song_bar: ({ bar }) => {
    const from = bar * 16;
    fake.song = fake.song.filter((one) => one.step < from || one.step >= from + 16);
    return fake.song;
  },
  seek_song: ({ step }) => { fake.step = step; return null; },

  // Undo and redo. `stepBack` is declared below, and hoists.
  undo: () => stepBack(true),
  redo: () => stepBack(false),

  set_bpm: ({ bpm }) => { fake.bpm = Math.max(40, Math.min(240, bpm)); return fake.bpm; },
  set_playing: ({ playing }) => {
    fake.playing = playing;
    if (!playing) fake.step = 0;
    return null;
  },
  panic_stop: () => { fake.playing = false; fake.step = 0; return null; },
  playhead: () => ({
    playing: fake.playing,
    step: fake.step,
    progress: 0.3,
    // One bit per pattern: everything covering this step sounds at once.
    patterns: fake.songMode
      ? fake.song
          .filter((one) => fake.step >= one.step && fake.step < one.step + Math.max(1, one.length))
          .reduce((mask, one) => mask | (1 << one.pattern), 0)
      : 1 << fake.active,
    voices: fake.playing ? 3 : 0,
    peak: fake.playing ? 0.6 : 0,
    streamErrors: 0,
    saveError: null,
  }),
};

/*
 * Undo and redo, mirroring AppState: a copy of the whole project before each edit, and edits
 * of the same kind close together counted as one step, so a drag is one thing to take back.
 */
const HISTORY_DEPTH = 128;
const COALESCE_MS = 600;
const history = { past: [], future: [], last: null };

const snapshot = () => ({
  bpm: fake.bpm,
  tracks: [...fake.tracks.entries()].map(([id, track]) => [id, { ...track }]),
  patterns: JSON.parse(JSON.stringify(fake.patterns)),
  song: JSON.parse(JSON.stringify(fake.song)),
  name: fake.name,
});

const restore = (kept) => {
  fake.bpm = kept.bpm;
  fake.tracks = new Map(kept.tracks.map(([id, track]) => [id, { ...track }]));
  fake.patterns = JSON.parse(JSON.stringify(kept.patterns));
  fake.song = JSON.parse(JSON.stringify(kept.song));
  fake.name = kept.name;
};

function remember(what, at) {
  const carryingOn = history.last && history.last.what === what && at - history.last.at < COALESCE_MS;
  history.last = { what, at };
  if (carryingOn) return;
  history.future.length = 0;
  history.past.push(snapshot());
  if (history.past.length > HISTORY_DEPTH) history.past.shift();
}

function stepBack(back) {
  const taken = back ? history.past.pop() : history.future.pop();
  if (!taken) return null;
  const leftBehind = snapshot();
  if (back) history.future.push(leftBehind);
  else history.past.push(leftBehind);
  restore(taken);
  history.last = null;
  return startup();
}

// What each edit is called, which is what decides where one step ends and the next begins.
const EDITS = {
  add_instruments: "tracks",
  add_dropped: "tracks",
  remove_track: "tracks",
  set_pattern_gain: "gain",
  set_pattern_muted: "mute",
  set_pattern_soloed: "solo",
  set_pattern_pitched: "pitched",
  set_voicing: "voicing",
  add_plugin: "tracks",
  set_step: "boxes",
  set_note: "notes",
  clear_note: "notes",
  move_note: "notes",
  add_pattern: "patterns",
  duplicate_pattern: "patterns",
  remove_pattern: "patterns",
  rename_pattern: "name",
  set_pattern_colour: "colour",
  set_pattern_steps: "length",
  place_pattern: "song",
  move_placement: "song",
  resize_placement: "song",
  clear_song_bar: "song",
  set_bpm: "tempo",
  // Renaming the project is not in here, because it is not in Rust either: the name is the
  // folder's, and a step back that did not rename the folder would be a step back in name
  // only.
};

for (const [name, what] of Object.entries(EDITS)) {
  const edit = handlers[name];
  handlers[name] = (args) => {
    remember(what, performance.now());
    return edit(args);
  };
}

// Lets a test see how many steps back there are without reaching into the history.
window.__weetbeats_history = () => ({ past: history.past.length, future: history.future.length });

// The native drag-drop events the webview sends instead of HTML5 ones.
const listeners = new Map();

window.__TAURI__ = {
  core: {
    invoke: async (name, args = {}) => {
      window.__weetbeats_calls.push({ name, args });
      const handler = handlers[name];
      if (!handler) throw new Error(`no such command: ${name}`);
      return handler(args);
    },
  },
  event: {
    listen: async (name, cb) => {
      listeners.set(name, cb);
      return () => listeners.delete(name);
    },
  },
};

// Lets a test fire a native drop without a real Finder.
window.__weetbeats_drop = (paths) => {
  listeners.get("tauri://drag-enter")?.({ payload: { paths } });
  listeners.get("tauri://drag-drop")?.({ payload: { paths } });
};

// Lets a test walk the playhead without waiting on real time.
window.__weetbeats_setStep = (step) => {
  fake.step = step;
  fake.playing = true;
};

// Opening and saving are in the native menu bar, which Rust owns: it tells the front end
// what happened by emitting an event. This is how a test plays the part of the menu.
window.__weetbeats_menu = (what) => {
  if (what === "save") {
    fake.saves += 1;
    listeners.get("saved")?.({ payload: fake.name });
    return;
  }
  if (what === "save_as" && fake.saveAs) {
    fake.folder = fake.saveAs;
    fake.name = fake.saveAs.split("/").pop().replace(/\.beat$/, "");
    listeners.get("project")?.({ payload: startup() });
    return;
  }
  if (what === "open" && fake.openFolder) {
    fake.folder = fake.openFolder;
    fake.name = fake.openFolder.split("/").pop().replace(/\.beat$/, "");
    listeners.get("project")?.({ payload: startup() });
    return;
  }
  if (what === "trouble") {
    listeners.get("trouble")?.({ payload: "the disk said no" });
  }
};

window.__weetbeats_state = fake;
