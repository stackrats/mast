//! Resource usage (M11): what each running container is actually costing.
//!
//! Two design points carry this module.
//!
//! **Sampling is subscriber-driven.** The loop does nothing while no client is
//! listening. Mast is a laptop tool, and spending CPU to measure CPU when
//! nobody is looking at the answer is self-defeating; the desktop client drops
//! its subscription when the window is hidden, and sampling stops with it.
//!
//! **The delta is ours, not Docker's.** Docker's stats endpoint will compute a
//! CPU delta for you, but only by blocking about a second to collect two
//! cycles. In `one-shot` mode it answers immediately with raw counters, and we
//! subtract against the previous tick. That is cheaper *and* steadier: the
//! delta spans our interval rather than the daemon's fixed one-second window.
//!
//! Nothing here is persisted or redacted (contrast `captures.rs`) — samples
//! are numbers, they carry none of the developer's content, and they are
//! worthless a minute later.

use std::collections::HashMap;
use std::time::Duration;

use mast_contract::{ProjectId, ServiceUsage, UsageSample};
use mast_docker::StatsSample;

use crate::Engine;

/// How often to sample while someone is watching. Fast enough that a spike is
/// visible, slow enough that the measurement is not itself the load.
pub const USAGE_INTERVAL: Duration = Duration::from_secs(2);

/// How long the loop waits before re-checking for subscribers while idle.
const IDLE_POLL: Duration = Duration::from_millis(500);

/// Most containers Mast will stat concurrently in one tick. A twelve-project
/// workspace should not open sixty simultaneous connections to the daemon.
const MAX_CONCURRENT_SAMPLES: usize = 8;

/// One container to sample, resolved from state before any I/O.
struct Target {
    project: ProjectId,
    service: String,
    container_id: String,
}

/// Cores consumed over the interval between two readings.
///
/// `system_cpu_ns` is total CPU time available across all cores, so the ratio
/// of the deltas is the fraction of the whole machine; multiplying by the core
/// count turns that into cores. Returns 0 when the counters did not advance,
/// which is also what a first reading looks like.
pub(crate) fn cores_between(previous: &StatsSample, current: &StatsSample) -> f64 {
    let cpu_delta = current.cpu_total_ns.saturating_sub(previous.cpu_total_ns) as f64;
    let system_delta = current.system_cpu_ns.saturating_sub(previous.system_cpu_ns) as f64;
    if system_delta <= 0.0 || cpu_delta <= 0.0 {
        return 0.0;
    }
    let cores = (cpu_delta / system_delta) * current.online_cpus.max(1) as f64;
    // A counter reset (container restarted under the same id) can produce a
    // nonsense ratio; clamping to the machine keeps one bad tick from
    // rescaling every sparkline in the UI.
    cores.min(current.online_cpus.max(1) as f64)
}

/// Working set: what the container is actually holding, with reclaimable page
/// cache taken off. This is the number `docker stats` shows, and the reason a
/// mysql container does not read as multiple gigabytes.
pub(crate) fn working_set(sample: &StatsSample) -> u64 {
    sample.memory_usage.saturating_sub(sample.memory_cache)
}

