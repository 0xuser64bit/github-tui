//! Bounded, session-owned dispatch for blocking GitHub requests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

const WORKER_COUNT: usize = 4;
const QUEUE_CAPACITY: usize = 16;

#[derive(Clone, Default)]
pub(crate) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

struct Job {
    cancellation: Cancellation,
    run: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    stopped: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    capacity: usize,
}

impl Shared {
    fn stop(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
        queue.stopped = true;
        queue.jobs.clear();
        self.ready.notify_all();
    }
}

#[derive(Default)]
pub(crate) struct RequestPool {
    shared: Option<Arc<Shared>>,
}

impl RequestPool {
    pub(crate) fn execute(
        &mut self,
        cancellation: Cancellation,
        run: impl FnOnce() + Send + 'static,
    ) -> Result<(), String> {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let shared = match &self.shared {
            Some(shared) => shared,
            None => self
                .shared
                .insert(start_workers(WORKER_COUNT, QUEUE_CAPACITY)?),
        };
        let mut queue = shared
            .queue
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        queue.jobs.retain(|job| !job.cancellation.is_cancelled());
        if queue.jobs.len() >= shared.capacity {
            return Err("Too many pending GitHub requests. Press r to retry.".to_string());
        }
        queue.jobs.push_back(Job {
            cancellation,
            run: Box::new(run),
        });
        shared.ready.notify_one();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn with_limits(workers: usize, capacity: usize) -> Result<Self, String> {
        Ok(Self {
            shared: Some(start_workers(workers, capacity)?),
        })
    }
}

impl Drop for RequestPool {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop();
        }
    }
}

fn start_workers(workers: usize, capacity: usize) -> Result<Arc<Shared>, String> {
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue::default()),
        ready: Condvar::new(),
        capacity,
    });
    for index in 0..workers {
        let worker = shared.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("github-fetch-{index}"))
            .spawn(move || run_worker(worker))
        {
            shared.stop();
            return Err(format!(
                "Could not start GitHub request workers: {error}. Retry with r."
            ));
        }
    }
    Ok(shared)
}

fn run_worker(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut queue = shared
                .queue
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            loop {
                if queue.stopped {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };
        if !job.cancellation.is_cancelled() {
            (job.run)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const DEADLINE: Duration = Duration::from_secs(5);

    fn pool(workers: usize, capacity: usize) -> RequestPool {
        RequestPool::with_limits(workers, capacity).unwrap()
    }

    fn block_worker(pool: &mut RequestPool) -> mpsc::Sender<()> {
        let (started, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        pool.execute(Cancellation::default(), move || {
            started.send(()).unwrap();
            let _ = blocked.recv();
        })
        .unwrap();
        waiting.recv_timeout(DEADLINE).unwrap();
        release
    }

    #[test]
    fn concurrent_fetches_and_pending_work_are_bounded() {
        let mut pool = pool(2, 2);
        let first = block_worker(&mut pool);
        let second = block_worker(&mut pool);
        let (completed, results) = mpsc::channel();
        for number in 0..2 {
            let completed = completed.clone();
            pool.execute(Cancellation::default(), move || {
                completed.send(number).unwrap();
            })
            .unwrap();
        }

        let error = pool
            .execute(Cancellation::default(), || panic!("queue overflow ran"))
            .unwrap_err();
        assert!(error.contains("pending"));
        assert!(error.contains("retry"));
        first.send(()).unwrap();
        second.send(()).unwrap();
        let mut received = [
            results.recv_timeout(DEADLINE).unwrap(),
            results.recv_timeout(DEADLINE).unwrap(),
        ];
        received.sort();
        assert_eq!(received, [0, 1]);
    }

    #[test]
    fn cancelled_work_reclaims_capacity_before_workers_become_available() {
        let mut pool = pool(1, 1);
        let release = block_worker(&mut pool);
        let cancellation = Cancellation::default();
        pool.execute(cancellation.clone(), || panic!("cancelled job ran"))
            .unwrap();
        cancellation.cancel();
        let (completed, result) = mpsc::channel();

        pool.execute(Cancellation::default(), move || {
            completed.send(()).unwrap();
        })
        .unwrap();

        release.send(()).unwrap();
        result.recv_timeout(DEADLINE).unwrap();
    }

    #[test]
    fn workers_skip_cancelled_jobs_without_another_submission() {
        let mut pool = pool(1, 2);
        let release = block_worker(&mut pool);
        let cancellation = Cancellation::default();
        pool.execute(cancellation.clone(), || panic!("cancelled job ran"))
            .unwrap();
        let (completed, result) = mpsc::channel();
        pool.execute(Cancellation::default(), move || {
            completed.send(()).unwrap();
        })
        .unwrap();
        cancellation.cancel();

        release.send(()).unwrap();
        result.recv_timeout(DEADLINE).unwrap();
    }

    #[test]
    fn dropping_the_pool_discards_pending_work_without_waiting() {
        let mut pool = pool(1, 1);
        let release = block_worker(&mut pool);
        let (completed, result) = mpsc::channel();
        pool.execute(Cancellation::default(), move || {
            completed.send(()).unwrap();
        })
        .unwrap();

        let (dropped, notification) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(pool);
            dropped.send(()).unwrap();
        });
        let shutdown = notification.recv_timeout(DEADLINE);
        release.send(()).unwrap();
        dropper.join().unwrap();
        shutdown.unwrap();

        assert!(matches!(
            result.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn already_cancelled_jobs_do_not_start_workers() {
        let mut pool = RequestPool::default();
        let cancellation = Cancellation::default();
        cancellation.cancel();

        pool.execute(cancellation, || panic!("cancelled job ran"))
            .unwrap();

        assert!(pool.shared.is_none());
    }
}
