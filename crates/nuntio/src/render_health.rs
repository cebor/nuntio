//! Whether the renderer keeps up: what to do about skipped and lost frames,
//! how long to hold frames while the GPU is busy or stuck, and when to look
//! for a GPU again after a lost device or a stall left the window on the CPU.

use std::time::{Duration, Instant};

use nuntio_render::FrameStatus;

/// Immediate retries of a skipped frame before waiting for the next event.
const MAX_FRAME_RETRIES: u32 = 3;
/// Lost-surface frames in a row that still rebuild the renderer.
const MAX_RENDERER_REBUILDS: u32 = 3;
/// How often to look whether the GPU is back.
const GPU_PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Longest wait between switch attempts that ended on the CPU again.
const GPU_PROBE_MAX_INTERVAL: Duration = Duration::from_secs(60);
/// Delay before drawing again while the GPU is busy; doubles per frame up to the max.
const BUSY_DELAY: Duration = Duration::from_millis(4);
const BUSY_MAX_DELAY: Duration = Duration::from_millis(64);
/// Delay after a frame timed out; doubles per frame up to the max.
const TIMEOUT_DELAY: Duration = Duration::from_millis(250);
const TIMEOUT_MAX_DELAY: Duration = Duration::from_secs(2);
/// Time without a frame (busy or timed out) after which the GPU is left for the CPU.
pub const STALL_LIMIT: Duration = Duration::from_secs(3);
/// First look-out for the GPU after leaving it for a stall.
const STALL_PROBE_INTERVAL: Duration = Duration::from_secs(30);
/// Longest look-out after repeated stalls.
const STALL_PROBE_MAX_INTERVAL: Duration = Duration::from_secs(600);

/// What the window does about the frame it just drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthAction {
    /// Draw again right away.
    Retry,
    /// Stop retrying; the next event redraws anyway.
    GiveUpFrame,
    /// Build a new renderer.
    Rebuild,
    /// Tell the user once that rebuilding doesn't help.
    ShowGpuError,
    /// Nothing to do: the frame is up, the window isn't drawable, or the
    /// renderer was given up on until something retries it.
    Nothing,
    /// Hold frames until `next_wakeup`; `run_timers` asks for the redraw then.
    Defer,
    /// No frame for `STALL_LIMIT`: leave the GPU for the CPU.
    LeaveGpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuProbe {
    pub next: Instant,
    pub interval: Duration,
}

impl GpuProbe {
    pub fn start(now: Instant) -> Self {
        Self {
            next: now + GPU_PROBE_INTERVAL,
            interval: GPU_PROBE_INTERVAL,
        }
    }

    /// No GPU yet: look again after the same interval.
    pub fn again(self, now: Instant) -> Self {
        Self {
            next: now + self.interval,
            ..self
        }
    }

    /// A GPU was offered but the new renderer ended on the CPU anyway:
    /// wait twice as long (at most `GPU_PROBE_MAX_INTERVAL`, never shorter
    /// than the interval it had), so a GPU that can't be used doesn't cost
    /// a rebuild every few seconds.
    pub fn back_off(self, now: Instant) -> Self {
        let interval = (self.interval * 2).min(GPU_PROBE_MAX_INTERVAL.max(self.interval));
        Self {
            next: now + interval,
            interval,
        }
    }
}

/// Skipped, lost and held frames of one window, and the look-out for a GPU
/// while a lost device or a stall left it on a CPU adapter it didn't ask for.
#[derive(Debug, Default)]
pub struct RenderHealth {
    /// Frames skipped in a row, to stop retrying a stuck surface.
    skipped: u32,
    /// Frames in a row that reported a lost surface or device.
    lost: u32,
    /// Set while the window is on the CPU with `window.gpu_acceleration`
    /// on: when to look for the GPU again.
    probe: Option<GpuProbe>,
    /// Busy or timed-out frames in a row, for the delay before the next.
    deferred: u32,
    /// When a held frame is drawn; frames before that are held.
    redraw_at: Option<Instant>,
    /// The first busy or timed-out frame since the last one that wasn't.
    stalled_since: Option<Instant>,
    /// Interval and time of the last switch to the CPU for a stall.
    last_stall: Option<(Duration, Instant)>,
}

