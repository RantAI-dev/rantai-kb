//! Ingest seats: the counting gates that keep one stage of the ingest
//! pipeline from taking more of the machine than it should (`ING-6`).
//!
//! A **seat** is a permit for one stage. A document holds a seat only while it
//! is in that stage, and gives it back on every exit path: success, failure, a
//! panic, or a cancelled wait. The gate is **priority ordered**: when a seat
//! frees, the waiter whose job was created first gets it, not the waiter that
//! happened to ask first. That is the difference between a queue that serves
//! the oldest upload first and one that serves the luckiest — and it is what
//! keeps a 300-page scan from holding a one-page memo behind it.
//!
//! `tokio::sync::Semaphore` is deliberately not used: it is first-in-first-out
//! by the order the waiters *queued*, which is the order the calls arrived, not
//! the order the documents were uploaded.
//!
//! The waiters are served from a binary heap keyed by [`SeatPriority`], and a
//! seat is handed over through a `oneshot` channel. Handing the *permit*
//! over — rather than a wake-up signal — is what makes a cancelled waiter
//! safe: dropping the receiving future drops the permit inside the channel,
//! which gives the seat straight back to the next waiter. A cancelled wait
//! can therefore neither leak a seat nor steal one.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use kb_ports::JobRecord;
use tokio::sync::oneshot;

/// Who gets the next seat.
///
/// The job's creation time, then its id. Two documents created in the same
/// millisecond still have a total order, so the queue never depends on the
/// order in which the waiters happened to arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatPriority {
    /// When the job was created, in epoch milliseconds. `None` when the
    /// adapter does not report it: such a job is served **last**, because an
    /// unknown age is not evidence of being old.
    pub created_at: Option<i64>,
    /// The job's id: the tie-break between two jobs of the same age.
    pub job_id: String,
}

impl SeatPriority {
    /// A priority for a job created at `created_at` (epoch milliseconds).
    #[must_use]
    pub fn new(created_at: Option<i64>, job_id: impl Into<String>) -> Self {
        Self {
            created_at,
            job_id: job_id.into(),
        }
    }

    /// The priority of a claimed job: when it was created, then its id.
    #[must_use]
    pub fn of_job(job: &JobRecord) -> Self {
        Self::new(job.created_at, job.id.clone())
    }
}

impl Ord for SeatPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.created_at, other.created_at) {
            (Some(a), Some(b)) => a.cmp(&b),
            (None, None) => Ordering::Equal,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
        }
        .then_with(|| self.job_id.cmp(&other.job_id))
    }
}

impl PartialOrd for SeatPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A counting gate for one stage of the ingest pipeline.
///
/// Cloning a gate shares it: every clone is the same pool of seats.
#[derive(Clone)]
pub struct Seats {
    inner: Arc<Inner>,
}

struct Inner {
    /// How many seats this gate has. `0` means no seat is ever free; the
    /// server rejects `0` when it reads the configuration.
    capacity: usize,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Seats that are taken: held by a job, or on their way to one.
    taken: usize,
    /// The jobs waiting for a seat, oldest first.
    waiters: BinaryHeap<Waiter>,
    /// A counter that keeps the wait order total even for equal priorities.
    next_seq: u64,
}

struct Waiter {
    priority: SeatPriority,
    seq: u64,
    /// The seat, handed over when this waiter is served.
    tx: oneshot::Sender<Permit>,
}

