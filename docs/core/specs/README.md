# Specs, one per task

One file per backlog task. Each spec gives:

- why the task exists and what users get;
- target numbers, against the best competitor where one has a documented number;
- an acceptance checklist;
- what is left out.

A number marked *(proposed)* is the planner's; the product owner confirms it on the feature page.

A spec is not a plan. Before building, a Feature also needs a page in
[`../features/`](../features/). For retrieval work, the target is a measured metric on a
labelled set (`rust/crates/kb-eval/datasets/`), never an eyeballed query.

| ID | Spec | Module | Size |
| --- | --- | --- | --- |
| [`SEC-1`](sec-1.md) | A key limited to some knowledge bases cannot open other documents by ID | Access & Security | S |
| [`ING-5`](ing-5.md) | Images are searchable by what they show, standalone or inside documents | Ingest | L |
| [`RET-5`](ret-5.md) | Adaptive arm choice: skip the arms a query's class cannot use, name every skip | Retrieval | M |
