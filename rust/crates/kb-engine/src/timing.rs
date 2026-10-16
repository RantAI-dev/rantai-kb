//! Per-stage latency accounting for the retrieval pipeline.
//!
//! One `/v1/search` costs query embedding, two retrieval arms that run
//! concurrently, an alive check, fusion, an optional rerank, neighbours,
//! figures and context building. Which of those dominates is an empirical
//! question, and the answer decides where the next week goes: a store that
//! scans every vector and a reranker that is slow on CPU look identical from
//! the outside at "2.3 seconds". So the engine times itself, per stage, on
//! every request, and the number travels out in the response.
//!
//! Two rules keep the number honest:
//!
//! * **The clock is a trait, not `Instant::now()`.** [`SystemClock`] is the
//!   production implementation; a test can inject [`ManualClock`] and assert
//!   the accounting without sleeping. `kb-engine` still depends on nothing but
//!   `std` for time, which is what keeps the boundary check trivially true.
//! * **Overlapping stages are never summed.** The vector arm and the keyword
//!   arm run inside one `tokio::join!`, so `vector_arm_ms + keyword_arm_ms`
//!   double-counts whatever they shared. The *accounted* columns are the ones
//!   that partition the request (`arms_ms` is the wall clock of the concurrent
//!   phase); the overlapping measurements live in `detail` and are reported
//!   separately. [`StageTimings::accounted_sum_ms`] is therefore a real bound
//!   on the total, and the server test asserts it stays one.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A monotonic clock the pipeline is timed against.
///
/// `Instant` is returned rather than a duration so a clock can answer "now" and
/// the pipeline keeps doing the subtracting, which is what lets [`ManualClock`]
/// lie convincingly.
pub trait Clock: Send + Sync + fmt::Debug {
    /// A monotonic reading. Never goes backwards.
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock that only moves when it is told to.
///
/// Every reading is `base + nanos`, so two readings of the same value are the
/// same instant: a stage that does no work measures exactly zero rather than a
/// scheduler's worth of noise. Tests use it to assert the arithmetic of
/// [`StageTimings`] instead of the arithmetic of the kernel.
#[derive(Debug)]
pub struct ManualClock {
    base: Instant,
    nanos: AtomicU64,
    /// Nanoseconds added by each reading, so a stage that *should* cost
    /// something can be made to.
    step_nanos: AtomicU64,
}

impl ManualClock {
    /// A clock frozen at `base`; readings return exactly `base`.
    #[must_use]
    pub fn frozen(base: Instant) -> Self {
        Self {
            base,
            nanos: AtomicU64::new(0),
            step_nanos: AtomicU64::new(0),
        }
    }

    /// A clock frozen at the real "now", advancing [`Duration::from_millis(1)`]
    /// per reading.
    #[must_use]
    pub fn ticking_ms() -> Self {
        Self::stepping(Duration::from_millis(1))
    }

    /// A clock frozen at the real "now", advancing `step` per reading.
    #[must_use]
    pub fn stepping(step: Duration) -> Self {
        Self {
            base: Instant::now(),
            nanos: AtomicU64::new(0),
            step_nanos: AtomicU64::new(u64::try_from(step.as_nanos()).unwrap_or(0)),
        }
    }

    /// Move the clock forward.
    pub fn advance(&self, by: Duration) {
        self.nanos.fetch_add(
            u64::try_from(by.as_nanos()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::frozen(Instant::now())
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        let step = self.step_nanos.load(Ordering::SeqCst);
        let nanos = self.nanos.fetch_add(step, Ordering::SeqCst);
        self.base + Duration::from_nanos(nanos)
    }
}

/// A running measurement against a [`Clock`].
#[derive(Debug, Clone, Copy)]
pub struct Stopwatch<'a> {
    clock: &'a dyn Clock,
    start: Instant,
}

impl<'a> Stopwatch<'a> {
    /// Start measuring.
    #[must_use]
    pub fn start(clock: &'a dyn Clock) -> Self {
        Self {
            clock,
            start: clock.now(),
        }
    }

    /// Whole microseconds since [`Stopwatch::start`].
    #[must_use]
    pub fn elapsed_us(&self) -> u64 {
        u64::try_from(
            self.clock
                .now()
                .saturating_duration_since(self.start)
                .as_micros(),
        )
        .unwrap_or(u64::MAX)
    }

