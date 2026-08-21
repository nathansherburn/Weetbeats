# Weetbeats - build plan

A tiny, fun, free music maker for macOS. Point it at a folder of samples, tick boxes, make songs.

## Decisions made

| Thing | Choice |
|---|---|
| Language | Rust |
| UI | Tauri, with an HTML/CSS/SVG front end |
| Audio out | `cpal` |
| Instruments | Sampler first. Host real plugins later |
| Plugin format | CLAP only. No VST3, no AU |
| Pattern length | 16 steps by default, adjustable per pattern, one step at a time, up to 64 |
| Song grid | Bars of 16 steps. A bar holds any number of patterns, all playing together |
| Steps | Plain on/off boxes |
| Projects | A folder, with samples copied in as they are added |
| Export | None yet, playback only |
| Distribution | Unsigned .app, users click through the warning |
| Licence | Open source, free forever |

## Why CLAP only

CLAP is a plain C API, MIT licensed, community owned. `clack-host` is a real Rust crate for building hosts. Nothing equivalent exists for VST3, where you'd be hand-writing bindings to a large C++ SDK. AU means Objective-C glue.

The supply is fine. The best free synths all ship CLAP: Surge XT, Odin 2, Vital, Cardinal. You don't need hundreds of plugins, you need five good ones.

Going C++ and JUCE wouldn't help, because JUCE doesn't host CLAP either. The only module that does is an unfinished side project.

## Architecture

Three parts. Keep them separate and this stays easy.

```
  Front end (HTML/JS)          Rust app state            Audio thread
  ------------------           --------------            ------------
  draws the grid        <-->   owns the project   -->    plays sound
  handles clicks              save/load, samples         never blocks
  polls playhead              Tauri commands             never allocates
```

### The one rule that matters

The audio thread must never allocate memory, lock a mutex, or touch a file. If it stalls for even a few milliseconds you get a click or a dropout. Everything it needs gets loaded ahead of time on another thread and handed over.

That means:

- Samples decode on a worker thread into `Arc<Vec<f32>>`, then get sent across
- The UI sends commands over a lock free ring buffer (`rtrb`)
- The audio thread writes the playhead position into an atomic, the UI reads it
- No `println!` on the audio thread, it allocates

### Don't animate from events

Tauri's message passing is not real time. If you fire an event on every step, the playhead will judder. Instead, poll the atomic from `requestAnimationFrame` in the front end. Smooth, and costs nothing.

### Crates

- `cpal` - audio output
- `symphonia` - decodes wav, mp3, flac, ogg
- `rtrb` - lock free queue between UI and audio
- `serde` + `serde_json` - project files
- `clack-host` - only at stage 6
- `hound` - only if you add WAV export

## Stage 1 - Beats

The whole app in miniature. Get this feeling good before anything else.

**What it does**

- Pick a samples folder, app scans it and shows a list
- Click a sample to hear it
- Drag a sample into the pattern, it becomes a track row
- 16 boxes per row, click to toggle
- Play, stop, BPM slider
- Per track: volume, mute, solo, delete

**Under the hood**

- Step clock driven by the audio callback, not by a timer. Sample count is the only honest clock.
- Each active step spawns a voice. A voice is a sample plus a read position.
- Fixed voice pool, say 64. Steal the oldest when you run out.
- Mixer sums voices, applies track volume, soft clips the master so it can't blow up.

**Store steps as notes, not booleans**

A step is a note at a fixed pitch. If the data model is notes from day one, the piano roll in stage 4 comes almost free. Skip this and you'll be migrating project files later.

**UX targets**

- Space bar plays and stops
- The grid should be one canvas or CSS grid, not 16 separate elements per row
- Click and drag across boxes to paint them on, like FL Studio
- Loading a sample takes zero dialogs

**Where to get samples**

Freesound (filter to CC0), the Legowelt sample archive, 99Sounds, SampleRadar. Ship a small CC0 starter pack so the app makes noise the second it opens. Never bundle anything with an attribution requirement.

## Stage 2 - Beats into songs

**What it does**

- Patterns get names, you can add, duplicate and delete them
- Song view: patterns down the left, bars across the top, paint a pattern across the bars
- Playhead sweeps across the song view
- Switch between pattern view and song view with one click
- Save and load projects

**Project format**

A folder, so it's easy to send to a friend.

```
MySong.beat/
  project.json
  samples/
    kick.wav
    clap.wav
```

`project.json` holds BPM, patterns, tracks and the song. Sample paths are relative to the folder.

**Samples are copied in when they are added, not when the project is saved.** A save-time copy leaves a window where the project points at a file somewhere else, which can move or be deleted in the meantime. Copying on the way in closes it: from the moment a sound is in the project, the project owns a copy, and the only way to break a project is to go into its folder and break it by hand. The other half of the deal is that a sample is deleted when the last track using it goes, so the folder holds exactly what the project uses and nothing else.

