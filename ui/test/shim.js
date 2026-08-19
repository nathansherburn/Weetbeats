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

const entries = [
  "01 kick", "02 snare", "03 clap", "04 hat closed",
  "05 hat open", "06 rim", "07 tom", "08 cowbell",
].map((name) => ({ path: `/pack/${name}.wav`, name, folder: "" }));

const handlers = {
  startup: () => ({
    project: {
      version: 1,
      bpm: fake.bpm,
      masterGain: fake.masterGain,
      pattern: { name: "Pattern 1", steps: fake.steps, tracks: [] },
    },
    audio: { device: "Test Output", sampleRate: 48000, channels: 2, format: "f32" },
    samples: { root: "/pack", entries, truncated: false },
  }),
  choose_folder: () => ({ root: "/other", entries: entries.slice(0, 2), truncated: false }),
  preview: () => null,
  audition: () => null,
  waveform: () => fake.peaks,
  add_track: ({ path }) => {
    if (fake.tracks.size >= 32) throw "that is 32 tracks, which is all of them";
    const id = fake.nextId++;
    const name = path.split("/").pop().replace(/\.wav$/, "");
    const track = { id, name, sample: { path, name }, gain: 0.8, muted: false, soloed: false, notes: [] };
    fake.tracks.set(id, track);
    return { track, peaks: fake.peaks };
  },
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

window.__TAURI__ = {
  core: {
    invoke: async (name, args = {}) => {
      window.__weetbeats_calls.push({ name, args });
      const handler = handlers[name];
      if (!handler) throw new Error(`no such command: ${name}`);
      return handler(args);
    },
  },
};

// Lets a test walk the playhead without waiting on real time.
window.__weetbeats_setStep = (step) => { fake.step = step; fake.playing = true; };
window.__weetbeats_state = fake;
