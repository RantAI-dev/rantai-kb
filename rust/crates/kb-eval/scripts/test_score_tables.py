#!/usr/bin/env python3
"""Unit tests for `score_tables.py` and `import_tables.py` (TBL-1, work package C).

    python3 -m unittest test_score_tables -v

The two things that can silently make a table eval meaningless are covered
here: comparing an answer that arrived as prose ("Rp 1.234,5 juta", "49,82%")
and building a `TableQuery` that matches the contract in
`kb_core::tables`.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import import_tables as imp  # noqa: E402
import score_tables as score  # noqa: E402


class NumberTest(unittest.TestCase):
    """Indonesian and English number formats, as a model would write them."""

    def test_plain_integers_and_decimals(self):
        self.assertEqual(score.parse_number("5730"), 5730.0)
        self.assertEqual(score.parse_number("49.82"), 49.82)
        self.assertEqual(score.parse_number(1_277_455), 1277455.0)
        self.assertEqual(score.parse_number("1,277,455"), 1277455.0)

    def test_indonesian_grouping_and_decimal_comma(self):
        self.assertEqual(score.parse_number("1.234.567"), 1234567.0)
        self.assertEqual(score.parse_number("1.234,5"), 1234.5)
        self.assertEqual(score.parse_number("Rp 1.234,5"), 1234.5)
        self.assertEqual(score.parse_number("Rp 1.234,5 juta"), 1_234_500_000.0)
        self.assertEqual(score.parse_number("661.665,87 M"), None)  # M alone is not a unit here
        self.assertEqual(score.parse_number("2,5 miliar"), 2_500_000_000.0)

    def test_percent(self):
        self.assertEqual(score.parse_number("49,82%"), 49.82)
        self.assertEqual(score.parse_number("49.82%"), 49.82)
        self.assertEqual(score.parse_number("100%"), 100.0)

    def test_currency_and_multipliers(self):
        self.assertEqual(score.parse_number("US$ 1,2 trillion"), 1_200_000_000_000.0)
        self.assertEqual(score.parse_number("$1.5 billion"), 1_500_000_000.0)
        self.assertEqual(score.parse_number("12 ribu"), 12_000.0)
        self.assertEqual(score.parse_number("(1.234,5)"), -1234.5)

    def test_not_numbers(self):
        for text in ("", None, "-", "N/A", "Kec. Prambanan", "Truk", True):
            self.assertIsNone(score.parse_number(text), text)

    def test_numbers_in_prose(self):
        self.assertEqual(score.numbers_in("sekitar 1.234,5 juta jiwa"), [1_234_500_000.0])
        self.assertEqual(score.numbers_in("49,82% dari total"), [49.82])
        self.assertEqual(score.numbers_in("no number here"), [])


class AnswersMatchTest(unittest.TestCase):
    def test_within_tolerance(self):
        self.assertTrue(score.answers_match(5730, 5730, 1e-6))
        self.assertTrue(score.answers_match(5730.0, 5730.000001, 1e-6))
        self.assertFalse(score.answers_match(5730, 5730.5, 1e-6))
        self.assertTrue(score.answers_match(0, 0, 0.0))
        self.assertFalse(score.answers_match(0, 1, 0.0))

    def test_integer_answers_from_json(self):
        self.assertTrue(score.answers_match(1277455, 1277455.0, 1e-6))
        self.assertTrue(score.answers_match(1277455, "1277455", 1e-6))
        self.assertTrue(score.answers_match(1277455, "1.277.455", 1e-6))

    def test_prose_answer_matches_number(self):
        self.assertTrue(score.answers_match(1_234_500_000, "Rp 1.234,5 juta", 1e-6))
        self.assertTrue(score.answers_match(49.82, "sekitar 49,82%", 1e-6))
        self.assertFalse(score.answers_match(49.82, "50,1%", 1e-6))

    def test_strings_are_normalised(self):
        self.assertTrue(score.answers_match("Kec. Prambanan", " kec. prambanan ", 0))
        self.assertTrue(score.answers_match("Prambanan", "Prambanan.", 0))
        self.assertFalse(score.answers_match("Prambanan", "Gantiwarno", 0))

    def test_group_max_dict_against_api_rows(self):
        gold = {"label": "Trucuk", "value": 365}
        self.assertTrue(score.answers_match(gold, [["Trucuk", 365]], 1e-6))
        self.assertTrue(score.answers_match(gold, [["Trucuk", 365.0]], 1e-6))
        self.assertTrue(score.answers_match(gold, {"label": "Trucuk", "value": 365}, 1e-6))
        self.assertFalse(score.answers_match(gold, [["Wedi", 430]], 1e-6))
        self.assertFalse(score.answers_match(gold, [["Trucuk", 364]], 1e-6))

    def test_top_k_list(self):
        gold = [{"label": "Trucuk", "value": 365}, {"label": "Jatinom", "value": 326}]
        got = [["Trucuk", 365], ["Jatinom", 326]]
        self.assertTrue(score.answers_match(gold, got, 1e-6))
        self.assertFalse(score.answers_match(gold, got[::-1], 1e-6))
        self.assertFalse(score.answers_match(gold, got[:1], 1e-6))

    def test_answer_of_collapses_one_cell(self):
        self.assertEqual(score.answer_of({"columns": ["total"], "rows": [[5730]]}), 5730)
        self.assertEqual(
            score.answer_of({"columns": ["Kecamatan", "n"], "rows": [["Trucuk", 365]]}),
            [["Trucuk", 365]],
        )

    def test_text_matches_for_the_agent(self):
        self.assertTrue(score.text_matches(5730, "Totalnya 5.730 kelahiran.", 1e-6))
        self.assertTrue(score.text_matches(1_277_455, "sekitar 1,28 juta jiwa", 1e-2))
        self.assertFalse(score.text_matches(5730, "Saya tidak tahu.", 1e-6))
        self.assertTrue(score.text_matches("Trucuk", "Kecamatan Trucuk.", 0))
        self.assertTrue(
            score.text_matches(
                {"label": "Trucuk", "value": 365}, "Trucuk dengan 365 kelahiran", 1e-6
            )
        )


class TableQueryTest(unittest.TestCase):
    """The JSON the generator writes is the contract in `kb_core::tables`."""

    def test_select_shapes(self):
        self.assertEqual(imp._select_column("Kecamatan"), {"column": "Kecamatan"})
        self.assertEqual(
            imp._select_agg("sum", "Jumlah", "total"),
            {"agg": "sum", "column": "Jumlah", "as": "total"},
        )
        self.assertEqual(imp._select_agg("count", None, "n"), {"agg": "count", "as": "n"})
        self.assertEqual(
            imp._select_agg("count_distinct", "Kode Code", "n"),
            {"agg": "count_distinct", "column": "Kode Code", "as": "n"},
        )

    def test_empty_members_are_omitted(self):
        query = imp._q(
            {
                "select": [imp._select_agg("sum", "2024", "total")],
                "filter": [],
                "group_by": [],
                "order_by": [],
                "limit": None,
            }
        )
        self.assertEqual(query, {"select": [{"agg": "sum", "column": "2024", "as": "total"}]})
        self.assertNotIn("filter", query)
        self.assertNotIn("include_derived", query)

    def test_include_derived_is_only_written_when_true(self):
        self.assertNotIn("include_derived", imp._q({"include_derived": False}))
        self.assertTrue(imp._q({"include_derived": True})["include_derived"])

    def test_contract_tokens(self):
        query = imp._q(
            {
                "select": [imp._select_agg("avg", "Jumlah", "rata")],
                "filter": [
                    {"column": "Kecamatan", "op": "contains", "value": "Klaten"},
                    {"column": "Tahun", "op": "between", "value": [2019, 2022]},
                ],
                "group_by": ["Kecamatan"],
                "order_by": [{"key": "rata", "dir": "desc"}],
                "limit": 3,
            }
        )
        # The contract's op tokens, not SQL.
        self.assertEqual([f["op"] for f in query["filter"]], ["contains", "between"])
        self.assertEqual(query["order_by"][0]["dir"], "desc")
        self.assertEqual(query["limit"], 3)
        self.assertNotIn("sql", json.dumps(query).lower())

    def test_reference_executor_over_a_small_grid(self):
        grid = {
            "header": ["Kecamatan", "Jumlah"],
            "rows": [["Wedi", 10], ["Bayat", 30], ["Jumlah", 40]],
            "derived_rows": [2],
        }
        result = imp.execute(grid, {"select": [imp._select_agg("sum", "Jumlah", "total")]})
        self.assertEqual(result["rows"], [[40]])
        self.assertEqual(result["rows_matched"], 2)
        self.assertEqual(result["excluded_derived_rows"], 1)

        including = imp.execute(
            grid,
            {"select": [imp._select_agg("sum", "Jumlah", "total")], "include_derived": True},
        )
        self.assertEqual(including["rows"], [[80]])

        grouped = imp.execute(
            grid,
            {
                "select": [imp._select_column("Kecamatan"), imp._select_agg("max", "Jumlah", "m")],
                "group_by": ["Kecamatan"],
                "order_by": [{"key": "m", "dir": "desc"}],
                "limit": 1,
            },
        )
        self.assertEqual(grouped["rows"], [["Bayat", 30]])

    def test_remap_query_onto_manifest_names(self):
        query = {
            "select": [{"agg": "sum", "column": "jumlah kelahiran / laki-laki", "as": "total"}],
            "filter": [{"column": "kecamatan", "op": "contains", "value": "Klaten"}],
        }
        names = ["JUMLAH KELAHIRAN / LAKI-LAKI", "KECAMATAN"]
        remapped = score._remap_query(query, names)
        self.assertEqual(remapped["select"][0]["column"], "JUMLAH KELAHIRAN / LAKI-LAKI")
        self.assertEqual(remapped["filter"][0]["column"], "KECAMATAN")
        # An already-correct query is left alone (None means "no drift").
        self.assertIsNone(score._remap_query({"select": [{"column": "KECAMATAN"}]}, names))


class HitShapesTest(unittest.TestCase):
    """The harness must read the tableId wherever the API puts it."""

    def test_chunk_metadata(self):
        hits = {
            "chunks": [
                {"content": "a", "tableId": "doc_t0"},
                {"content": "b", "metadata": {"tableId": "doc_t1"}},
                {"content": "c", "metadata": {"table_id": "doc_t0"}},
            ]
        }
        self.assertEqual(score.table_ids_of(hits), ["doc_t0", "doc_t1"])

    def test_top_level_tables(self):
        hits = {"tables": [{"tableId": "doc_t2"}, {"id": "doc_t3"}], "chunks": []}
        self.assertEqual(score.table_ids_of(hits), ["doc_t2", "doc_t3"])

    def test_no_tables(self):
        self.assertEqual(score.table_ids_of({"chunks": [{"content": "text only"}]}), [])

    def test_json_block(self):
        plan = score.json_block('```json\n{"tableId": "x_t0", "query": {"select": []}}\n```')
        self.assertEqual(plan["tableId"], "x_t0")
        self.assertEqual(
            score.json_block('Here it is: {"tableId": "y_t1", "query": {"select": []}} done')["tableId"],
            "y_t1",
        )
        self.assertIsNone(score.json_block("no json"))


class DatasetShapeTest(unittest.TestCase):
    """If a built dataset is next to the script, check the contract fields."""

    def test_query_records_if_built(self):
        dataset = Path.home() / "kbdata" / "tables-v1"
        if not (dataset / "queries.jsonl").exists():
            self.skipTest("no built dataset")
        records = [json.loads(line) for line in (dataset / "queries.jsonl").read_text().splitlines() if line]
        self.assertGreaterEqual(len(records), 300)
        required = {"id", "lang", "question", "file", "sheet_or_index", "gold_query", "gold_answer", "tolerance", "template"}
        for record in records:
            self.assertTrue(required <= set(record), (record["id"], required - set(record)))
            self.assertIn(record["lang"], ("id", "en"))
            self.assertIn(record["template"], imp.TEMPLATES)
            self.assertIsInstance(record["gold_query"], dict)
            self.assertIn("select", record["gold_query"])


if __name__ == "__main__":
    unittest.main()
