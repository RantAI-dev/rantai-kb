//! Ingest progress model. Pure arithmetic so both the worker and the API layer
//! agree on what a job's percentage means.
//!
//! Progress is weighted across pipeline steps (extraction dominates) so the bar
//! advances in proportion to real cost rather than jumping one-Nth per step.
//! A step with a known `current/total` fills its slice smoothly; an
//! indeterminate one holds at the slice start.

/// One stage of the ingest pipeline, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestStep {
    /// Written by the claim, before any work starts.
    Queued,
    /// Turning the stored file into text.
    Extracting,
    /// Splitting the text into chunks.
    Chunking,
    /// Per-chunk LLM entity and relation pass.
    ExtractingEntities,
    /// Cropping and uploading figure assets.
    ProcessingFigures,
    /// Asking the embedding endpoint for vectors.
    Embedding,
    /// Writing chunks to the vector store.
    Storing,
    /// Terminal.
    Done,
}

impl IngestStep {
    /// The wire string stored in `IngestJob.step` and shown by `GET /v1/jobs/:id`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Extracting => "extracting",
            Self::Chunking => "chunking",
            Self::ExtractingEntities => "extracting_entities",
            Self::ProcessingFigures => "processing_figures",
            Self::Embedding => "embedding",
            Self::Storing => "storing",
            Self::Done => "done",
        }
    }

    fn position(self) -> usize {
        match self {
            Self::Queued => 0,
            Self::Extracting => 1,
            Self::Chunking => 2,
            Self::ExtractingEntities => 3,
            Self::ProcessingFigures => 4,
            Self::Embedding => 5,
            Self::Storing => 6,
            Self::Done => 7,
        }
    }

    /// Relative cost of this step when it runs.
    const fn base_cost(self) -> f64 {
        match self {
            Self::Queued | Self::Done => 0.0,
            Self::Extracting => 45.0,
            Self::Chunking => 4.0,
            Self::ExtractingEntities => 22.0,
            Self::ProcessingFigures => 10.0,
            Self::Embedding => 12.0,
            Self::Storing => 7.0,
        }
    }
}

/// Which optional steps this job's policy runs. A skipped step carries zero
/// weight so the bar never reserves space for work that will not happen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressFlags {
    /// Whether the entity/relation pass runs for this job.
    pub entities: bool,
    /// Whether the figure-asset pass runs for this job.
    pub figures: bool,
}

/// A step plus its in-step position. `current`/`total` are absent when the
/// work is indeterminate.
#[derive(Debug, Clone, Copy)]
pub struct StepProgress {
    /// The step being reported.
    pub step: IngestStep,
    /// In-step position, e.g. page 12 of 210. `None` when indeterminate.
    pub current: Option<u32>,
    /// In-step denominator. `None` when indeterminate.
    pub total: Option<u32>,
}

impl StepProgress {
    /// A step whose position is not tracked.
    #[must_use]
    pub const fn of(step: IngestStep) -> Self {
        Self {
            step,
            current: None,
            total: None,
        }
    }

    /// A step at `current` of `total`.
    #[must_use]
    pub const fn at(step: IngestStep, current: u32, total: u32) -> Self {
        Self {
            step,
            current: Some(current),
            total: Some(total),
        }
    }
}

const ORDER: [IngestStep; 8] = [
    IngestStep::Queued,
    IngestStep::Extracting,
    IngestStep::Chunking,
    IngestStep::ExtractingEntities,
    IngestStep::ProcessingFigures,
    IngestStep::Embedding,
    IngestStep::Storing,
    IngestStep::Done,
];

/// Step weights renormalised to 100 for the steps this job actually runs.
fn weights_for(flags: ProgressFlags) -> [f64; 8] {
    let mut costs = [0.0f64; 8];
    let mut sum = 0.0;
    for (i, step) in ORDER.iter().enumerate() {
        let mut cost = step.base_cost();
        if *step == IngestStep::ExtractingEntities && !flags.entities {
            cost = 0.0;
        }
        if *step == IngestStep::ProcessingFigures && !flags.figures {
            cost = 0.0;
        }
        costs[i] = cost;
        sum += cost;
    }
    if sum > 0.0 {
        for cost in &mut costs {
            *cost = *cost / sum * 100.0;
        }
    }
    costs
}

