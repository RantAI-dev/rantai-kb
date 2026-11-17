# Page retrieval bake-off

- datasets: vidore3-cs-en-pages, vidore3-hr-en-pages
- baseline: `qwen3-vl-embed-2b`

## Embedders, per page input mode

| model | mode | nDCG@10 [95% CI] | Recall@5 | Recall@10 | MRR@10 | Δ nDCG vs baseline | s/page | licence |
|---|---|---:|---:|---:|---:|---:|---:|---|

### vidore3-cs-en-pages

| model | mode | nDCG@10 [95% CI] | Recall@5 | Recall@10 | MRR@10 | Δ nDCG vs baseline |
|---|---|---:|---:|---:|---:|---:|

### vidore3-hr-en-pages

| model | mode | nDCG@10 [95% CI] | Recall@5 | Recall@10 | MRR@10 | Δ nDCG vs baseline |
|---|---|---:|---:|---:|---:|---:|

## FAILED

| model | error |
|---|---|
| `qwen3-vl-embed-2b` | endpoint DOWN: <urlopen error [Errno 111] Connection refused> |
| `nemotron-embed-vl-1b-v2` | endpoint DOWN: <urlopen error [Errno 111] Connection refused> |
| `nemotron-rerank-vl-1b-v2` | endpoint DOWN: <urlopen error [Errno 111] Connection refused> |
| `qwen3-vl-rerank-2b` | endpoint DOWN: <urlopen error [Errno 111] Connection refused> |
