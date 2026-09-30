//! The animation: lines queue up and are released onto the screen at a
//! steady pace, so changes rise instead of appearing all at once.
//!
//! This module knows nothing about terminals; it is generic over the line
//! type so it can be tested with plain numbers.

use std::collections::VecDeque;
use std::time::Duration;

/// Lines kept after they scrolled off the top. More than any screen is tall.
const MAX_SHOWN: usize = 1000;
/// Queued lines beyond this are dropped (oldest first) and counted.
const MAX_PENDING: usize = 5000;

pub struct Feed<T> {
    shown: VecDeque<T>,
    pending: VecDeque<T>,
    /// Fractional lines earned but not yet released.
    budget: f64,
    /// Lines per second when the queue is short.
    pub speed: f64,
    pub paused: bool,
    /// Total lines dropped because the queue overflowed.
    pub dropped: usize,
}

impl<T> Feed<T> {
    pub fn new(speed: f64) -> Self {
        Self {
            shown: VecDeque::new(),
            pending: VecDeque::new(),
            budget: 0.0,
            speed,
            paused: false,
            dropped: 0,
        }
    }

    pub fn push(&mut self, lines: impl IntoIterator<Item = T>) {
        self.pending.extend(lines);
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
            self.dropped += 1;
        }
    }

    /// Advances the animation by `elapsed`. Releases `speed` lines per second
    /// plus half the queue per second, so small changes glide in at a
    /// readable pace and big bursts still drain within a few seconds.
    pub fn tick(&mut self, elapsed: Duration) {
        if self.paused || self.pending.is_empty() {
            self.budget = 0.0;
            return;
        }
        let rate = self.speed + self.pending.len() as f64 / 2.0;
        self.budget += rate * elapsed.as_secs_f64();
        while self.budget >= 1.0 {
            let Some(line) = self.pending.pop_front() else {
                break;
            };
            self.shown.push_back(line);
            self.budget -= 1.0;
        }
        while self.shown.len() > MAX_SHOWN {
            self.shown.pop_front();
        }
    }

    /// The newest `n` released lines, oldest first (top of the screen first).
    pub fn newest(&self, n: usize) -> impl Iterator<Item = &T> {
        self.shown.iter().skip(self.shown.len().saturating_sub(n))
    }

    pub fn queued(&self) -> usize {
        self.pending.len()
    }

    pub fn clear(&mut self) {
        self.shown.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn releases_at_speed_plus_half_the_queue() {
        let mut feed = Feed::new(10.0);
        feed.push(0..20);
        // rate = 10 + 20 / 2 = 20 lines in the first second
        feed.tick(SECOND);
        assert_eq!(feed.queued(), 0);
        assert_eq!(feed.newest(3).copied().collect::<Vec<_>>(), [17, 18, 19]);
    }

    #[test]
    fn small_steps_accumulate() {
        let mut feed = Feed::new(10.0);
        feed.push(0..2);
        // rate = 10 + 1 = 11/s; 50 ms earns 0.55 lines, so nothing yet...
        feed.tick(SECOND / 20);
        assert_eq!(feed.queued(), 2);
        // ...and the next 50 ms completes the first line.
        feed.tick(SECOND / 20);
        assert_eq!(feed.queued(), 1);
    }

    #[test]
    fn pause_holds_everything() {
        let mut feed = Feed::new(10.0);
        feed.push(0..5);
        feed.paused = true;
        feed.tick(SECOND * 10);
        assert_eq!(feed.queued(), 5);
        assert_eq!(feed.newest(10).count(), 0);
    }

    #[test]
    fn overflow_drops_oldest_and_counts() {
        let mut feed = Feed::new(10.0);
        feed.push(0..MAX_PENDING + 7);
        assert_eq!(feed.queued(), MAX_PENDING);
        assert_eq!(feed.dropped, 7);
        feed.tick(SECOND * 60);
        assert_eq!(feed.newest(1).next(), Some(&(MAX_PENDING + 6)));
        assert_eq!(feed.newest(usize::MAX).count(), MAX_SHOWN);
    }
}
