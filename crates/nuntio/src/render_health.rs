//! Whether the renderer keeps up: what to do about skipped and lost frames,
//! and when to look for a GPU again after a lost device left the window on
//! the CPU.

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
    /// wait twice as long (at most `GPU_PROBE_MAX_INTERVAL`), so a GPU
    /// that can't be used doesn't cost a rebuild every few seconds.
    pub fn back_off(self, now: Instant) -> Self {
        let interval = (self.interval * 2).min(GPU_PROBE_MAX_INTERVAL);
        Self {
            next: now + interval,
            interval,
        }
    }
}

/// Skipped and lost frames of one window, and the look-out for a GPU while
/// a lost device left it on a CPU adapter it didn't ask for.
#[derive(Debug, Default)]
pub struct RenderHealth {
    /// Frames skipped in a row, to stop retrying a stuck surface.
    skipped: u32,
    /// Frames in a row that reported a lost surface or device.
    lost: u32,
    /// Set while the window is on the CPU with `window.gpu_acceleration`
    /// on: when to look for the GPU again.
    probe: Option<GpuProbe>,
}

impl RenderHealth {
    /// Record how a frame went and say what to do about it.
    pub fn on_frame(&mut self, status: FrameStatus) -> HealthAction {
        if status == FrameStatus::Skipped {
            self.skipped += 1;
        } else {
            self.skipped = 0;
        }
        if status == FrameStatus::Presented {
            self.lost = 0;
        }
        match status {
            FrameStatus::Presented | FrameStatus::Paused => HealthAction::Nothing,
            // A surface that keeps timing out must not spin.
            FrameStatus::Skipped if self.skipped <= MAX_FRAME_RETRIES => HealthAction::Retry,
            FrameStatus::Skipped => HealthAction::GiveUpFrame,
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

    /// When the window next has to wake up for the look-out.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.probe.map(|probe| probe.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_frames_retry_a_few_times_then_give_up() {
        let mut health = RenderHealth::default();
        for _ in 0..MAX_FRAME_RETRIES {
            assert_eq!(health.on_frame(FrameStatus::Skipped), HealthAction::Retry);
        }
        assert_eq!(
            health.on_frame(FrameStatus::Skipped),
            HealthAction::GiveUpFrame
        );
        // Any other outcome starts the count over.
        assert_eq!(health.on_frame(FrameStatus::Paused), HealthAction::Nothing);
        assert_eq!(health.on_frame(FrameStatus::Skipped), HealthAction::Retry);
    }

    #[test]
    fn lost_frames_rebuild_a_few_times_then_give_up() {
        let mut health = RenderHealth::default();
        for _ in 0..MAX_RENDERER_REBUILDS {
            assert_eq!(health.on_frame(FrameStatus::Lost), HealthAction::Rebuild);
        }
        assert_eq!(
            health.on_frame(FrameStatus::Lost),
            HealthAction::ShowGpuError
        );
        assert_eq!(health.on_frame(FrameStatus::Lost), HealthAction::Nothing);
        assert_eq!(health.on_frame(FrameStatus::Lost), HealthAction::Nothing);
    }

    #[test]
    fn retry_rearms_a_given_up_renderer_and_a_presented_frame_resets_the_count() {
        let mut health = RenderHealth::default();
        assert!(!health.retry());
        for _ in 0..=MAX_RENDERER_REBUILDS {
            health.on_frame(FrameStatus::Lost);
        }
        assert!(health.retry());
        assert!(!health.retry());
        assert_eq!(health.on_frame(FrameStatus::Lost), HealthAction::Rebuild);

        health.on_frame(FrameStatus::Lost);
        health.on_frame(FrameStatus::Presented);
        for _ in 0..MAX_RENDERER_REBUILDS {
            assert_eq!(health.on_frame(FrameStatus::Lost), HealthAction::Rebuild);
        }
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
