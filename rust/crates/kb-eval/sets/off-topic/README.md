# Off-topic questions (RET-2)

Two sets of questions that are expected to have **no answer** in the corpora the
relevance floor is calibrated against. They exist so "returns nothing" is a
measurable outcome instead of a hunch.

| File | Corpus it must not be answerable from | Count |
|---|---|---|
| `miracl-id.tsv` | MIRACL-id (Indonesian Wikipedia passages) | 55 |
| `vidore-en.tsv` | ViDoRe v3 HR and CS (HR/EU employment documents, CS textbook/course documents) | 55 |

Format: `qid<TAB>query`, one question per line, no header. The qid is stable so
a run's per-query output can be joined back to it.

## How they were made

**Author-written, not externally validated.** There is no annotator pool and no
second pass. Each question was written by hand against these rules, and the
first person to run a calibration should treat the labels as a hypothesis to
check (spot-check the top hit for any query that survives the floor), not as
ground truth.

- Indonesian questions avoid general knowledge that Indonesian Wikipedia is
  likely to contain (people, places, history, biology, popular culture). They
  are personal, hypothetical, product-support, arithmetic or small-business
  questions: "berapa harga tiket bioskop saya minggu lalu", "bagaimana cara
  reset router Acme X200 di rumah saya".
- English questions avoid HR, employment, labour law and computer science,
  because ViDoRe v3's HR and CS collections cover those. They use cooking,
  sports scores, gardening, celebrity, household and personal questions:
  "how long should I roast a two kilogram chicken", "who won the neighborhood
  baking contest this year".
- A few questions deliberately contain identifier-like tokens (`X200`,
  `17 kali 23`). They are still off-topic: the token does not appear in the
  corpus, so the keyword arm has no exact match and the floor's identifier
  exemption must **not** fire on them.
- No question names a document, a passage or a factual entity that would make a
  web search a shortcut to the answer. The point is retrieval behaviour, not
  world knowledge.

## Known weakness

Wikipedia is broad and the web is broader. A question that looks personal can
still share vocabulary with a passage ("harga tiket", "reset", "score"), which
is exactly what the floor is supposed to resist. If the off-topic empty rate is
low, inspect the surviving queries before widening the set: a survivor may be a
lexical coincidence the floor is right to reject at a higher threshold, or a
genuine corpus answer that means the question belongs in a different set.

## Calibration

`rust/crates/kb-eval/scripts/floor_calibrate.py` takes per-query JSON search
responses for these sets and for an in-domain set with qrels, sweeps thresholds,
and reports the off-topic empty rate against in-domain nDCG@10. See the script's
`--help`.