**Decided**

A song slot is one bar, and a bar is 16 steps — one pattern of the default length. A bar holds **any number of patterns**, and they all sound together: that is what makes a kick pattern, a hat pattern and a snare pattern add up to a beat, which is the whole reason to have patterns rather than one long grid.

A pattern is not restarted at every bar. Each one carries how far into its run of bars it is, so a pattern painted across four bars plays through those four bars — twice if it is two bars long, four times over if it is four steps long. A pattern painted into less room than it needs gets cut off at the end of the run, and the block says so.

The first pass at this had one slot per pattern, with the slot as long as whatever pattern was in it. It read well on paper and was wrong in the hand: nothing could stack, and with every column a different width there was no sensible way to drag across the song to fill it in.

The engine holds every pattern's notes so it can move on at the bar line without asking the app thread for anything. A bar of the song is a `u32` of pattern bits, which is what caps patterns at 32.

**Opening and saving are in the menu bar**, not buttons in the window. So is everything else a Mac app keeps up there — and the Edit menu earns its keep even here, because without it copy and paste stop working in the one text field the app has.

## Stage 3 - Sampler instrument

Small stage. Two hours of work, big payoff.

Take one sample and pitch it across the keyboard. Play it faster to go up, slower to go down. That's it. No synthesis, no oscillators, no envelopes beyond a short attack and release to stop clicks.

Why this earns its place:

- It makes stage 4 immediately useful, because you have something to write melodies for
- Sampled instruments cover a huge amount of beat making already
- It proves out pitch, note-on and note-off before plugins arrive

Sits in the pattern grid as a track row, same as a sample. In step mode it plays one fixed note, so a bass line can be a rhythm part.

## Stage 4 - Piano roll

**What it does**

- Right click a track, choose "piano roll"
- Keyboard down the left, bars across the top
- Draw notes, drag to move, drag the edge to resize
- Velocity as a bar under each note
- Same pattern container as the step grid, just a different editor

**Audio side**

Real polyphony and note-off handling. Each note takes a voice at its start and releases it at its end. Voice stealing needs a few milliseconds of fade or you get clicks.

**Rendering**

This is where the webview gets tested. Hundreds of notes at 60fps means canvas, not one DOM node per note. Only redraw when something changes or the playhead moves.

## Stage 5 - Effects

You asked for song, instrument and pattern level. Two of those are normal. Pattern level gets weird when the same pattern plays twice in a song.

Suggested model:

- **Track level** - a chain on each track. Covers the "instrument" case.
- **Master level** - a chain on the whole output. Covers the "song" case.
- **Pattern level** - skip it. To make a pattern sound different, duplicate it and change the track effects.

**Starter effects**

Filter, delay, reverb, distortion, compressor. Five is plenty.

**Constraints**

Effects run on the audio thread, so same rules apply. Pre-allocate delay buffers at maximum size. Parameter changes must never resize anything, and need smoothing over a few milliseconds or you get zipper noise.

## Stage 6 - CLAP plugin hosting

The payoff stage. Surge XT and friends inside your app.

**Scope it tightly**

- Instruments only at first, effects plugins later
- Each plugin opens in its own floating window, not embedded in your UI
- Run plugins in a separate child process, so a crash can't kill your app
- Scan a plugins folder plus the standard `~/Library/Audio/Plug-Ins/CLAP` path

**What the work actually is**

`clack-host` handles the API. The fiddly parts are: creating a native macOS window for the plugin's UI, keeping parameters in sync, saving plugin state into your project folder, and the process boundary. Bounded, but not a weekend.

**Project files**

Plugin state is an opaque blob the plugin gives you. Store it base64 in `project.json`, or as a separate file in the project folder. Note which plugin and version made it, so you can warn instead of crash when it's missing.

## Shipping it

**Bundling**

`cargo tauri build` gives you a `.app`. Build a universal binary so it runs on both Apple Silicon and Intel.

**The Gatekeeper warning**

Unsigned apps get blocked. Put this in the README, first line:

> First time you open it: right click the app, choose Open, then click Open again.

Expect a chunk of your support questions to be about this. Signing costs $99 a year through the Apple Developer Program, worth doing if the app takes off.

## Risks, in order of how much they'll hurt

1. **Audio thread discipline.** One stray allocation and you'll chase clicks for days. Set the rules now.
2. **Piano roll rendering.** The one place the webview choice could bite. Plan for canvas early.
3. **The notes vs steps data model.** Get it right in stage 1 or pay for it in stage 4.
4. **Stage 6 creeping forward.** Plugins are the exciting bit. Building them early means shipping nothing.

## Open questions

- Keyboard shortcuts: a full set, or just space and delete?
- Undo: needed from stage 2, or can it wait?
- MIDI keyboard input for the piano roll?
- Does the sample browser need folders and search, or is a flat list fine?