impl RenderHealth {
    /// Record how a frame went and say what to do about it.
    pub fn on_frame(&mut self, status: FrameStatus, now: Instant) -> HealthAction {
        if status == FrameStatus::Skipped {
            self.skipped += 1;
        } else {
            self.skipped = 0;
        }
        if status == FrameStatus::Presented {
            self.lost = 0;
        }
        if !matches!(status, FrameStatus::Busy | FrameStatus::TimedOut) {
            self.deferred = 0;
            self.redraw_at = None;
            // A reconfigured surface inside a stall keeps the stall clock running.
            if status != FrameStatus::Skipped {
                self.stalled_since = None;
            }
        }
        match status {
            FrameStatus::Presented | FrameStatus::Paused => HealthAction::Nothing,
            // A surface that keeps needing a new configuration must not spin.
            FrameStatus::Skipped if self.skipped <= MAX_FRAME_RETRIES => HealthAction::Retry,
            FrameStatus::Skipped => HealthAction::GiveUpFrame,
            FrameStatus::Busy | FrameStatus::TimedOut => {
                let since = *self.stalled_since.get_or_insert(now);
                self.deferred += 1;
                self.redraw_at = Some(now + defer_delay(status, self.deferred));
                // The hold stays: if the window can't switch, the frame is
                // still drawn at `redraw_at`, and the next `LeaveGpu` comes
                // only after another `STALL_LIMIT`.
                if now.duration_since(since) >= STALL_LIMIT {
                    self.stalled_since = None;
                    return HealthAction::LeaveGpu;
                }
                HealthAction::Defer
            }
            FrameStatus::Lost => {
                self.lost += 1;
                match self.lost {
                    ..=MAX_RENDERER_REBUILDS => HealthAction::Rebuild,
                    n if n == MAX_RENDERER_REBUILDS + 1 => HealthAction::ShowGpuError,
                    // Resizing, focusing or showing the window tries again.
                    _ => HealthAction::Nothing,
                }
            }
        }
    }

    /// A busy or stuck GPU holds frames until their time: drawing now would block on it.
    pub fn holds(&self, now: Instant) -> bool {
        self.redraw_at.is_some_and(|at| now < at)
    }

    /// The held frame is due: drop the hold and say so, once.
    pub fn redraw_due(&mut self, now: Instant) -> bool {
        let due = self.redraw_at.is_some_and(|at| now >= at);
        if due {
            self.redraw_at = None;
        }
        due
    }

    /// A new renderer replaced the stalled one: draw it at once.
    pub fn release_hold(&mut self) {
        self.redraw_at = None;
        self.deferred = 0;
    }

    /// The look-out for the GPU after leaving it for a stall: 30 s at first,
    /// twice as long (at most 10 min) if it stalled again within one interval
    /// of coming back.
    pub fn stall_probe(&mut self, now: Instant) -> GpuProbe {
        let interval = match self.last_stall {
            Some((interval, at)) if now < at + interval * 2 => {
                (interval * 2).min(STALL_PROBE_MAX_INTERVAL)
            }
            _ => STALL_PROBE_INTERVAL,
        };
        self.last_stall = Some((interval, now));
        GpuProbe {
            next: now + interval,
            interval,
        }
    }

    /// After the renderer was given up on, try drawing again: something
    /// changed (resize, focus, the window was shown). Returns whether to
    /// redraw.
    pub fn retry(&mut self) -> bool {
        let given_up = self.lost > MAX_RENDERER_REBUILDS;
        if given_up {
            self.lost = 0;
        }
        given_up
    }

    /// The look-out for a GPU, once it is due.
    pub fn probe_due(&self, now: Instant) -> Option<GpuProbe> {
        self.probe.filter(|probe| now >= probe.next)
    }

    pub fn set_probe(&mut self, probe: Option<GpuProbe>) {
        self.probe = probe;
    }

    /// When the window next has to wake up for the look-out or a held frame.
    pub fn next_wakeup(&self) -> Option<Instant> {
        [self.probe.map(|probe| probe.next), self.redraw_at]
            .into_iter()
            .flatten()
            .min()
    }
}

