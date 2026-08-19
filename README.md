# Weetbeats

Delicious, ambitious, nutritious beats.

A tiny, fun, free music maker for macOS. Point it at a folder of samples, tick boxes, make
songs. Open source, free forever.

> **First time you open it:** right click the app, choose Open, then click Open again.
> Weetbeats is not signed, so macOS blocks it until you say otherwise.

## Where it is up to

Stage 1 of [the build plan](docs/BUILD_PLAN.md) is done: a working step sequencer.

- Pick a folder of samples, or use the drums that ship with it
- Click a sample to hear it, drag it right to make a track
- Sixteen boxes a row, click or drag across them to paint a beat
- Play, stop, tempo, master volume
- Per track: volume, mute, solo, delete

Patterns, songs and saving are stage 2.

## Running it

Needs [Rust](https://rustup.rs) and the [Tauri CLI](https://v2.tauri.app/reference/cli/).

```sh
cargo install tauri-cli --version "^2"
cargo tauri dev
```

To build a `.app`:

```sh
cargo tauri build
```

For a universal binary that runs on both Apple Silicon and Intel:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo tauri build --target universal-apple-darwin
```

## How it fits together

Three parts that never share a lock.

```
  Front end (ui/)              App thread (src-tauri/)      Audio thread (crates/engine)
  ---------------              -----------------------      ---------------------------
  draws the grid        <-->   owns the project       -->   plays sound
  handles clicks               decodes samples              never blocks
  polls the playhead           Tauri commands               never allocates
```

- `crates/engine` — the audio engine. No system dependencies at all, so it builds and
  tests on any machine. Step clock, voice pool, mixer, sample decoding, project model.
- `src-tauri` — the desktop app. Opens the audio device with `cpal`, decodes samples on
  worker threads, and answers the front end.
- `ui` — HTML, CSS and one file of JavaScript. No framework, no build step.
- `tools/starter-pack` — synthesises the eight drums in `assets/starter-pack`.
- `tools/icon` — draws the app icon.

### The one rule

The audio thread never allocates, locks, blocks or touches a file. A stall of a few
milliseconds is a click or a dropout, and there is no way to test your way out of one
after the fact — so it is tested for directly. `crates/engine/tests/no_alloc.rs` counts
every allocation during a few hundred blocks of heavy playback and fails if there is one.

Everything the audio thread needs is prepared elsewhere and handed over:

- Samples decode on a worker thread into an `Arc<Sample>` and arrive through a queue
- The UI sends commands over a lock free ring buffer (`rtrb`)
- Samples the audio thread finishes with are handed *back*, so nothing is freed there
- The playhead is an atomic the front end polls; nothing is pushed at it

### Steps are notes

A ticked box is a note at middle C, one step long — not a boolean. The piano roll in
stage 4 is then a different editor over the same data instead of a project file migration.

## Tests

```sh
cargo test                 # the engine: clock, voices, mixer, decoding, no allocations
node ui/test/run.js        # the front end in a real browser, with Rust stubbed out
```

The front end tests need Playwright, which is not otherwise required:

```sh
npm install playwright && npx playwright install chromium
```

## The drums

`assets/starter-pack` is synthesised from scratch by `tools/starter-pack`, so there is
nothing to attribute and no licence to honour. Regenerate them with:

```sh
cargo run -p starter-pack -- assets/starter-pack
```

For more, Freesound (filtered to CC0), the Legowelt sample archive, 99Sounds and
SampleRadar are all worth a look. Never bundle anything with an attribution requirement.

## Licence

MIT.
