//! Bounded CPU work kept off the HTTP executor and simulator thread.
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Lazily created workers. Dropping the sender lets idle threads exit.
pub struct Workers {
    count: usize,
    sender: OnceLock<Result<mpsc::SyncSender<Job>, String>>,
}

impl Workers {
    pub fn new(requested: u8) -> Self {
        let available = std::thread::available_parallelism().map_or(1, usize::from);
        Self {
            count: worker_count(requested, available),
            sender: OnceLock::new(),
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    fn start(&self) -> Result<mpsc::SyncSender<Job>, String> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(self.count * 2);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..self.count {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("openatc-cpu-{index}"))
                .spawn(move || {
                    loop {
                        let job = receiver
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .recv();
                        let Ok(job) = job else {
                            break;
                        };
                        // A failed import must not permanently remove a worker.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::config_cache::on_worker(job);
                        }));
                    }
                })
                .map_err(|e| format!("Could not start CPU worker: {e}"))?;
        }
        Ok(sender)
    }

    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, String> {
        let sender = self
            .sender
            .get_or_init(|| self.start())
            .as_ref()
            .map_err(Clone::clone)?;
        let (done, result) = oneshot::channel();
        sender
            .try_send(Box::new(move || {
                if !done.is_closed() {
                    let _ = done.send(work());
                }
            }))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => {
                    "CPU work queue is full; try again shortly.".to_owned()
                }
                mpsc::TrySendError::Disconnected(_) => "CPU workers are unavailable.".to_owned(),
            })?;
        result
            .await
            .map_err(|_| "CPU task failed or was cancelled.".to_owned())
    }
}

fn worker_count(requested: u8, available: usize) -> usize {
    if requested == 0 {
        (available / 2).clamp(1, 4)
    } else {
        usize::from(requested).clamp(1, 16).min(available.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_budget_leaves_capacity_and_manual_budget_is_bounded() {
        assert_eq!(worker_count(0, 1), 1);
        assert_eq!(worker_count(0, 8), 4);
        assert_eq!(worker_count(0, 64), 4);
        assert_eq!(worker_count(1, 64), 1);
        assert_eq!(worker_count(255, 64), 16);
        assert_eq!(worker_count(8, 2), 2);
    }
    #[tokio::test]
    async fn independent_jobs_overlap_and_a_panic_does_not_lose_the_worker() {
        let pool = Workers {
            count: 2,
            sender: OnceLock::new(),
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let other = barrier.clone();
        let (a, b) = tokio::join!(
            pool.run(move || {
                barrier.wait();
                21
            }),
            pool.run(move || {
                other.wait();
                34
            })
        );
        assert_eq!(a.unwrap() + b.unwrap(), 55);
        assert!(pool.run(|| panic!("bad import")).await.is_err());
        assert_eq!(pool.run(|| 7).await.unwrap(), 7);
    }

    #[tokio::test]
    async fn a_full_queue_rejects_work_and_dropped_requests_never_execute() {
        let pool = Workers {
            count: 1,
            sender: OnceLock::new(),
        };
        let (started, waiting) = oneshot::channel();
        let (release, gate) = mpsc::channel();
        let mut active = Box::pin(pool.run(move || {
            let _ = started.send(());
            gate.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            1
        }));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(active.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        waiting.await.unwrap();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let mut cancelled =
            Box::pin(pool.run(move || flag.store(true, std::sync::atomic::Ordering::SeqCst)));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(cancelled.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        let mut next = Box::pin(pool.run(|| 2));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(pool.run(|| 3).await.unwrap_err().contains("queue is full"));
        drop(cancelled);
        release.send(()).unwrap();
        assert_eq!(active.await.unwrap(), 1);
        assert_eq!(next.await.unwrap(), 2);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
