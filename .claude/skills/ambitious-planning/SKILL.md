---
name: ambitious-planning
description: Plan a non-trivial change to rantai-kb (feature, port step, retrieval/ingest improvement, runner brief) at full ambition — find the goal behind the goal, a hard measurable target, expert methods, raw data, and outside validation. Use before writing a plan, a design doc, or a prompt for a delegated runner agent.
---

# Ambitious planning

Based on Felix Rieseberg, "How to Be More Ambitious: A Tutorial for Humans with Models"
(2026-10-13). Speed (same work, faster) is the default and needs no help. This skill is about
the other three kinds of ambition: **altitude**, **depth** and **range**.

Work through the five steps, then write the plan in the output format at the bottom. Keep each
answer short and concrete; this is a thinking checklist, not an essay.

## 1. Altitude: find the goal behind the goal

- What is literally being asked?
- What should happen *after* it is done? Who uses the result, and for what decision or action?
- Go up one more level: what is the goal beyond that?
- Is there a version of the work that removes a recurring human step entirely (a harness, a CI
  job, a scheduled check) instead of doing the task once?

Pick the highest level you can still deliver and verify in this session. If the literal task is
the wrong one for that goal, say so and propose the better one; don't silently do either.

Example: "add a reranker" → "users get the right page first" → "we can prove retrieval quality
before every change". The ambitious plan adds an eval set and a CI metric *first*, then the
reranker, and reports the delta.

## 2. Depth: do what the best practitioner would do

Answer these four questions before planning (adapted from the article's depth prompt):

1. **What would a top professional in this field do here that wasn't asked for?**
2. **What is the rawest material they would start from**, instead of what has already been
   digested? (Real PDFs, not a description of them. Raw SurrealDB rows, not the API summary.
   Profiler output, not "it feels slow". Production queries, not invented ones.)
3. **Which specific methods and instruments would they use?** Real tools, not markdown
   tables: recall@k / MRR / nDCG on a labelled set, `cargo bench`/criterion, flamegraphs,
   `EXPLAIN`, property tests, fuzzing, load tests, diff harnesses.
4. **What, other than the model or the requester, would show the result is right?**

Watch for vocabulary without method: sounding like an expert while doing the same shallow work.
If the plan has no instrument and no external check, it is not deep yet.

## 3. Set a specific, hard target

Replace "improve" with a number and a deadline-shaped constraint. Choose a target that feels a
little unreasonable (the 200 ms when 5 s seemed ambitious), then plan as if it is solvable.

- Good: "right page in top 1 for ≥ 85% of the 100 labelled questions", "ingest a 300-page PDF
  in < 90 s", "zero cross-tenant hits across the parity matrix", "p95 search < 300 ms".
- Bad: "better retrieval", "faster ingest", "more robust".

Record the current baseline first. A target without a baseline can't be claimed.

## 4. Challenge the assumptions adversarially

List the 1–3 beliefs the plan depends on ("BM25 hurts more than it helps", "the reranker is the
bottleneck", "Rust matches Bun"). For each, plan a cheap test that could *disprove* it from raw
data before building on it. Once deep in the data, ask: what signal is here that we didn't know
to look for? Write down blind spots found.

## 5. Range: borrow methods from other fields

Rewrite the task's noun as a verb phrase. "A search API" → "getting the one paragraph that
answers a question to a model under a token budget, without leaking another tenant's data."
Then ask which fields own that verb and what *method* each contributes:

| Field | Method it lends |
|---|---|
| Information retrieval research | labelled qrels, recall@k, MRR, ablations |
| Security / red team | try to break isolation, abuse inputs, least privilege |
| SRE | SLOs, failure injection, graceful degradation, dashboards |
| Document engineering / typography | layout analysis, reading order, headers/footers, tables |
| Statistics | confidence intervals, enough samples to claim a delta |
| Product / UX research | what the end user actually sees and trusts |

Use the method, not the costume. "Do what a red-teamer does: write a test that tries to read KB-B
with a KB-A key" — not "act like a hacker".

## Output format

```markdown
## Goal
Literal ask → goal behind it → chosen level (and why not higher).

## Target
Baseline: <measured or "to measure in step 1">. Target: <number>. Stretch: <number>.

## Depth
Expert move not asked for · raw material · instruments · outside validator.

## Assumptions to break
1. <belief> — test: <how it could be disproved>

## Plan
Ordered steps. The first step creates or runs the measurement. Each step names its check and
is committed separately.

## Validation
The exact commands / harnesses / labelled sets that prove it. No self-grading.

## Out of scope / risks
What we deliberately skip, and what could make the target unreachable.
```

## When briefing a runner agent

A delegated runner gets the same sections, plus:

- The goal behind the goal in one sentence, so it can make judgment calls.
- The hard target and the exact command that measures it.
- "Paste raw output for every claim; report unmeasured results as unmeasured."
- Commit-early rule and the files it may touch.
- What to do if the target is out of reach: stop at the best measured result and explain the
  gap. Partial-and-honest beats complete-and-invented.

Then verify the runner's claims yourself by re-running its checks.

## Guardrails

Ambition doesn't override the rules in `AGENTS.md`: tenant isolation, Rust↔Bun compatibility,
evidence-only reporting, and asking before outward-facing actions (push, PR, deploy). Bigger
goals need stronger verification, not weaker.
