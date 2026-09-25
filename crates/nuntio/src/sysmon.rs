//! Samples CPU, memory, network and battery on a background thread for the
//! status bar.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nuntio_config::StatusItem;
use starship_battery::State;
use starship_battery::units::ratio::percent;
use sysinfo::{Networks, System};
use winit::event_loop::EventLoopProxy;

use crate::event::UserEvent;

const INTERVAL: Duration = Duration::from_secs(1);
/// The charge level changes slowly; reading it costs more than the rest.
const BATTERY_INTERVAL: Duration = Duration::from_secs(30);

/// One reading; `None` for sources that aren't sampled or not available.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    /// Usage of all cores, 0–100.
    pub cpu: Option<f32>,
    pub memory: Option<Memory>,
    pub network: Option<Throughput>,
    pub battery: Option<Battery>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Memory {
    pub used: u64,
    pub total: u64,
}

/// Bytes per second over all interfaces except loopback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throughput {
    pub down: f64,
    pub up: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    /// Charge level, 0–100.
    pub level: f32,
    pub charging: bool,
}

/// The sampling thread; it stops when this is dropped.
pub struct SystemMonitor {
    items: Vec<StatusItem>,
    _stop: mpsc::Sender<()>,
}

impl SystemMonitor {
    pub fn start(items: &[StatusItem], proxy: EventLoopProxy<UserEvent>) -> Self {
        let (stop, stopped) = mpsc::channel();
        let wanted = items.to_vec();
        let spawned = thread::Builder::new().name("sysmon".into()).spawn(move || {
            let mut sampler = Sampler::new(&wanted);
            loop {
                let sample = sampler.sample();
                if proxy.send_event(UserEvent::SystemStats(sample)).is_err() {
                    return;
                }
                match stopped.recv_timeout(INTERVAL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
            tracing::debug!("system monitor stopped");
        });
        if let Err(err) = spawned {
            tracing::warn!("cannot start system monitor: {err}");
        }
        Self {
            items: items.to_vec(),
            _stop: stop,
        }
    }

    /// The items it was started for.
    pub fn items(&self) -> &[StatusItem] {
        &self.items
    }
}

struct Sampler {
    system: Option<System>,
    cpu: bool,
    memory: bool,
    networks: Option<(Networks, Instant)>,
    battery: Option<BatterySource>,
}

struct BatterySource {
    manager: starship_battery::Manager,
    last: Option<Battery>,
    next_read: Instant,
}

impl Sampler {
    fn new(items: &[StatusItem]) -> Self {
        let cpu = items.contains(&StatusItem::Cpu);
        let memory = items.contains(&StatusItem::Memory);
        let networks = items
            .contains(&StatusItem::Network)
            .then(|| (Networks::new_with_refreshed_list(), Instant::now()));
        let battery = items
            .contains(&StatusItem::Battery)
            .then(starship_battery::Manager::new)
            .and_then(|manager| {
                manager
                    .inspect_err(|err| tracing::info!("battery unavailable: {err}"))
                    .ok()
            })
            .map(|manager| BatterySource {
                manager,
                last: None,
                next_read: Instant::now(),
            });
        let mut system = (cpu || memory).then(System::new);
        if cpu && let Some(system) = system.as_mut() {
            // CPU usage is the difference to the previous refresh.
            system.refresh_cpu_usage();
        }
        Self {
            system,
            cpu,
            memory,
            networks,
            battery,
        }
    }

    fn sample(&mut self) -> Sample {
        let mut sample = Sample::default();
        if let Some(system) = self.system.as_mut() {
            if self.cpu {
                system.refresh_cpu_usage();
                sample.cpu = Some(system.global_cpu_usage());
            }
            if self.memory {
                system.refresh_memory();
                sample.memory = Some(Memory {
                    used: system.used_memory(),
                    total: system.total_memory(),
                });
            }
        }
        if let Some((networks, last)) = self.networks.as_mut() {
            networks.refresh(true);
            let now = Instant::now();
            let seconds = now.duration_since(*last).as_secs_f64().max(0.001);
            *last = now;
            let (down, up) = networks
                .iter()
                .filter(|(name, _)| !is_loopback(name))
                .fold((0, 0), |(down, up), (_, data)| {
                    (down + data.received(), up + data.transmitted())
                });
            sample.network = Some(Throughput {
                down: down as f64 / seconds,
                up: up as f64 / seconds,
            });
        }
        if let Some(source) = self.battery.as_mut() {
            if Instant::now() >= source.next_read {
                source.last = read_battery(&source.manager);
                source.next_read = Instant::now() + BATTERY_INTERVAL;
            }
            sample.battery = source.last;
        }
        sample
    }
}

/// The first battery, if there is one.
fn read_battery(manager: &starship_battery::Manager) -> Option<Battery> {
    let battery = manager.batteries().ok()?.flatten().next()?;
    Some(Battery {
        level: battery.state_of_charge().get::<percent>().clamp(0.0, 100.0),
        charging: battery.state() == State::Charging,
    })
}

fn is_loopback(name: &str) -> bool {
    name == "lo" || name.starts_with("lo0") || name.contains("Loopback")
}
