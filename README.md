# Weetbeats

Delicious, ambitious, nutritious beats.

A tiny, fun, free music maker for macOS. Add a few sounds, tick boxes, make songs.
Open source, free forever.

> **First time you open it:** right click the app, choose Open, then click Open again.
> Weetbeats is not signed, so macOS blocks it until you say otherwise.

## Where it is up to

Stages 1 to 4 of [the build plan](docs/BUILD_PLAN.md) are done: a step sequencer, patterns
strung together into a song, samples played across the keyboard, and a piano roll to write
with.

- Add instruments from the system file picker, or drop sounds on the window
- Eight drums ship with it, and the picker opens on them the first time
- Click or drag across the boxes to paint a beat; right click rubs one out
- Set how many boxes a pattern has, one step at a time
- Patterns down the left: click one to pick it out, double click a block to edit it
- Song view: draw a pattern anywhere, drag it about, drag its ends to say how long it plays
- Snap, zoom (buttons or a trackpad pinch) in the song and in the piano roll
- A colour per pattern, lighter while it sounds
- Undo and redo, with a drag counted as one step
- Any track can be a pitched instrument instead of a drum, with a piano roll to write in
- An instrument's row in a pattern shows its notes instead of boxes; click it for the roll
- Click a track's name to hear it, or a key in the roll to hear that note
- Play, stop, tempo, and a playhead you can drag whether or not it is playing
- Per track: volume, mute, solo, delete

Effects are stage 5, hosting CLAP plugins stage 6.

### Patterns and the song

A pattern is a grid of boxes and a length. An instrument belongs to the project rather than
to one pattern, so every pattern plays the same kit and a new pattern is an empty grid over
sounds you already have.

In the song a pattern goes down as a **block**. A new block is one play-through — four steps
of a four step pattern, thirty two of a thirty two step one, nothing padded out to fill a bar
it did not ask for — and then it is its own length: drag its right hand end out and the
pattern **repeats** to fill it, drag it in and the pattern is **cut off** part way through.
Drag the middle to slide it along, drag the left hand end to change where it comes in.

A block starts wherever the **snap** puts it, which is a bar by default and anything from one
step up. It is not tied to its pattern's own grid, so a thirty two step pattern can start on
a half bar, and changing a pattern's length never moves or resizes what is already in the
song.

Blocks **overlap freely** between patterns, so a kick pattern, a hat pattern and a snare
pattern play together and add up to a beat. That is the whole reason to have patterns rather
than one long grid: build the parts separately, then bring them in one at a time across the
song. Two blocks of the *same* pattern cannot overlap — one pattern cannot play over itself
— so dropping one on another takes its place.

The song is drawn, not ticked: press and drag along a lane to fill it in, and right click to
rub a block out. Each pattern gets its own colour, lighter while it is sounding; the swatch
on its row in the panel changes it. Bars of sixteen steps are the ruler along the top — drag
along it to move the playhead, playing or not, and right click a bar to empty it. Zoom with
the buttons in the corner or by pinching the trackpad.

Patterns are windows over the song. Click one in the panel to open it, click it again — or
press escape, or the × at the top left of the editor — to put it away, and **double click a
block in the song** to open the pattern in it. The name at the top of the panel is the
project's, and it is also the way back to the song; double click it to rename the project,
which renames its folder.

An open pattern wears the colour of its blocks: the × and the name tab at the top left, the
line under the ruler, and every box and note inside. Opening a pattern should look like
pulling one of its blocks open, not like going somewhere else.

That titlebar sits outside the scrolling area, which is deliberate. Pinned to the far right
the close button could end up under a scrollbar or clipped by the window's own corner; put in
the grid as a sticky cell it holds for about a windowful of scrolling and then slides away,
which is a thing sticky grid items do.

### Instruments and the piano roll

A track is a drum until you say otherwise: hit it and the whole sample plays, however short
the note is. Press **♪** on a track and it becomes an instrument instead — the sample is
pitched across the keyboard, faster for higher notes and slower for lower ones, and a note
**stops when it ends**. Press it again and it is a drum again.

That one switch also changes what its row looks like. An instrument's row is a **small piano
roll** of its own notes rather than a line of boxes, because boxes cannot say which pitch or
how long. Click the small one to open the roll proper, and press **♪** to go back to boxes.
Nothing is lost either way: they are two views of one lane of notes.

In the roll, press to draw a note and keep dragging to set how long it is; the next one you
draw comes out that long. Grab a note to move it, grab its right hand end to stretch it,
right click to rub it out. Everything you touch plays as you touch it. How hard each note is
hit is the lane underneath — drag it. Drawing **past the end** of the pattern makes the
pattern longer, which is how one bar becomes two.

A box in the step grid *is* a note in the roll: middle C, one step long. There is no
converting between them and nothing to keep in step.

