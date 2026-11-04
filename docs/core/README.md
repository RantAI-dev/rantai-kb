# Core product documents

The product owner's view of rantai-kb. It is kept small on purpose, so it
stays current. The format follows RantAI Lakehouse's `docs/core/`.

| File | What it is | When you touch it |
| --- | --- | --- |
| [PRODUCT.md](PRODUCT.md) | The one product document: what it is, what it has and lacks, what blocks a release, what comes next, and the open decisions | When a feature is accepted; before a release |
| [BACKLOG.md](BACKLOG.md) | The master list of work: ID, kind, item, module, priority, status, PR and source | Weekly, and on every merge |
| [specs/](specs/) | One spec per backlog task: why, target numbers, acceptance checklist, and what is left out | When a task is added or its targets change |
| [features/](features/) | One page per feature: what the user can do, decisions, limits, acceptance checklist. Written only when a feature is built | When a feature starts and when it finishes |
| [HANDOFF.md](HANDOFF.md) | The current state for a planner or reviewer taking over: what is merged, what is in flight, and what to watch for | When the planner role changes hands |

## How a feature moves

1. It is in `BACKLOG.md` with Kind **Feature**, a Module, a Priority and
   Status **Planned**.
2. It gets a page in `features/` (copy `_TEMPLATE.md`). The planner agent
   may draft the page. The product owner answers its decisions and names the
   Acceptor.
3. The agents plan, build, review and merge it (`AGENTS.md`). On merge its
   Status becomes **In Acceptance**, with the PR recorded.
4. The product owner runs the acceptance checklist on that page.
5. `PRODUCT.md` and `BACKLOG.md` are updated, and the Status becomes
   **Released**.

Merged is not accepted. A feature counts as done at step 4.

**People.** The repository is public and open source. Owner, Acceptor and
Assignee names are not written here; they are held outside the repo (where
is decision `DEC-2`).

## Rules

- **Nothing invented.**
  - A status is Have, Partial, Missing or *Not verified*. There are no made-up
    numbers or dates.
  - A retrieval number is one we ran, with the folder it came from under
    `docs/eval-ab/` or `docs/eval-*`.
  - A target either cites a competitor's documented number or is marked
    *(proposed)* until the product owner signs it on the feature page.
- **Say where a claim comes from.**
- **No customer names, hostnames, keys or secrets.**
- **Engineering detail stays elsewhere.** It lives in `docs/architecture-target.md`,
  `docs/rust-cutover.md`, `docs/kb-tiers.md`, `rust/README.md` and
  `rust/deploy/README.md`. Link to it; do not copy it.
