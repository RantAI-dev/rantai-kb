# Legacy-format fixtures

Used by `tests/legacy_formats.rs`.

| File | Made by | Content |
|---|---|---|
| `legacy-sample.doc` | LibreOffice 24.2 Writer, "MS Word 97" filter, from an HTML page written for this test | heading, Windows-1252 punctuation, a table |
| `legacy-sample.ppt` | python-pptx 1.0.2 deck → LibreOffice 7.4 Impress, "MS PowerPoint 97" filter | two slides, a free text box, a speaker note |
| `table.xls` | CSV → LibreOffice 7.4 Calc, "MS Excel 97" filter | one sheet, three rows |
| `fax-group3.tif`, `fax-group4.tif` | Pillow 12, two 1-bit pages, CCITT T.4 (1-D) and T.6 | `fax-page-N.png` are the pages as drawn |
| `poi-Word6.doc`, `poi-Word95.doc` | Apache POI `test-data/document/Word6.doc`, `Word95.doc` (Apache-2.0) | real Word 6.0 and Word 95 files |
| `poi-fuzz-1.ppt`, `poi-fuzz-2.doc` | Apache POI clusterfuzz cases (Apache-2.0) | malformed compound files: must fail with a reason, not panic |