    /// Milliseconds since [`Stopwatch::start`], with microsecond resolution.
    ///
    /// Microseconds are kept rather than rounding to whole milliseconds: a
    /// cache hit and a 0.9 ms arm are both "0 ms" once rounded, and the
    /// difference between them is the whole point of measuring.
    #[must_use]
    pub fn elapsed_ms(&self) -> f64 {
        self.elapsed_us() as f64 / 1000.0
    }
}

/// How long each stage of one retrieval took, in milliseconds.
///
/// The two groups are not interchangeable. The first is a *partition*: the
/// stages run one after another, so they add up to no more than the total. The
/// second is *overlapping*: the query embed, the arms and the keyword arm's
/// alive check all happen inside `arms_ms`, so adding them to the first group
/// would double-count the time they shared.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StageTimings {
    // ── accounted: sequential, they partition the request ──
    /// Query expansion, when enabled. Usually 0.
    pub expand_ms: f64,
    /// The concurrent retrieval phase: query embed + vector arm + keyword arm,
    /// whatever ran inside it.
    pub arms_ms: f64,
    /// Alive check and title/category enrichment on the surviving hits.
    pub enrich_ms: f64,
    /// RRF fusion.
    pub fusion_ms: f64,
    /// Cross-encoder rerank, when enabled and when there were candidates.
    pub rerank_ms: f64,
    /// Neighbour fetch and weave.
    pub neighbours_ms: f64,
    /// Figure co-retrieval and the optional figure gate.
    pub figures_ms: f64,
    /// Building the context string and the numbered sources.
    pub context_ms: f64,
    /// The whole pipeline, start to finish.
    pub total_ms: f64,

    // ── detail: overlapping, reported separately, never summed ──
    /// The whole pipeline as measured inside the engine. Below `total_ms` in
    /// the response, which also carries the route's own work.
    pub engine_total_ms: f64,
    /// Embedding the query, whether it was a cache hit or a provider call.
    pub query_embed_ms: f64,
    /// True when the query embedding came from the cache. Counted rather than
    /// averaged: one number for "hit" and one for "miss" is two numbers.
    pub query_embed_cached: bool,
    /// The vector arm's own store call, excluding the query embed.
    pub vector_arm_ms: f64,
    /// The keyword (BM25) arm, including its alive check.
    pub keyword_arm_ms: f64,
    /// The entity arm. Always 0 while the Rust pipeline has no entity arm;
    /// reported rather than omitted so the shape of the answer does not change
    /// when one is built.
    pub entity_arm_ms: f64,
    /// `find_alive_meta_by_ids` inside the keyword arm (Postgres).
    pub scope_check_ms: f64,
}

impl StageTimings {
    /// The accounted stages, which partition the request.
    ///
    /// Never more than [`StageTimings::total_ms`] for a pipeline that did what
    /// it was asked: it is the sum of sequential spans, and the only
    /// non-accounted work is per-request bookkeeping.
    #[must_use]
    pub fn accounted_sum_ms(&self) -> f64 {
        self.expand_ms
            + self.arms_ms
            + self.enrich_ms
            + self.fusion_ms
            + self.rerank_ms
            + self.neighbours_ms
            + self.figures_ms
            + self.context_ms
    }