What plays is what you can see. A row of boxes plays only the notes a box can mean — the ones
at middle C — so a melody written in the roll goes quiet when you switch the row back to
boxes, and comes back the moment you switch it again. Nothing is deleted either way.

## Projects

A project is a folder, so you can send one to a friend in one piece.

```
MySong.beat/
  project.json
  samples/
    kick.wav
    clap.wav
    .undo/        # samples a deleted track might still want back
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

A ticked box is a note at middle C, one step long — not a boolean. Which is why the piano
roll, four stages later, was a new editor rather than a project file migration: it writes
into the same lane the boxes do, at any pitch and any length.

### Every pattern lives on the audio thread

The engine holds the notes of every pattern, not just the ones playing. A block starts *on
the boundary frame*, and there is no time to ask the app thread what comes next — so the song
is two grids the engine owns outright: a step of it is a `u32` with a bit per pattern saying
what starts there, and beside it a `u16` per pattern saying how long that block is. Starting
a block is reading two numbers. Each pattern also carries how far into its block it is, which
is what makes it play through, come round again when the block is longer than the pattern, and
stop part way when it is shorter.

Seeking is the only place that has to look backwards, and blocks of one pattern never overlap,
so the nearest start behind the playhead is the only one that can still be sounding: if its
length has run out, no earlier one is any better.

Which patterns play depends on the mode: the open pattern loops while you are editing it, and
the song plays when you are looking at the song.

### A block is its own length

A pattern's length and a block's length are different things, and the second one does not
follow the first. Change a four step pattern to sixteen and its blocks stay four steps long
where they are — the file describes the same music afterwards as before.

The alternative, snapping blocks onto the new grid, is what an earlier version did, and it is
how a project ended up with blocks nobody could click: they were written on one grid and hit
tested on another. Both halves of that are fixed. Blocks are hit tested over their own length,
wherever they sit, and opening a project never moves anything — the only thing it throws away
is a block whose pattern is gone.

### Undo is a copy of the whole project

Not a list of changes and their opposites. Every edit would otherwise need an opposite, and
its opposite's opposite for redo, and the one that gets forgotten is the one that loses your
work. A project is a few hundred notes and a list of samples — small enough to copy — so it
is copied, before each edit, up to a hundred and twenty eight steps back.

Edits of the same kind less than 600ms apart are one step, so a drag across sixteen boxes
comes back in one go rather than sixteen.

The one thing a copy of `project.json` cannot put back is a file. Deleting the last track
using a sample used to delete the sample, which would make an undo a liar — the track would
come back pointing at nothing. So it is moved to `samples/.undo/` inside the project instead,
and undo brings it out again. Opening a project throws that folder away, because a window
that has just opened has nothing to undo.

"Save as" takes the stash with it, which looks like clutter in a copy and is not: the window's
history still points at those files and the copy is where the project now lives, so leaving
them behind would mean taking back a deleted track and getting the track without its sound.
The copy is cleaned the first time anyone opens it. If a sample ever does go missing anyway,
the step back says so rather than leaving you with a silent track.

Renaming the project is the one edit outside the history: the name is the folder's, not
`project.json`'s, and a step back that left the folder where it was would be a step back in
name only.

Undo and redo are in the Edit menu, and that is not a detail — on macOS a menu item's key
equivalent is handled before the window sees the key, so a standard Edit menu would swallow
cmd-Z and hand it to the webview, which would undo typing and nothing else. Ours emit an
event; cut, copy, paste and select all are still the standard items.

### The meter is in decibels, and it is a transform

A linear meter spends nearly all of its travel in the top six decibels, so a mix at a sensible
level barely moves it while a raw one-shot on its own slams it — which reads as a meter that
does not work. The engine holds the peak and lets it fall at 1.6 per second so a hit between
two of the front end's polls is still seen; the front end draws that peak on a scale from
&minus;48 dB up.

It is drawn by scaling an opaque cover over a gradient, not by animating a width. A width has
to lay the page out, and it was being set sixty times a second behind a song view that was
already redrawing every frame — which is why it looked dead in song mode in particular. A
transform is the one thing a browser can animate without laying out anything.

### Zoom is one zoom per frame

A trackpad pinch arrives as a run of wheel events, far faster than the screen refreshes.
Zooming on each one meant several relayouts a frame, which is what made it judder, so they
are added up and applied once, in the frame that draws. The song's canvases do not change
size when it zooms at all — they are the window's width at every zoom — so all that changes
there is how wide the song says it is.

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
node ui/test/contract.js   # the front end and Rust agree about the commands
```

The contract check is there because the front end talks to Rust through strings: it reads
both sides and compares the command names and their arguments. `run.js` runs it first, since
a stub that answers differently from the real thing makes every check after it meaningless.

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
