//! Locks and cancellable parallel sweeps.
//!
//! This is the one module that is a redesign rather than a translation. The
//! TypeScript reference serialised everything on promise chains because it had
//! a single thread and an event loop; Rust has real threads, so the shapes
//! that made sense there do not all make sense here:
//!
//! - `KeyedLock` no longer offers "queue behind the current holder". The only
//!   caller that needed queueing was the settings writer, and in Rust that is
//!   a `Mutex` around the settings themselves. What remains is the semantics
//!   installs actually want: refuse immediately if the game is busy, so a
//!   second click is told so rather than silently firing again minutes later.
//!
//! - The bounded-concurrency map is threads over a shared cursor rather than a
//!   pool of promises, so a library scan uses every core instead of
//!   interleaving IO waits on one.
//!
//! - Cancellation is an explicit token rather than an `AbortSignal`, and it is
//!   checked between items. A superseded sweep stops feeding results into
//!   shared state instead of racing the sweep that replaced it.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// A cheap, clonable "stop what you are doing" flag.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Held for as long as a job owns its key; releases on drop, including on a
/// panic, so a crashed job cannot leave a game permanently marked busy.
pub struct JobGuard {
    key: String,
    busy: Arc<Mutex<BTreeSet<String>>>,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        lock(&self.busy).remove(&self.key);
    }
}

impl JobGuard {
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Refuses rather than queues.
///
/// Installing is destructive and slow. A second click on the same game should
/// be told "busy" at once, and a click on a *different* game should also be
/// refused while an install runs, because both touch the same component cache.
/// The upstream project used a single module-level boolean for the second
/// case, which gave the UI no way to ask which game was busy so it could grey
/// out one card rather than the whole window.
#[derive(Debug, Default)]
pub struct KeyedLock {
    busy: Arc<Mutex<BTreeSet<String>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock means a previous holder panicked. The contents here are
    // a set of strings either way, so recovering beats propagating the panic
    // into every later call.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Windows paths differing only in case are the same folder.
fn normalise(key: &str) -> String {
    key.to_lowercase()
}

impl KeyedLock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_busy(&self, key: &str) -> bool {
        lock(&self.busy).contains(&normalise(key))
    }

    pub fn busy_keys(&self) -> Vec<String> {
        lock(&self.busy).iter().cloned().collect()
    }

    /// Take the key, or `None` if somebody already has it.
    pub fn try_acquire(&self, key: &str) -> Option<JobGuard> {
        let normalised = normalise(key);
        let mut busy = lock(&self.busy);
        if busy.contains(&normalised) {
            return None;
        }
        busy.insert(normalised.clone());
        Some(JobGuard {
            key: normalised,
            busy: Arc::clone(&self.busy),
        })
    }
}

/// Map over items with bounded concurrency, checking `cancel` between items.
///
/// Returns results in the order of the input, with `None` where the worker
/// failed or where cancellation stopped the sweep before reaching that item.
/// One unreadable folder must not abandon the other ninety-nine, so a worker
/// is expected to turn its own failure into `None` rather than unwinding.
pub fn pooled_map<T, R, F>(
    items: &[T],
    concurrency: usize,
    cancel: &Cancel,
    worker: F,
) -> Vec<Option<R>>
where
    T: Sync,
    R: Send,
    F: Fn(&T, usize) -> Option<R> + Sync,
{
    let width = concurrency.clamp(1, items.len().max(1));
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let cursor = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..width {
            scope.spawn(|| loop {
                if cancel.is_cancelled() {
                    return;
                }
                let index = cursor.fetch_add(1, Ordering::SeqCst);
                let Some(item) = items.get(index) else { return };
                let value = worker(item, index);
                if cancel.is_cancelled() {
                    return;
                }
                if let Some(slot) = lock(&results).get_mut(index) {
                    *slot = value;
                }
            });
        }
    });

    // Consume the mutex rather than locking it: every worker thread has joined
    // by now, so there is nothing left to contend with.
    results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