/// How long to hold the frame after `deferred` busy or timed-out frames in a row.
fn defer_delay(status: FrameStatus, deferred: u32) -> Duration {
    let (first, max) = if status == FrameStatus::TimedOut {
        (TIMEOUT_DELAY, TIMEOUT_MAX_DELAY)
    } else {
        (BUSY_DELAY, BUSY_MAX_DELAY)
    };
    first
        .saturating_mul(1 << deferred.saturating_sub(1).min(16))
        .min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_frames_retry_a_few_times_then_give_up() {
        let now = Instant::now();
        let mut health = RenderHealth::default();
        for _ in 0..MAX_FRAME_RETRIES {
            assert_eq!(
                health.on_frame(FrameStatus::Skipped, now),
                HealthAction::Retry
            );
        }
        assert_eq!(
            health.on_frame(FrameStatus::Skipped, now),
            HealthAction::GiveUpFrame
        );
        // Any other outcome starts the count over.
        assert_eq!(
            health.on_frame(FrameStatus::Paused, now),
            HealthAction::Nothing
        );
        assert_eq!(
            health.on_frame(FrameStatus::Skipped, now),
            HealthAction::Retry
        );
    }

    #[test]
    fn lost_frames_rebuild_a_few_times_then_give_up() {
        let now = Instant::now();
        let mut health = RenderHealth::default();
        for _ in 0..MAX_RENDERER_REBUILDS {
            assert_eq!(
                health.on_frame(FrameStatus::Lost, now),
                HealthAction::Rebuild
            );
        }
        assert_eq!(
            health.on_frame(FrameStatus::Lost, now),
            HealthAction::ShowGpuError
        );
        assert_eq!(
            health.on_frame(FrameStatus::Lost, now),
            HealthAction::Nothing
        );
        assert_eq!(
            health.on_frame(FrameStatus::Lost, now),
            HealthAction::Nothing
        );
    }

    #[test]
    fn retry_rearms_a_given_up_renderer_and_a_presented_frame_resets_the_count() {
        let now = Instant::now();
        let mut health = RenderHealth::default();
        assert!(!health.retry());
        for _ in 0..=MAX_RENDERER_REBUILDS {
            health.on_frame(FrameStatus::Lost, now);
        }
        assert!(health.retry());
        assert!(!health.retry());
        assert_eq!(
            health.on_frame(FrameStatus::Lost, now),
            HealthAction::Rebuild
        );

        health.on_frame(FrameStatus::Lost, now);
        health.on_frame(FrameStatus::Presented, now);
        for _ in 0..MAX_RENDERER_REBUILDS {
            assert_eq!(
                health.on_frame(FrameStatus::Lost, now),
                HealthAction::Rebuild
            );
        }
    }

    #[test]
    fn busy_frames_are_held_with_growing_delays() {
        let now = Instant::now();
        let mut health = RenderHealth::default();
        for ms in [4, 8, 16, 32, 64, 64] {
            assert_eq!(health.on_frame(FrameStatus::Busy, now), HealthAction::Defer);
            assert_eq!(health.next_wakeup(), Some(now + Duration::from_millis(ms)));
        }
        assert!(health.holds(now));
        let due = now + Duration::from_millis(64);
        assert!(!health.holds(due));
        assert!(health.redraw_due(due));
        assert!(!health.redraw_due(due));

        health.on_frame(FrameStatus::Busy, now);
        assert_eq!(
            health.on_frame(FrameStatus::Presented, now),
            HealthAction::Nothing
        );
        assert!(!health.holds(now));
        assert_eq!(health.next_wakeup(), None);
        // The delay starts over too.
        health.on_frame(FrameStatus::Busy, now);
        assert_eq!(health.next_wakeup(), Some(now + BUSY_DELAY));
    }

    #[test]
    fn timed_out_frames_back_off_up_to_two_seconds() {
        let start = Instant::now();
        let mut health = RenderHealth::default();
        for (i, ms) in [250, 500, 1000, 2000, 2000].into_iter().enumerate() {
            // Stay below the stall limit: only the delays are checked here.
            let now = start + Duration::from_millis(100 * i as u64);
            assert_eq!(
                health.on_frame(FrameStatus::TimedOut, now),
                HealthAction::Defer
            );
            assert_eq!(health.next_wakeup(), Some(now + Duration::from_millis(ms)));
        }
    }

    #[test]
    fn a_stall_leaves_the_gpu_once_per_limit() {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        let mut health = RenderHealth::default();
        assert_eq!(health.on_frame(FrameStatus::Busy, now), HealthAction::Defer);
        assert_eq!(
            health.on_frame(FrameStatus::Busy, at(2900)),
            HealthAction::Defer
        );
        assert_eq!(
            health.on_frame(FrameStatus::TimedOut, at(3000)),
            HealthAction::LeaveGpu
        );
        // The frame stays held, in case the window can't switch.
        assert!(health.holds(at(3000)));
        assert_eq!(
            health.on_frame(FrameStatus::Busy, at(3100)),
            HealthAction::Defer
        );
        assert_eq!(
            health.on_frame(FrameStatus::Busy, at(6000)),
            HealthAction::Defer
        );
        assert_eq!(
            health.on_frame(FrameStatus::Busy, at(6100)),
            HealthAction::LeaveGpu
        );
    }

    #[test]
    fn a_presented_frame_or_occlusion_ends_the_stall() {
        let now = Instant::now();
        let at = |ms: u64| now + Duration::from_millis(ms);
        for (between, after) in [
            (FrameStatus::Presented, HealthAction::Defer),
            (FrameStatus::Paused, HealthAction::Defer),
            // A reconfigured surface isn't a frame: the stall goes on.
            (FrameStatus::Skipped, HealthAction::LeaveGpu),
        ] {
            let mut health = RenderHealth::default();
            health.on_frame(FrameStatus::Busy, now);
            health.on_frame(between, at(1000));
            assert_eq!(
                health.on_frame(FrameStatus::Busy, at(3500)),
                after,
                "{between:?}"
            );
        }
    }

    #[test]
    fn stall_probes_double_while_the_gpu_keeps_stalling() {
        let now = Instant::now();
        let at = |secs: u64| now + Duration::from_secs(secs);
        let mut health = RenderHealth::default();
        let probe = health.stall_probe(now);
        assert_eq!(probe.interval, Duration::from_secs(30));
        assert_eq!(probe.next, at(30));
        // Each stall comes within twice the last interval of the one before.
        for (secs, interval) in [
            (50, 60),
            (150, 120),
            (350, 240),
            (750, 480),
            (1650, 600),
            (2400, 600),
        ] {
            let probe = health.stall_probe(at(secs));
            assert_eq!(probe.interval, Duration::from_secs(interval));
            assert_eq!(probe.next, at(secs + interval));
        }
        // Twice the interval without a stall: start over.
        assert_eq!(
            health.stall_probe(at(2400 + 1200)).interval,
            STALL_PROBE_INTERVAL
        );
        // A probe that ends on the CPU again doesn't shrink a long interval.
        let long = GpuProbe {
            next: now,
            interval: STALL_PROBE_MAX_INTERVAL,
        };
        assert_eq!(long.back_off(now).interval, STALL_PROBE_MAX_INTERVAL);
    }

    #[test]
    fn gpu_probe_backs_off_up_to_a_minute() {
        let now = Instant::now();
        let probe = GpuProbe::start(now);
        assert_eq!(probe.interval, Duration::from_secs(5));
        assert_eq!(probe.next, now + Duration::from_secs(5));
        let later = now + Duration::from_secs(7);
        let again = probe.again(later);
        assert_eq!(again.interval, Duration::from_secs(5));
        assert_eq!(again.next, later + Duration::from_secs(5));
        let mut probe = again;
        for secs in [10, 20, 40, 60, 60] {
            probe = probe.back_off(now);
            assert_eq!(probe.interval, Duration::from_secs(secs));
            assert_eq!(probe.next, now + probe.interval);
        }
    }

    #[test]
    fn the_probe_is_due_once_its_time_has_come() {
        let now = Instant::now();
        let mut health = RenderHealth::default();
        assert_eq!(health.next_wakeup(), None);
        assert_eq!(health.probe_due(now), None);
        health.set_probe(Some(GpuProbe::start(now)));
        assert_eq!(health.next_wakeup(), Some(now + GPU_PROBE_INTERVAL));
        assert_eq!(health.probe_due(now), None);
        assert!(health.probe_due(now + GPU_PROBE_INTERVAL).is_some());
    }
}
