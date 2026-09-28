//! Bounded in-process resource ownership for matrix workers.
//!
//! This pool coordinates cells within one matrix supervisor. It is not a
//! replacement for the host-shared DeviceLeaseSession: platform runners still
//! need the durable OS lease before changing a simulator or device. The pool
//! prevents two cells from entering the same runner-owned resource at once
//! and gives resource waiting the same deadline boundary as cell execution.

use super::matrix::{MAX_MATRIX_RESOURCE_ID_BYTES, MAX_MATRIX_RESOURCES_PER_CELL};
use anyhow::{Result, bail};
use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const RESOURCE_WAIT_SLICE: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Default)]
pub struct MatrixResourcePool {
    state: Arc<ResourceState>,
}

#[derive(Debug, Default)]
struct ResourceState {
    held: Mutex<BTreeSet<String>>,
    changed: Condvar,
}

#[derive(Debug)]
pub struct MatrixResourceGuard {
    pool: MatrixResourcePool,
    resources: Vec<String>,
}

impl MatrixResourcePool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for all requested resources until the supplied cell deadline.
    ///
    /// Resource ordering is normalized before acquisition so two workers
    /// requesting overlapping resource sets cannot deadlock by taking them in
    /// different orders.
    pub fn acquire(&self, resources: &[String], deadline: Instant) -> Result<MatrixResourceGuard> {
        let resources = normalize_resources(resources)?;
        if Instant::now() >= deadline {
            bail!("matrix resource wait deadline exceeded");
        }

        let mut held = self
            .state
            .held
            .lock()
            .map_err(|_| anyhow::anyhow!("matrix resource state was poisoned"))?;
        loop {
            if resources.iter().all(|resource| !held.contains(resource)) {
                held.extend(resources.iter().cloned());
                return Ok(MatrixResourceGuard {
                    pool: self.clone(),
                    resources,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("matrix resource wait deadline exceeded");
            }
            let wait_for = remaining.min(RESOURCE_WAIT_SLICE);
            let (next, _) = self
                .state
                .changed
                .wait_timeout(held, wait_for)
                .map_err(|_| anyhow::anyhow!("matrix resource state was poisoned"))?;
            held = next;
        }
    }

    fn release(&self, resources: &[String]) {
        let Ok(mut held) = self.state.held.lock() else {
            return;
        };
        for resource in resources {
            held.remove(resource);
        }
        self.state.changed.notify_all();
    }
}

impl MatrixResourceGuard {
    pub fn resources(&self) -> &[String] {
        &self.resources
    }

    pub fn release(mut self) {
        self.pool.release(&self.resources);
        self.resources.clear();
    }
}

impl Drop for MatrixResourceGuard {
    fn drop(&mut self) {
        if !self.resources.is_empty() {
            self.pool.release(&self.resources);
            self.resources.clear();
        }
    }
}

fn normalize_resources(resources: &[String]) -> Result<Vec<String>> {
    if resources.len() > MAX_MATRIX_RESOURCES_PER_CELL {
        bail!(
            "matrix cell exceeds the {} resource limit",
            MAX_MATRIX_RESOURCES_PER_CELL
        );
    }
    let mut normalized = BTreeSet::new();
    for resource in resources {
        if resource.is_empty()
            || resource.len() > MAX_MATRIX_RESOURCE_ID_BYTES
            || !resource.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')
            })
        {
            bail!("invalid matrix resource id");
        }
        if !normalized.insert(resource.clone()) {
            bail!("duplicate matrix resource id '{resource}'");
        }
    }
    Ok(normalized.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    fn resource(name: &str) -> String {
        name.into()
    }

    #[test]
    fn different_resources_can_be_held_at_the_same_time() {
        let pool = MatrixResourcePool::new();
        let first = pool
            .acquire(
                &[resource("device:a")],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        let second = pool
            .acquire(
                &[resource("device:b")],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(first.resources(), ["device:a"]);
        assert_eq!(second.resources(), ["device:b"]);
    }

    #[test]
    fn overlapping_resources_wait_until_the_owner_releases() {
        let pool = MatrixResourcePool::new();
        let first = pool
            .acquire(
                &[resource("device:a")],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker_pool = pool.clone();
        let worker = thread::spawn(move || {
            let guard = worker_pool
                .acquire(
                    &[resource("device:a")],
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap();
            sender.send(guard.resources().to_vec()).unwrap();
            guard
        });

        assert!(receiver.recv_timeout(Duration::from_millis(30)).is_err());
        first.release();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            vec![resource("device:a")]
        );
        worker.join().unwrap();
    }

    #[test]
    fn resource_wait_honors_the_cell_deadline() {
        let pool = MatrixResourcePool::new();
        let _first = pool
            .acquire(
                &[resource("device:a")],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        let error = pool
            .acquire(
                &[resource("device:a")],
                Instant::now() + Duration::from_millis(20),
            )
            .unwrap_err();
        assert!(error.to_string().contains("deadline exceeded"));
    }

    #[test]
    fn duplicate_or_unsafe_resources_are_rejected() {
        let pool = MatrixResourcePool::new();
        let error = pool
            .acquire(
                &[resource("device:a"), resource("device:a")],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(error.to_string().contains("duplicate"));
        let error = pool
            .acquire(
                &[resource("../device")],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(error.to_string().contains("invalid"));
    }
}
