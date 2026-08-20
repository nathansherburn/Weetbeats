/*
 * Stands in for the Rust side so the front end can be driven in a browser.
 * Mirrors what each Tauri command actually returns, including the casing.
 */
window.__weetbeats_calls = [];

const fake = {
  bpm: 120,
  masterGain: 0.9,
  steps: 16,
  playing: false,
  step: 0,
  nextId: 0,
  tracks: new Map(),
  peaks: Array.from({ length: 96 }, (_, i) => Math.abs(Math.sin(i / 7)) * 0.9),
};

// What the file picker "returns". A test sets this before clicking Add.
fake.picks = ["/pack/01 kick.wav"];

// Mirrors add_all in commands.rs, including what it refuses and what it says about it.
function addAll(paths) {
  const added = { tracks: [], failed: [] };
  for (const path of paths) {
    const base = path.split("/").pop();
    if (!/\.(wav|mp3|flac|ogg|oga|aiff?|aifc|m4a|mp4|aac|caf|wave)$/i.test(path)) {
      added.failed.push(`${base} is not a sound file`);
      continue;
    }
    if (fake.tracks.size >= 32) {
      added.failed.push("that is 32 tracks, which is all of them");
      continue;
    }
    const id = fake.nextId++;
    const name = path.split("/").pop().replace(/\.[^.]+$/, "");
    const track = { id, name, sample: { path, name }, gain: 0.8, muted: false, soloed: false, notes: [] };
    fake.tracks.set(id, track);
    added.tracks.push({ track, peaks: fake.peaks });
  }
  return added;
}

const handlers = {
  startup: () => ({
    project: {
      version: 1,
      bpm: fake.bpm,
      masterGain: fake.masterGain,
      pattern: { name: "Pattern 1", steps: fake.steps, tracks: [] },
    },
    audio: { device: "Test Output", sampleRate: 48000, channels: 2, format: "f32" },
  }),
  audition: () => null,
  add_instruments: () => addAll(fake.picks),
  add_dropped: ({ paths }) => addAll(paths),
  remove_track: ({ id }) => { fake.tracks.delete(id); return null; },
  set_step: ({ id, step, on }) => {
    const track = fake.tracks.get(id);
    if (!track) return false;
    const at = track.notes.findIndex((n) => n.step === step);
    if (on && at < 0) track.notes.push({ step, pitch: 60, velocity: 100, length: 1 });
    if (!on && at >= 0) track.notes.splice(at, 1);
    return on;
  },
  set_track_gain: ({ id, gain }) => { fake.tracks.get(id).gain = gain; return null; },
  set_track_muted: ({ id, muted }) => { fake.tracks.get(id).muted = muted; return null; },
  set_track_soloed: ({ id, soloed }) => { fake.tracks.get(id).soloed = soloed; return null; },
  set_bpm: ({ bpm }) => { fake.bpm = Math.max(40, Math.min(240, bpm)); return fake.bpm; },
  set_master_gain: ({ gain }) => { fake.masterGain = gain; return null; },
  set_playing: ({ playing }) => { fake.playing = playing; if (!playing) fake.step = 0; return null; },
  panic_stop: () => { fake.playing = false; fake.step = 0; return null; },
  playhead: () => ({
    playing: fake.playing,
    step: fake.step,
    progress: 0.3,
    voices: fake.playing ? 3 : 0,
    peak: fake.playing ? 0.6 : 0,
    streamErrors: 0,
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
window.__weetbeats_setStep = (step) => { fake.step = step; fake.playing = true; };
window.__weetbeats_state = fake;
