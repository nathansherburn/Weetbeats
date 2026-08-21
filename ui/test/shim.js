/*
 * Stands in for the Rust side so the front end can be driven in a browser.
 * Mirrors what each Tauri command actually does, including what it refuses and the casing
 * it answers in. Where the real thing has a rule — one slot is one pattern, a shortened
 * pattern loses the notes off its end — the rule is here too, so a test that passes here
 * is testing the same behaviour.
 */
window.__weetbeats_calls = [];

const MAX_PATTERNS = 32;
const MAX_TRACKS = 32;
const MAX_STEPS = 64;
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
  patterns: [{ id: 0, name: "Pattern 1", steps: 16, lanes: [] }],
  // { step, pattern }: what plays where.
  song: [],
  name: "Untitled",
  folder: "/tmp/Untitled.beat",
  saves: 0,
  peaks: Array.from({ length: 96 }, (_, i) => Math.abs(Math.sin(i / 7)) * 0.9),
};

// What the file picker "returns". A test sets this before clicking Add.
fake.picks = ["/pack/01 kick.wav"];
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
      gain: 0.8,
      muted: false,
      soloed: false,
      pitched: false,
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
  set_track_gain: ({ id, gain }) => { fake.tracks.get(id).gain = gain; return null; },
  set_track_muted: ({ id, muted }) => { fake.tracks.get(id).muted = muted; return null; },
  set_track_soloed: ({ id, soloed }) => { fake.tracks.get(id).soloed = soloed; return null; },
  set_track_pitched: ({ id, pitched }) => { fake.tracks.get(id).pitched = pitched; return null; },

  // The piano roll's three commands. A note is identified by where it is.
  set_note: ({ pattern: id, track, at, velocity, length }) => {
    const p = pattern(id);
    if (!p || at.step >= p.steps) return false;
    const l = lane(p, track);
    const note = {
      step: at.step,
      pitch: at.pitch,
      velocity: Math.max(1, Math.min(127, velocity)),
      length: Math.max(1, Math.min(length, p.steps - at.step)),
    };
    const was = l.notes.findIndex((n) => n.step === at.step && n.pitch === at.pitch);
    if (was >= 0) l.notes[was] = note;
    else if (l.notes.length >= MAX_NOTES) return false;
    else l.notes.push(note);
    return true;
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
    fake.patterns.push({ id, name: nextName(), steps: 16, lanes: [] });
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
  // Mirrors Project::set_pattern_steps: the notes off the end go, and the pattern's places
  // in the song move onto the new grid.
  set_pattern_steps: ({ id, steps }) => {
    const p = pattern(id);
    if (!p) return [0, arrangement()];
    p.steps = Math.max(1, Math.min(MAX_STEPS, steps));
    trim(p);
    const seen = new Set();
    fake.song = fake.song.filter((one) => {
      if (one.pattern !== id) return true;
      one.step = Math.floor(one.step / p.steps) * p.steps;
      if (seen.has(one.step)) return false;
      seen.add(one.step);
      return true;
    });
    sortSong();
    return [p.steps, arrangement()];
  },
  open_pattern: ({ id }) => { fake.active = id; fake.songMode = false; return null; },
  close_pattern: () => { fake.songMode = true; return null; },

  // Mirrors Project::set_placement: a slot is as long as the pattern in it.
  place_pattern: ({ pattern: id, slot, on }) => {
    const p = pattern(id);
    if (!p) return false;
    const step = slot * Math.max(1, p.steps);
    fake.song = fake.song.filter((one) => !(one.pattern === id && one.step === step));
    if (!on) return false;
    if (step + p.steps > MAX_SONG_BARS * 16) return false;
    fake.song.push({ step, pattern: id });
    sortSong();
    return true;
  },
  clear_song_bar: ({ bar }) => {
    const from = bar * 16;
    fake.song = fake.song.filter((one) => one.step < from || one.step >= from + 16);
    return fake.song;
  },
  seek_song: ({ step }) => { fake.step = step; return null; },

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
          .filter((one) => {
            const p = pattern(one.pattern);
            return p && fake.step >= one.step && fake.step < one.step + p.steps;
          })
          .reduce((mask, one) => mask | (1 << one.pattern), 0)
      : 1 << fake.active,
    voices: fake.playing ? 3 : 0,
    peak: fake.playing ? 0.6 : 0,
    streamErrors: 0,
    saveError: null,
  }),
};

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