impl Ord for Waiter {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` is a max-heap and the *lowest* priority has to be
        // served first, so the comparison is reversed. The sequence number
        // keeps the order of otherwise equal waiters (first queued first).
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Waiter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Waiter {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}

impl Eq for Waiter {}

/// The right to hold one seat.
///
/// Dropping it gives the seat back, whoever drops it: a finished document, a
/// failed one, a panicking one, or a waiter that was cancelled after the
/// seat had already been handed to it.
struct Permit {
    /// `None` once the seat has been given back.
    inner: Option<Arc<Inner>>,
}

impl Permit {
    /// Give the seat up without running the release path. Used when the
    /// release path has already run — the seat was handed to the next waiter,
    /// or the counter was decremented.
    fn disarm(&mut self) {
        self.inner = None;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            Inner::release(&inner);
        }
    }
}

impl Inner {
    /// The state, ignoring poisoning: the state is a counter and a heap, and
    /// a panic in an unrelated task must not close the gate forever.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Give one seat back: to the oldest waiter, or to the pool.
    ///
    /// A waiter whose future was dropped is skipped: the seat goes to the
    /// next one, and to the pool when there is none.
    fn release(inner: &Arc<Self>) {
        let mut permit = Permit {
            inner: Some(Arc::clone(inner)),
        };
        let mut state = inner.lock();
        loop {
            match state.waiters.pop() {
                Some(waiter) => match waiter.tx.send(permit) {
                    // The seat moved to that waiter; `taken` does not change.
                    Ok(()) => return,
                    // The waiter is gone: the permit comes back and the seat
                    // goes to the next one.
                    Err(returned) => permit = returned,
                },
                None => {
                    state.taken -= 1;
                    permit.disarm();
                    return;
                }
            }
        }
    }
}

impl Seats {
    /// A gate with `capacity` seats.
    ///
    /// A capacity of `0` means no seat is ever free: the server refuses to
    /// configure that, because a stage that can never run is a stage that
    /// hangs every document.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                capacity,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// How many seats this gate has.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// How many jobs are waiting for a seat right now.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.inner.lock().waiters.len()
    }

    /// How many seats are taken right now (held, or on their way to a job).
    #[must_use]
    pub fn taken(&self) -> usize {
        self.inner.lock().taken
    }

    /// Take a seat, waiting for one when the gate is full.
    ///
    /// The wait is ordered by `priority`: the oldest job waiting gets the
    /// next seat. Dropping the returned future gives up the wait without
    /// taking a seat, and without stealing one from anybody else.
    pub async fn acquire(&self, priority: SeatPriority) -> SeatGuard {
        let (tx, rx) = oneshot::channel::<Permit>();
        {
            let mut state = self.inner.lock();
            if state.taken < self.inner.capacity {
                state.taken += 1;
                drop(state);
                return SeatGuard {
                    permit: Some(Permit {
                        inner: Some(Arc::clone(&self.inner)),
                    }),
                };
            }
            let seq = state.next_seq;
            state.next_seq += 1;
            state.waiters.push(Waiter { priority, seq, tx });
        }
        // Wait for the seat. The permit travels through the channel: if this
        // future is dropped first, the permit inside the channel is dropped
        // with it and the seat goes to the next waiter.
        match rx.await {
            Ok(permit) => SeatGuard {
                permit: Some(permit),
            },
            Err(_) => {
                // The gate is gone (the process is shutting down). No seat was
                // taken, so there is nothing to give back.
                SeatGuard { permit: None }
            }
        }
    }
}

/// A seat, held for as long as the guard lives.
///
/// Dropping the guard — on every path out of the stage, including a panic —
/// gives the seat back to the pool.
#[must_use = "the seat is released as soon as the guard is dropped"]
pub struct SeatGuard {
    permit: Option<Permit>,
}

impl Drop for SeatGuard {
    fn drop(&mut self) {
        // The permit gives the seat back when it is dropped; that is the
        // whole release path, so a panic or a cancelled job releases it too.
        if let Some(permit) = self.permit.take() {
            drop(permit);
        }
    }
}

/// The three stage gates of the ingest pipeline.
#[derive(Clone)]
pub struct StageSeats {
    /// Local parsing, PDF splitting and rendering.
    pub extract: Seats,
    /// One document's embedding requests.
    pub embed: Seats,
    /// The vector upsert, the document row and the graph writes.
    pub store: Seats,
}

impl StageSeats {
    /// Build the three gates.
    #[must_use]
    pub fn new(extract: usize, embed: usize, store: usize) -> Self {
        Self {
            extract: Seats::new(extract),
            embed: Seats::new(embed),
            store: Seats::new(store),
        }
    }
}

/// How many seats each stage has (`KB_SEATS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatCapacity {
    /// Seats for local extraction.
    pub extract: usize,
    /// Seats for embedding.
    pub embed: usize,
    /// Seats for the writes.
    pub store: usize,
}

impl Default for SeatCapacity {
    fn default() -> Self {
        Self {
            extract: 2,
            embed: 4,
            store: 2,
        }
    }
}

impl SeatCapacity {
    /// The gates these capacities describe.
    #[must_use]
    pub fn seats(&self) -> StageSeats {
        StageSeats::new(self.extract, self.embed, self.store)
    }
}
