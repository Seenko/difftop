//! The animation: lines queue up and are released onto the screen at a
//! steady pace, so changes rise instead of appearing all at once.
//!
//! It also owns scrolling: the view is either *following* (pinned to the
//! newest line) or held at a fixed place while new lines keep arriving below.
//!
//! This module knows nothing about terminals; it is generic over the line
//! type so it can be tested with plain numbers.

use std::collections::VecDeque;
use std::time::Duration;

/// Lines kept after they scrolled off the top, so there is history to scroll
/// back through.
const MAX_SHOWN: usize = 10_000;
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
    /// How many lines sit below the view; 0 means following the newest line.
    offset: usize,
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
            offset: 0,
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
            // Keep a scrolled view still while lines arrive below it.
            if self.offset > 0 {
                self.offset += 1;
            }
        }
        while self.shown.len() > MAX_SHOWN {
            self.shown.pop_front();
        }
        self.offset = self.offset.min(self.shown.len());
    }

    /// The `height` lines in view, top of the screen first.
    pub fn view(&self, height: usize) -> impl Iterator<Item = &T> {
        let end = self.shown.len() - self.offset;
        self.shown.range(end.saturating_sub(height)..end)
    }

    /// Scrolls towards older lines (positive `lines`) or newer ones
    /// (negative), for a view `height` lines tall. Scrolling back down to the
    /// newest line resumes following.
    pub fn scroll(&mut self, lines: isize, height: usize) {
        let top = self.shown.len().saturating_sub(height);
        self.offset = self.offset.saturating_add_signed(lines).min(top);
    }

    pub fn follow(&mut self) {
        self.offset = 0;
    }

    /// Lines newer than the view; 0 while following.
    pub fn below(&self) -> usize {
        self.offset
    }

    pub fn queued(&self) -> usize {
        self.pending.len()
    }

    pub fn clear(&mut self) {
        self.shown.clear();
        self.offset = 0;
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
        assert_eq!(feed.view(3).copied().collect::<Vec<_>>(), [17, 18, 19]);
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
        assert_eq!(feed.view(10).count(), 0);
    }

    #[test]
    fn overflow_drops_oldest_and_counts() {
        let mut feed = Feed::new(10.0);
        feed.push(0..MAX_PENDING + 7);
        assert_eq!(feed.queued(), MAX_PENDING);
        assert_eq!(feed.dropped, 7);
        feed.tick(SECOND * 60);
        assert_eq!(feed.view(1).next(), Some(&(MAX_PENDING + 6)));
        assert_eq!(feed.view(usize::MAX).count(), MAX_PENDING);
    }

    fn view(feed: &Feed<usize>, height: usize) -> Vec<usize> {
        feed.view(height).copied().collect()
    }

    #[test]
    fn scrolled_view_holds_still_while_lines_arrive() {
        let mut feed = Feed::new(10.0);
        feed.push(0..10);
        feed.tick(SECOND);
        feed.scroll(2, 3);
        assert_eq!(view(&feed, 3), [5, 6, 7]);
        assert_eq!(feed.below(), 2);

        feed.push(10..14);
        feed.tick(SECOND);
        assert_eq!(view(&feed, 3), [5, 6, 7]);
        assert_eq!(feed.below(), 6);

        feed.follow();
        assert_eq!(view(&feed, 3), [11, 12, 13]);
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut feed = Feed::new(10.0);
        feed.push(0..10);
        feed.tick(SECOND);
        feed.scroll(100, 3);
        assert_eq!(view(&feed, 3), [0, 1, 2]);
        feed.scroll(-100, 3);
        assert_eq!(feed.below(), 0);
        assert_eq!(view(&feed, 3), [7, 8, 9]);

        // Fewer lines than the screen: nothing to scroll.
        let mut short = Feed::new(10.0);
        short.push(0..2);
        short.tick(SECOND);
        short.scroll(5, 3);
        assert_eq!(short.below(), 0);
    }
}