/// Overall 0–100 progress. Only `done` reports 100; every other step caps at 99
/// so a client never shows a job as complete before it is.
#[must_use]
pub fn compute_overall_progress(progress: StepProgress, flags: ProgressFlags) -> u32 {
    if progress.step == IngestStep::Done {
        return 100;
    }
    let weights = weights_for(flags);
    let idx = progress.step.position();

    let completed: f64 = weights[..idx].iter().sum();
    let fraction = match (progress.current, progress.total) {
        (Some(current), Some(total)) if total > 0 => {
            (f64::from(current) / f64::from(total)).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };

    let value = completed + weights[idx] * fraction;
    // Clamped first, so the truncation below cannot lose anything.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = value.round().clamp(0.0, 99.0) as u32;
    percent
}

/// Seconds remaining from elapsed time and progress. Re-derived on every
/// update, so it self-corrects. `None` while progress is too low to mean
/// anything or the job has not started.
#[must_use]
pub fn compute_eta_seconds(
    progress: u32,
    started_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<i32> {
    let started = started_at?;
    if progress <= 5 || progress >= 100 {
        return None;
    }
    let elapsed = now.saturating_duration_since(started).as_secs_f64();
    if elapsed <= 0.0 {
        return None;
    }
    let remaining = (elapsed * f64::from(100 - progress)) / f64::from(progress);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let seconds = remaining.round().max(1.0) as i32;
    Some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const BOTH: ProgressFlags = ProgressFlags {
        entities: true,
        figures: true,
    };
    const NEITHER: ProgressFlags = ProgressFlags {
        entities: false,
        figures: false,
    };

    #[test]
    fn done_is_the_only_hundred() {
        assert_eq!(
            compute_overall_progress(StepProgress::of(IngestStep::Done), BOTH),
            100
        );
        let before = compute_overall_progress(StepProgress::at(IngestStep::Storing, 10, 10), BOTH);
        assert!(before <= 99, "{before}");
    }

    #[test]
    fn progress_is_monotonic_across_steps() {
        let steps = [
            IngestStep::Extracting,
            IngestStep::Chunking,
            IngestStep::ExtractingEntities,
            IngestStep::ProcessingFigures,
            IngestStep::Embedding,
            IngestStep::Storing,
        ];
        let values: Vec<u32> = steps
            .iter()
            .map(|s| compute_overall_progress(StepProgress::of(*s), BOTH))
            .collect();
        for pair in values.windows(2) {
            assert!(pair[1] >= pair[0], "{values:?}");
        }
        assert_eq!(values[0], 0, "an indeterminate step holds at its start");
        assert!(values[5] < 100, "{values:?}");
    }

    #[test]
    fn skipped_steps_do_not_reserve_progress() {
        // With no entities and no figures the bar must not sit still during
        // two steps that never run: chunking should already be past a third.
        let with_optional =
            compute_overall_progress(StepProgress::of(IngestStep::ExtractingEntities), BOTH);
        let without_optional =
            compute_overall_progress(StepProgress::of(IngestStep::ExtractingEntities), NEITHER);
        assert!(without_optional > with_optional);
    }

    #[test]
    fn in_step_counters_fill_the_slice() {
        let start = compute_overall_progress(StepProgress::of(IngestStep::Embedding), BOTH);
        let end = compute_overall_progress(StepProgress::at(IngestStep::Embedding, 10, 10), BOTH);
        assert!(end > start, "{start} -> {end}");
        // A total of zero is indeterminate, not infinite progress.
        let zero = compute_overall_progress(StepProgress::at(IngestStep::Embedding, 5, 0), BOTH);
        assert_eq!(zero, start);
    }

    #[test]
    fn eta_needs_a_started_job_and_enough_progress() {
        let now = Instant::now();
        assert_eq!(compute_eta_seconds(50, None, now), None);
        assert_eq!(compute_eta_seconds(2, Some(now), now), None);
        assert_eq!(compute_eta_seconds(100, Some(now), now), None);

        let started = now - Duration::from_secs(60);
        let eta = compute_eta_seconds(50, Some(started), now).unwrap_or_default();
        assert_eq!(eta, 60, "halfway through 60s leaves 60s");
    }
}
