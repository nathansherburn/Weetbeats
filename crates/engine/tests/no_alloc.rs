//! Risk number one on the build plan: one stray allocation on the audio thread and you
//! spend days chasing clicks. So count them.
//!
//! A global allocator wraps the system one and, while armed, counts every allocation on
//! this thread. `Engine::render` is then driven hard — notes triggering, voices stealing,
//! samples being swapped out, commands arriving — and the count has to come back zero.
//!
//! This file holds exactly one test on purpose. The counter is per thread and armed with a
//! global flag, and one test per binary means nothing else can be running while it is.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use weetbeats_engine::command::{TrashBin, COMMAND_CAPACITY, TRASH_CAPACITY};
use weetbeats_engine::{Command, Engine, EngineNote, Sample, Shared};

static ARMED: AtomicBool = AtomicBool::new(false);

thread_local! {
    // `const` init and no destructor, so touching this cannot itself allocate.
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Run a closure with allocation counting on, and report how many there were.
fn count_allocations(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    ARMED.store(true, Ordering::SeqCst);
    f();
    ARMED.store(false, Ordering::SeqCst);
    COUNT.with(|c| c.get())
}

#[test]
fn render_does_not_allocate() {
    const RATE: u32 = 48_000;

    let shared = Arc::new(Shared::new());
    let (mut tx, rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (trash_tx, mut trash_rx) = rtrb::RingBuffer::new(TRASH_CAPACITY);
    let mut engine = Engine::new(
        RATE,
        128.0,
        16,
        Arc::clone(&shared),
        rx,
        TrashBin::new(trash_tx, Arc::clone(&shared)),
    );

    // Everything the audio thread will touch is built up front, off the audio thread.
    let samples: Vec<Arc<Sample>> = (0..8)
        .map(|i| {
            let frames = 4_000 + i * 1_000;
            let data: Vec<f32> = (0..frames)
                .map(|n| ((n as f32) * 0.01).sin() * 0.5)
                .collect();
            Arc::new(Sample::from_data("tone", RATE, 1, data))
        })
        .collect();

    for id in 0..16u16 {
        tx.push(Command::AddTrack {
            track: id,
            gain: 0.5,
        })
        .unwrap();
        tx.push(Command::SetTrackSample {
            track: id,
            sample: Some(Arc::clone(&samples[(id % 8) as usize])),
        })
        .unwrap();
        // Notes in four patterns, so switching between them and walking the song both
        // happen inside the armed window below.
        for pattern in 0..4u16 {
            for step in 0..8u16 {
                tx.push(Command::SetNote {
                    pattern,
                    track: id,
                    note: EngineNote {
                        step,
                        pitch: 48 + (step as u8) + (pattern as u8),
                        velocity: 100,
                        length: 1,
                    },
                })
                .unwrap();
            }
        }
    }

    // A short song, so the engine crosses a slot boundary every few blocks. Moving on to
    // the next pattern is the one place the audio thread reaches for something it was not
    // already holding, so it is the one most worth counting.
    for pattern in 0..4u16 {
        tx.push(Command::SetPatternSteps { pattern, steps: 8 })
            .unwrap();
    }
    // Two patterns starting at every bar line, so overlapping placements are counted too.
    tx.push(Command::ClearSong).unwrap();
    for bar in 0..4u32 {
        for pattern in [bar as u16, (bar as u16 + 1) % 4] {
            tx.push(Command::PlacePattern {
                pattern,
                step: bar * 8,
            })
            .unwrap();
        }
    }
    tx.push(Command::SetSongLen(32)).unwrap();
    tx.push(Command::SetSongMode(true)).unwrap();
    tx.push(Command::SetPlaying(true)).unwrap();

    // Preallocated output, exactly as the real callback gets from the device.
    let mut out = vec![0.0f32; 512 * 2];

    // Warm up outside the armed window: the first render also drains the setup commands.
    engine.render(&mut out, 2);

    let mut returned = 0usize;
    let allocations = count_allocations(|| {
        for block in 0..400 {
            // Keep the queue busy the way a user leaning on the UI would.
            let id = (block % 16) as u16;
            let _ = tx.push(Command::SetTrackGain {
                track: id,
                gain: 0.3 + (block % 5) as f32 * 0.1,
            });
            let _ = tx.push(Command::SetTrackMuted {
                track: id,
                muted: block % 7 == 0,
            });
            let _ = tx.push(Command::SetBpm(90.0 + (block % 60) as f32));
            // Swapping a sample makes the engine hand the old one back through the bin.
            let _ = tx.push(Command::SetTrackSample {
                track: id,
                sample: Some(Arc::clone(&samples[block % 8])),
            });
            let _ = tx.push(Command::Preview {
                sample: Arc::clone(&samples[block % 8]),
                gain: 0.4,
            });
            // And leaning on the parts that are new: the song, and switching between
            // patterns and the song the way opening and closing the editor does.
            let _ = tx.push(Command::PlacePattern {
                pattern: (block % 4) as u16,
                step: (block % 32) as u32,
            });
            let _ = tx.push(Command::UnplacePattern {
                pattern: ((block + 1) % 4) as u16,
                step: (block % 32) as u32,
            });
            let _ = tx.push(Command::SetPatternSteps {
                pattern: (block % 4) as u16,
                steps: 4 + (block % 12) as u32,
            });
            if block % 37 == 0 {
                let _ = tx.push(Command::SetActivePattern((block % 4) as u16));
                let _ = tx.push(Command::SetSongMode(block % 74 == 0));
                let _ = tx.push(Command::SeekSong((block % 32) as u32));
            }

            engine.render(&mut out, 2);

            // The app thread drains the bin as it goes. Popping is a memcpy and the
            // samples are still owned by `samples`, so dropping them here frees nothing —
            // which is the whole point of handing them back.
            while trash_rx.pop().is_ok() {
                returned += 1;
            }
        }
    });

    assert_eq!(
        allocations, 0,
        "the audio thread allocated {allocations} times — that is a dropout waiting to happen"
    );

    // And it really did the work: voices sounded, and samples came back to be dropped on
    // the app thread rather than being freed on the audio thread.
    assert!(shared.playhead().active_voices > 0, "nothing was playing");
    assert!(returned > 0, "no samples were handed back");
    assert_eq!(
        shared.dropped_on_audio_thread(),
        0,
        "the trash queue overflowed, so samples were dropped on the audio thread"
    );
}