#[derive(Debug)]
pub struct SweepOutcome<R> {
    /// False when a newer sweep superseded this one, or it was cancelled.
    pub completed: bool,
    pub results: Vec<Option<R>>,
}

/// A long-running pass over the library of which only the newest is wanted.
///
/// The bug this exists to prevent: the upstream project calls its `load()`
/// from eight places, mostly without awaiting it, and `load()` starts a scan
/// which starts an artwork fetch. Add a folder while the first pass is still
/// running and two loops interleave, both writing the same settings keys, both
/// driving the same progress bar, and both asking Steam for the same artwork.
///
/// Starting a sweep therefore cancels the one before it, and a superseded
/// sweep reports that it did not complete.
#[derive(Debug, Default)]
pub struct Sweeper {
    generation: AtomicU64,
    current: Mutex<Option<Cancel>>,
}

impl Sweeper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_running(&self) -> bool {
        lock(&self.current).is_some()
    }

    /// Cancel whatever is in flight, if anything.
    pub fn cancel(&self) {
        if let Some(cancel) = lock(&self.current).take() {
            cancel.cancel();
        }
    }

    pub fn run<T, R, F, P>(
        &self,
        items: &[T],
        concurrency: usize,
        worker: F,
        mut on_progress: P,
    ) -> SweepOutcome<R>
    where
        T: Sync,
        R: Send,
        F: Fn(&T, usize) -> Option<R> + Sync,
        P: FnMut(Progress) + Send,
    {
        // Supersede whatever was in flight before touching shared state.
        self.cancel();

        let cancel = Cancel::new();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.current) = Some(cancel.clone());

        let done = AtomicUsize::new(0);
        let total = items.len();
        let progress = Mutex::new(&mut on_progress);
        (lock(&progress))(Progress { done: 0, total });

        let results = pooled_map(items, concurrency, &cancel, |item, index| {
            let value = worker(item, index);
            let seen = done.fetch_add(1, Ordering::SeqCst) + 1;
            if !cancel.is_cancelled() {
                (lock(&progress))(Progress { done: seen, total });
            }
            value
        });

        // Only the newest sweep may report completion, and only if it was not
        // cancelled part-way.
        let mine = self.generation.load(Ordering::SeqCst) == generation;
        let completed = mine && !cancel.is_cancelled();
        if mine {
            *lock(&self.current) = None;
        }
        SweepOutcome { completed, results }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_key_is_refused_while_it_is_held_and_freed_on_drop() {
        let locks = KeyedLock::new();
        let guard = locks
            .try_acquire("D:\\Games\\Skyrim")
            .expect("first acquire");

        // Case-insensitively the same folder.
        assert!(locks.try_acquire("d:\\games\\skyrim").is_none());
        assert!(locks.is_busy("D:\\Games\\Skyrim"));
        // A different game is not affected.
        assert!(!locks.is_busy("D:\\Games\\Oblivion"));

        drop(guard);
        assert!(!locks.is_busy("D:\\Games\\Skyrim"));
        assert!(locks.try_acquire("D:\\Games\\Skyrim").is_some());
    }

    #[test]
    fn a_panicking_job_still_releases_its_key() {
        let locks = KeyedLock::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = locks.try_acquire("game").expect("acquire");
            panic!("the install exploded");
        }));
        assert!(result.is_err());
        // Releasing on unwind is what stops a crashed job marking a game busy
        // for the rest of the session.
        assert!(!locks.is_busy("game"));
    }

    #[test]
    fn busy_keys_names_the_specific_game() {
        let locks = KeyedLock::new();
        let _a = locks.try_acquire("D:\\Games\\A").expect("a");
        let _b = locks.try_acquire("D:\\Games\\B").expect("b");
        let mut keys = locks.busy_keys();
        keys.sort();
        assert_eq!(keys, vec!["d:\\games\\a", "d:\\games\\b"]);
    }

    #[test]
    fn pooled_map_preserves_order_and_survives_a_failing_worker() {
        let items: Vec<u32> = (0..40).collect();
        let cancel = Cancel::new();
        let results = pooled_map(&items, 4, &cancel, |item, _| {
            if item % 2 == 0 {
                Some(item * 10)
            } else {
                // A worker reporting failure must not abandon the sweep.
                None
            }
        });

        assert_eq!(results.len(), 40);
        assert_eq!(results.first().and_then(|v| *v), Some(0));
        assert_eq!(results.get(1).and_then(|v| *v), None);
        assert_eq!(results.get(2).and_then(|v| *v), Some(20));
    }

    #[test]
    fn pooled_map_actually_runs_in_parallel() {
        let items: Vec<u32> = (0..8).collect();
        let cancel = Cancel::new();
        let started = Instant::now();
        // Eight items, each sleeping 40ms, across four threads: serial would
        // take ~320ms, parallel ~80ms. Assert well inside the gap.
        let results = pooled_map(&items, 4, &cancel, |item, _| {
            std::thread::sleep(Duration::from_millis(40));
            Some(*item)
        });
        let elapsed = started.elapsed();

        assert_eq!(results.iter().filter(|r| r.is_some()).count(), 8);
        assert!(
            elapsed < Duration::from_millis(240),
            "took {elapsed:?}, which suggests it ran serially"
        );
    }

    #[test]
    fn cancelling_stops_a_sweep_early() {
        let items: Vec<u32> = (0..500).collect();
        let cancel = Cancel::new();
        let observed = Arc::new(AtomicUsize::new(0));

        let counter = Arc::clone(&observed);
        let stopper = cancel.clone();
        let results = pooled_map(&items, 2, &cancel, move |item, _| {
            let seen = counter.fetch_add(1, Ordering::SeqCst);
            if seen == 20 {
                stopper.cancel();
            }
            std::thread::sleep(Duration::from_millis(1));
            Some(*item)
        });

        let delivered = results.iter().filter(|r| r.is_some()).count();
        assert!(
            delivered < 500,
            "expected an early stop, delivered {delivered}"
        );
    }

    #[test]
    fn a_new_sweep_supersedes_the_one_before_it() {
        let sweeper = Sweeper::new();
        let items: Vec<u32> = (0..20).collect();

        let first = sweeper.run(&items, 2, |item, _| Some(*item), |_| {});
        assert!(first.completed);

        let second = sweeper.run(&items, 2, |item, _| Some(item * 2), |_| {});
        assert!(second.completed);
        assert_eq!(second.results.first().and_then(|v| *v), Some(0));
        assert_eq!(second.results.get(3).and_then(|v| *v), Some(6));
        assert!(!sweeper.is_running());
    }

    #[test]
    fn a_cancelled_sweep_reports_that_it_did_not_complete() {
        let sweeper = Arc::new(Sweeper::new());
        let items: Vec<u32> = (0..400).collect();

        let stopper = Arc::clone(&sweeper);
        let outcome = sweeper.run(
            &items,
            2,
            move |item, index| {
                if index == 10 {
                    stopper.cancel();
                }
                std::thread::sleep(Duration::from_millis(1));
                Some(*item)
            },
            |_| {},
        );

        assert!(
            !outcome.completed,
            "a cancelled sweep must not claim success"
        );
    }

    #[test]
    fn progress_counts_up_to_the_total_exactly_once() {
        let sweeper = Sweeper::new();
        let items: Vec<u32> = (0..25).collect();
        let seen = Mutex::new(Vec::new());

        let outcome = sweeper.run(
            &items,
            4,
            |item, _| Some(*item),
            |progress| lock(&seen).push(progress.done),
        );

        assert!(outcome.completed);
        let mut counts = lock(&seen).clone();
        assert_eq!(counts.first().copied(), Some(0));
        assert_eq!(counts.iter().max().copied(), Some(25));
        // Every step reported once, with no duplicates or skips.
        counts.sort_unstable();
        counts.dedup();
        assert_eq!(counts.len(), 26, "expected 0..=25 exactly once each");
    }

    use std::time::Instant;
}
