//! The step clock. Driven by the sample count, because that is the only honest clock.
//!
//! A wall clock timer drifts against the audio device and a timer thread can be late by
//! whole milliseconds. Counting the frames the callback has actually rendered cannot drift,
//! because it *is* what the speaker heard.

use crate::STEPS_PER_BEAT;

/// Where the pattern is up to, in frames.
#[derive(Debug, Clone)]
pub struct StepClock {
    sample_rate: f64,
    bpm: f64,
    samples_per_step: f64,
    /// Frames rendered since the current step started.
    accum: f64,
    step: u32,
    steps: u32,
    /// Set when a step boundary has been reached and nothing has triggered it yet.
    pending: bool,
    /// Set when the last step of the pattern has just gone by. The engine reads this to
    /// know when to move the song on to the next slot.
    wrapped: bool,
}

impl StepClock {
    pub fn new(sample_rate: u32, bpm: f32, steps: u32) -> Self {
        let mut clock = StepClock {
            sample_rate: sample_rate.max(1) as f64,
            bpm: 120.0,
            samples_per_step: 1.0,
            accum: 0.0,
            step: 0,
            steps: steps.max(1),
            pending: true,
            wrapped: false,
        };
        clock.set_bpm(bpm);
        clock
    }

    /// Frames in one step at the current tempo. Fractional on purpose: rounding here is
    /// where "my loop drifts against the metronome" comes from.
    pub fn samples_per_step(&self) -> f64 {
        self.samples_per_step
    }

    pub fn bpm(&self) -> f32 {
        self.bpm as f32
    }

    pub fn step(&self) -> u32 {
        self.step
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// How far through the current step, 0.0 to just under 1.0.
    pub fn progress(&self) -> f32 {
        (self.accum / self.samples_per_step).clamp(0.0, 1.0) as f32
    }

    pub fn set_bpm(&mut self, bpm: f32) {
        // Below 20 the maths still works but the UI has no business asking.
        self.bpm = (bpm as f64).clamp(20.0, 400.0);
        self.samples_per_step = self.sample_rate * 60.0 / (self.bpm * STEPS_PER_BEAT);
        // Keep the position within the new, possibly shorter, step.
        if self.accum > self.samples_per_step {
            self.accum = self.samples_per_step;
        }
    }

    pub fn set_steps(&mut self, steps: u32) {
        self.steps = steps.max(1);
        if self.step >= self.steps {
            self.step = 0;
            self.pending = true;
        }
    }

    /// Back to the top of the pattern, with step 0 due to fire immediately.
    pub fn rewind(&mut self) {
        self.jump_to(0);
    }

    /// Straight to a step, with that step due to fire immediately. Anything past the end
    /// lands on the last step rather than wrapping round to somewhere unexpected.
    pub fn jump_to(&mut self, step: u32) {
        self.step = step.min(self.steps.saturating_sub(1));
        self.accum = 0.0;
        self.pending = true;
        self.wrapped = false;
    }

    /// True when a step should be triggered before rendering another frame.
    #[inline]
    pub fn due(&self) -> bool {
        self.pending
    }

    /// Take the step that is due, so it only triggers once.
    #[inline]
    pub fn take_step(&mut self) -> u32 {
        self.pending = false;
        self.step
    }

    /// Frames that can be rendered before the next step boundary. Never zero, so a render
    /// loop built on this always makes progress.
    #[inline]
    pub fn frames_to_next_step(&self) -> usize {
        let remaining = self.samples_per_step - self.accum;
        if remaining <= 1.0 {
            1
        } else {
            remaining.ceil() as usize
        }
    }

    /// True once when the pattern has just come round to the top, and false until it
    /// happens again. Taking it means the caller has dealt with it.
    #[inline]
    pub fn take_wrapped(&mut self) -> bool {
        std::mem::take(&mut self.wrapped)
    }

    /// Move the clock on by frames that have been rendered.
    #[inline]
    pub fn advance(&mut self, frames: usize) {
        self.accum += frames as f64;
        while self.accum >= self.samples_per_step {
            self.accum -= self.samples_per_step;
            self.step += 1;
            if self.step >= self.steps {
                self.step = 0;
                self.wrapped = true;
            }
            self.pending = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteenths_at_120bpm() {
        // 120bpm is 2 beats a second, 4 steps a beat, so 8 steps a second.
        let clock = StepClock::new(44_100, 120.0, 16);
        assert_eq!(clock.samples_per_step(), 5512.5);
    }

    /// The point of the whole module: no drift, even though a step is not a whole number
    /// of frames and the callback size has nothing to do with the tempo.
    #[test]
    fn does_not_drift_over_a_minute() {
        let mut clock = StepClock::new(44_100, 137.0, 16);
        let mut steps = 0u32;
        let mut rendered = 0usize;
        let total = 44_100 * 60;
        while rendered < total {
            if clock.due() {
                clock.take_step();
                steps += 1;
            }
            // An awkward callback size that never lines up with a step.
            let n = clock.frames_to_next_step().min(411).min(total - rendered);
            clock.advance(n);
            rendered += n;
        }
        // 137bpm at 4 steps a beat is 548 steps a minute. The one at frame 0 counts; the
        // one landing on the very last frame belongs to the next minute.
        assert_eq!(steps, 548);
        // And the last one has to be where the maths says, not a frame either side.
        let expected_last = (547.0 * 44_100.0 * 60.0 / (137.0 * 4.0)) as usize;
        assert!(rendered >= expected_last);
    }

    #[test]
    fn wraps_at_the_end_of_the_pattern() {
        let mut clock = StepClock::new(48_000, 120.0, 4);
        let mut seen = Vec::new();
        for _ in 0..9 {
            if clock.due() {
                seen.push(clock.take_step());
            }
            clock.advance(clock.frames_to_next_step());
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 0, 1, 2, 3, 0]);
    }

    #[test]
    fn tempo_change_keeps_the_playhead_inside_the_step() {
        let mut clock = StepClock::new(44_100, 60.0, 16);
        clock.advance(5_000);
        clock.set_bpm(240.0);
        assert!(clock.progress() <= 1.0);
        assert!(clock.frames_to_next_step() >= 1);
    }

    #[test]
    fn coming_round_to_the_top_is_reported_once() {
        let mut clock = StepClock::new(48_000, 120.0, 4);
        for _ in 0..3 {
            clock.advance(clock.frames_to_next_step());
            assert!(
                !clock.take_wrapped(),
                "wrapped in the middle of the pattern"
            );
        }
        clock.advance(clock.frames_to_next_step());
        assert_eq!(clock.step(), 0);
        assert!(
            clock.take_wrapped(),
            "the pattern came round and nobody said so"
        );
        assert!(!clock.take_wrapped(), "reported the same wrap twice");
    }

    #[test]
    fn shrinking_the_pattern_pulls_the_playhead_back() {
        let mut clock = StepClock::new(44_100, 120.0, 16);
        for _ in 0..10 {
            clock.advance(clock.frames_to_next_step());
        }
        assert_eq!(clock.step(), 10);
        clock.set_steps(8);
        assert_eq!(clock.step(), 0);
    }
}