    /// The stages as `(name, ms)` pairs, accounted first, for logging and for
    /// a caller that wants to render them without knowing the field names.
    #[must_use]
    pub fn accounted_pairs(&self) -> [(&'static str, f64); 8] {
        [
            ("expand", self.expand_ms),
            ("arms", self.arms_ms),
            ("enrich", self.enrich_ms),
            ("fusion", self.fusion_ms),
            ("rerank", self.rerank_ms),
            ("neighbours", self.neighbours_ms),
            ("figures", self.figures_ms),
            ("context", self.context_ms),
        ]
    }

    /// The overlapping measurements, as `(name, ms)` pairs.
    #[must_use]
    pub fn detail_pairs(&self) -> [(&'static str, f64); 6] {
        [
            ("queryEmbed", self.query_embed_ms),
            ("vectorArm", self.vector_arm_ms),
            ("keywordArm", self.keyword_arm_ms),
            ("entityArm", self.entity_arm_ms),
            ("scopeCheck", self.scope_check_ms),
            ("engineTotal", self.engine_total_ms),
        ]
    }
}

/// Log one stage of the pipeline at debug level, under a stable name.
///
/// `docs/architecture-target.md` §7 asks for one span per stage with an
/// OpenInference kind; the names here are the stable ones a dashboard or a log
/// query can match on, and the kind is carried as a field so an exporter can
/// label it.
#[must_use]
pub fn stage_span(stage: &'static str, kind: &'static str) -> tracing::Span {
    tracing::debug_span!("kb.stage", stage, openinference.span_kind = kind)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_moves_forward() {
        let clock = SystemClock;
        let sw = Stopwatch::start(&clock);
        std::thread::sleep(Duration::from_millis(2));
        assert!(sw.elapsed_us() >= 1_000, "a 2 ms sleep is not zero time");
    }

    #[test]
    fn a_frozen_clock_measures_nothing() {
        let clock = ManualClock::frozen(Instant::now());
        let sw = Stopwatch::start(&clock);
        assert_eq!(sw.elapsed_us(), 0);
        assert_eq!(sw.elapsed_ms(), 0.0);
    }

    #[test]
    fn a_ticking_clock_advances_exactly_one_step_per_reading() {
        let clock = ManualClock::stepping(Duration::from_micros(500));
        let start = clock.now();
        let second = clock.now();
        let third = clock.now();
        assert_eq!(second - start, Duration::from_micros(500));
        assert_eq!(third - start, Duration::from_micros(1_000));
    }

    #[test]
    fn a_stopwatch_over_a_manual_clock_is_exact() {
        // Each reading moves a known amount, so the measurement is arithmetic
        // rather than timing. One reading: 2 ms between start and elapsed.
        let clock = ManualClock::stepping(Duration::from_millis(2));
        let sw = Stopwatch::start(&clock);
        assert_eq!(sw.elapsed_us(), 2_000);
    }

    #[test]
    fn every_reading_costs_the_clock_one_step() {
        // `elapsed_us` reads the clock, so calling it twice measures twice:
        // a stopwatch is not a snapshot, it is the time since it was started
        // *as of now*.
        let clock = ManualClock::stepping(Duration::from_millis(2));
        let sw = Stopwatch::start(&clock);
        assert_eq!(sw.elapsed_us(), 2_000);
        assert_eq!(sw.elapsed_us(), 4_000);
    }

    #[test]
    fn advancing_a_clock_after_the_stopwatch_started_is_counted() {
        let clock = ManualClock::frozen(Instant::now());
        let sw = Stopwatch::start(&clock);
        clock.advance(Duration::from_millis(12));
        assert_eq!(sw.elapsed_us(), 12_000);
    }

    #[test]
    fn the_accounted_stages_partition_the_request() {
        let t = StageTimings {
            expand_ms: 0.0,
            arms_ms: 200.0,
            enrich_ms: 3.0,
            fusion_ms: 0.5,
            rerank_ms: 40.0,
            neighbours_ms: 2.0,
            figures_ms: 1.0,
            context_ms: 0.5,
            total_ms: 250.0,
            ..StageTimings::default()
        };
        let sum = t.accounted_sum_ms();
        assert!(
            sum <= t.total_ms,
            "accounted {sum} exceeds total {}",
            t.total_ms
        );
    }

    /// The rule the whole module exists for: two arms inside one `join!`
    /// overlap, so the accounted column is the phase and the arms are detail.
    #[test]
    fn overlapping_arms_do_not_inflate_the_total() {
        let t = StageTimings {
            arms_ms: 100.0,
            // Both arms ran the whole 100 ms, because they were concurrent.
            vector_arm_ms: 98.0,
            keyword_arm_ms: 97.0,
            total_ms: 105.0,
            ..StageTimings::default()
        };
        assert!(t.accounted_sum_ms() <= t.total_ms);
        assert!(
            t.vector_arm_ms + t.keyword_arm_ms > t.arms_ms,
            "the detail really does double-count; that is why it is detail"
        );
    }

    #[test]
    fn default_timings_are_zero_and_consistent() {
        let t = StageTimings::default();
        assert_eq!(t.accounted_sum_ms(), 0.0);
        assert_eq!(t.total_ms, 0.0);
        assert!(!t.query_embed_cached);
        assert_eq!(t.accounted_pairs().len(), 8);
        assert_eq!(t.detail_pairs().len(), 6);
    }
}
