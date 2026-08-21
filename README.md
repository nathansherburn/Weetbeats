# Weetbeats

Delicious, ambitious, nutritious beats.

A tiny, fun, free music maker for macOS. Add a few sounds, tick boxes, make songs.
Open source, free forever.

> **First time you open it:** right click the app, choose Open, then click Open again.
> Weetbeats is not signed, so macOS blocks it until you say otherwise.

## Where it is up to

Stages 1 and 2 of [the build plan](docs/BUILD_PLAN.md) are done: a step sequencer, and
patterns strung together into a song.

- Add instruments from the system file picker, or drop sounds on the window
- Eight drums ship with it, and the picker opens on them the first time
- Click or drag across the boxes to paint a beat; right click rubs one out
- Set how many boxes a pattern has, one step at a time
- Patterns down the left: click to open one, click it again to go back to the song
- Song view: paint a pattern across the bars, and stack patterns to build a beat up
- Click a track's name to hear it
- Play, stop, tempo
- Per track: volume, mute, solo, delete

The sampler instrument is stage 3, the piano roll stage 4.

### Patterns and the song

A pattern is a grid of boxes and a length. An instrument belongs to the project rather than
to one pattern, so every pattern plays the same kit and a new pattern is an empty grid over
sounds you already have.

In the song, **one click is one play-through of one pattern**. Every lane is divided by its
own pattern's length, so a four step pattern goes in four steps at a time and a thirty two
step pattern takes thirty two — nothing is padded out to fill a bar it did not ask for.

Placements **overlap freely**, so a kick pattern, a hat pattern and a snare pattern play
together and add up to a beat. That is the whole reason to have patterns rather than one long
grid: build the parts separately, then bring them in one at a time across the song.

The song is painted, not ticked: press and drag along a lane to fill it in, and right click to
rub a placement out. Bars of sixteen steps are the ruler along the top — drag along them to
play from anywhere, and right click one to empty it.

Patterns are windows over the song. Click one in the panel to open it, click it again — or
press escape, or the × in the corner, or **song** at the top of the panel — to put it away.

## Projects

A project is a folder, so you can send one to a friend in one piece.

```
MySong.beat/
  project.json
  samples/
    kick.wav
    clap.wav
```

Samples are copied in the moment their track is added, and deleted when the last track using
one goes — not gathered up at save time. So the folder always holds exactly what the project
uses, and moving or deleting the file you dragged in cannot break anything. The only way to
break a project is to go into its folder and break it by hand.

The project is written out about once a second when anything has changed, and again when the
window closes, so there is nothing to remember to do. The **File** menu has Open, Save and
Save As: Save As puts a copy wherever you like and carries on working there, Open picks up
another folder. Weetbeats reopens whatever you had last time.

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
  tests on any machine. Step clock, voice pool, mixer, sample decoding, the project model
  and the project folder on disk.
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

### Every pattern lives on the audio thread

The engine holds the notes of every pattern, not just the ones playing. At the bar line the
song moves on *on the boundary frame*, and there is no time to ask the app thread for what
comes next — so a bar of the song is a `u32` with a bit per pattern, and moving on is reading
the next one. Each pattern also carries how far into its run of bars it is, which is what
makes it play through them rather than starting again at every bar.

Which patterns play depends on the mode: the open pattern loops while you are editing it, and
the song plays when you are looking at the song.

### No master volume

There is a level meter but no master fader. The system volume covers "make it quieter", and
the track faders cover "make this bit quieter", which between them is everything a fader
would have done except pulling the whole mix back off the soft clipper — and if the meter is
pinned, pulling the tracks down is the better answer anyway. `masterGain` is still in
`project.json` for anyone who wants to lean on it.

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
