# Visual fixtures

Used by `tests/visuals.rs`. `make.py` rebuilds all of them (Pillow, python-pptx and
LibreOffice Writer); every picture is a synthetic colour block, no real content.

| File | Made by | What it checks |
|---|---|---|
| `deck.pptx` | python-pptx 1.0.2 | slide pages; alt text set on one picture; python-pptx's default `descr` (the file name) is not alt text |
| `report.docx`, `report.odt`, `report.pdf` | LibreOffice 24.2 Writer from one HTML page with two `data:` URI pictures | `wp:docPr descr`, `svg:title`, PDF image objects (Writer downsamples the 2400 px picture to 1250 px in the PDF) |
| `sheets.xlsx` | written by hand to the OOXML layout | sheet index from the workbook; an EMF picture counted as unsupported |
| `sheets.ods` | written by hand to the ODF layout | table index; `svg:title` + `svg:desc` |
| `book.epub` | written by hand to the EPUB 3 layout | spine order, relative `src`, entity in `alt`, remote image skipped |
| `big.png` | Pillow | a standalone upload shrunk to 1568 px |