impl Engine {
    /// Live resource usage. Subscribing is what starts the sampler; dropping
    /// the last subscription stops it.
    pub fn subscribe_usage(&self) -> futures::stream::BoxStream<'static, UsageSample> {
        use futures::StreamExt;
        let mut rx = self.inner.usage_tx.subscribe();
        let (tx, out_rx) = tokio::sync::mpsc::channel::<UsageSample>(8);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(sample) => {
                        if tx.send(sample).await.is_err() {
                            return;
                        }
                    }
                    // A slow client skips to the newest reading rather than
                    // replaying a backlog of stale numbers.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|sample| (sample, rx))
        })
        .boxed()
    }

    /// Every running container currently in state, with the project and
    /// service it belongs to.
    fn usage_targets(&self) -> Vec<Target> {
        let st = self.inner.state.lock().unwrap();
        st.projects
            .values()
            .flat_map(|entry| {
                let project = ProjectId(entry.record.id.clone());
                entry.summary.services.iter().filter_map(move |service| {
                    if service.state != Some(mast_contract::ContainerState::Running) {
                        return None;
                    }
                    Some(Target {
                        project: project.clone(),
                        service: service.name.clone(),
                        container_id: service.container_id.clone()?,
                    })
                })
            })
            .collect()
    }

    /// One pass: stat every running container, diff against the last pass,
    /// and broadcast. Returns without sending when nothing is running.
    async fn sample_usage(&self) {
        let targets = self.usage_targets();
        if targets.is_empty() {
            return;
        }
        let Some(adapter) = self.inner.adapter.lock().unwrap().clone() else {
            return;
        };

        use futures::StreamExt;
        let readings: Vec<(Target, StatsSample)> = futures::stream::iter(targets)
            .map(|target| {
                let adapter = adapter.clone();
                async move {
                    match adapter.container_stats(&target.container_id).await {
                        Ok(sample) => Some((target, sample)),
                        // A container that stopped mid-tick is the common
                        // case here, and not worth a log line.
                        Err(_) => None,
                    }
                }
            })
            .buffer_unordered(MAX_CONCURRENT_SAMPLES)
            .filter_map(|result| async move { result })
            .collect::<Vec<_>>()
            .await;

        let mut host_cores = 0u32;
        let mut host_memory_bytes = 0u64;
        let mut services = Vec::with_capacity(readings.len());
        let mut previous = self.inner.usage_prev.lock().unwrap();
        let mut next: HashMap<String, StatsSample> = HashMap::with_capacity(readings.len());

        for (target, current) in readings {
            host_cores = host_cores.max(current.online_cpus);
            // An unlimited container reports host RAM as its limit, which is
            // the normal Sail case — so the largest limit seen is the host's.
            host_memory_bytes = host_memory_bytes.max(current.memory_limit);

            let cpu_cores = previous
                .get(&target.container_id)
                .map(|prev| cores_between(prev, &current))
                .unwrap_or(0.0);
            next.insert(target.container_id.clone(), current);

            services.push(ServiceUsage {
                project: target.project,
                service: target.service,
                cpu_cores,
                memory_bytes: working_set(&current),
                memory_limit_bytes: current.memory_limit,
                // Filled in below: deciding this needs the host total, which
                // is only known once every reading is in.
                memory_limited: false,
            });
        }

        // Replacing the map wholesale drops containers that have gone away,
        // and drops a stale counter when a container id changes — diffing
        // against a previous life would produce a nonsense delta.
        *previous = next;
        drop(previous);

        // A container with no limit of its own reports exactly the host's
        // memory, so "limited" means "below the largest limit we saw" — no
        // magic constant, and correct whatever the machine has.
        for service in &mut services {
            service.memory_limited =
                service.memory_limit_bytes > 0 && service.memory_limit_bytes < host_memory_bytes;
        }

        let _ = self.inner.usage_tx.send(UsageSample {
            at_unix_ms: crate::captures::now_unix_ms(),
            host_cores,
            host_memory_bytes,
            services,
        });
    }

    /// The sampler loop, spawned by `start()`. Idles cheaply — and makes no
    /// docker calls at all — while nothing is subscribed.
    pub(crate) fn usage_loop(&self) {
        let engine = self.clone();
        tokio::spawn(async move {
            loop {
                if engine.inner.usage_tx.receiver_count() == 0 {
                    // Nobody is looking. Clear the previous readings so the
                    // first tick after someone returns is a fresh baseline
                    // rather than a delta across an arbitrary gap.
                    engine.inner.usage_prev.lock().unwrap().clear();
                    tokio::time::sleep(IDLE_POLL).await;
                    continue;
                }
                engine.sample_usage().await;
                tokio::time::sleep(engine.inner.config.usage_interval).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(cpu_total_ns: u64, system_cpu_ns: u64) -> StatsSample {
        StatsSample {
            cpu_total_ns,
            system_cpu_ns,
            online_cpus: 8,
            memory_usage: 0,
            memory_cache: 0,
            memory_limit: 0,
        }
    }

    #[test]
    fn cores_are_the_share_of_the_machine_times_its_cores() {
        // The container used an eighth of all available CPU time, on 8 cores:
        // exactly one core's worth.
        let cores = cores_between(&sample(0, 0), &sample(1_000, 8_000));
        assert!((cores - 1.0).abs() < f64::EPSILON, "{cores}");

        // Half of one core.
        let cores = cores_between(&sample(0, 0), &sample(500, 8_000));
        assert!((cores - 0.5).abs() < f64::EPSILON, "{cores}");

        // Saturating every core.
        let cores = cores_between(&sample(0, 0), &sample(8_000, 8_000));
        assert!((cores - 8.0).abs() < f64::EPSILON, "{cores}");
    }

    #[test]
    fn a_first_reading_has_nothing_to_subtract_from() {
        // Identical counters: no time passed, so no usage.
        assert_eq!(cores_between(&sample(500, 8_000), &sample(500, 8_000)), 0.0);
    }

    #[test]
    fn a_counter_reset_does_not_produce_a_wild_reading() {
        // The container restarted under the same id, so its CPU counter went
        // backwards while the host's kept climbing.
        let cores = cores_between(&sample(9_000, 1_000), &sample(100, 2_000));
        assert_eq!(cores, 0.0);

        // And a ratio that would exceed the machine is clamped to it.
        let cores = cores_between(&sample(0, 1_000), &sample(100_000, 1_100));
        assert_eq!(cores, 8.0);
    }

    #[test]
    fn the_working_set_excludes_reclaimable_cache() {
        let mut s = sample(0, 0);
        s.memory_usage = 2 * 1024 * 1024 * 1024;
        s.memory_cache = 1_800 * 1024 * 1024;
        // What docker stats would show: ~248 MB, not 2 GB.
        assert_eq!(working_set(&s), 2 * 1024 * 1024 * 1024 - 1_800 * 1024 * 1024);
    }

    #[test]
    fn cache_larger_than_usage_does_not_underflow() {
        let mut s = sample(0, 0);
        s.memory_usage = 100;
        s.memory_cache = 500;
        assert_eq!(working_set(&s), 0);
    }
}
