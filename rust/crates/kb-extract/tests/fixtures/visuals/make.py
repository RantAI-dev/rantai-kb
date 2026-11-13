"""Rebuild the visual fixtures. PYTHONPATH must provide Pillow and python-pptx;
`soffice` (LibreOffice Writer) must be on PATH. All content is synthetic.

    PYTHONPATH=/tmp/legacy-parity/py python3 make.py
"""

import io
import subprocess
import tempfile
import zipfile
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).parent
NS_R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG = "http://schemas.openxmlformats.org/package/2006/relationships"


def picture(w, h, color, label):
    im = Image.new("RGB", (w, h), color)
    ImageDraw.Draw(im).text((10, 10), label, fill=(255, 255, 255))
    out = io.BytesIO()
    im.save(out, "PNG")
    return out.getvalue()


RED = picture(800, 600, (200, 30, 30), "red")
BLUE = picture(2400, 1200, (30, 30, 200), "blue")  # longer than 1568: shrinks
GREEN = picture(300, 200, (30, 160, 30), "green")


def pptx():
    from pptx import Presentation
    from pptx.util import Inches

    prs = Presentation()
    s1 = prs.slides.add_slide(prs.slide_layouts[5])
    s1.shapes.title.text = "Slide one"
    pic = s1.shapes.add_picture(io.BytesIO(RED), Inches(1), Inches(2), width=Inches(4))
    pic._element.nvPicPr.cNvPr.set("descr", "A red square on slide one")
    s2 = prs.slides.add_slide(prs.slide_layouts[5])
    s2.shapes.title.text = "Slide two"
    s2.shapes.add_picture(io.BytesIO(BLUE), Inches(1), Inches(2), width=Inches(4))
    s2.shapes.add_picture(io.BytesIO(GREEN), Inches(5), Inches(2), width=Inches(2))
    prs.save(HERE / "deck.pptx")


def writer_exports():
    """LibreOffice Writer: the same HTML page as .docx, .odt and .pdf."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        import base64

        def uri(png):
            return "data:image/png;base64," + base64.b64encode(png).decode()

        # data: URIs, so Writer embeds the pictures instead of linking files.
        (tmp / "page.html").write_text(
            "<html><body><p>Report with two pictures.</p>"
            f'<img src="{uri(RED)}" alt="A red square in the report" width="300">'
            f'<p>Between the pictures.</p><img src="{uri(BLUE)}" width="400"></body></html>'
        )
        for target in ("docx:MS Word 2007 XML", "odt", "pdf:writer_web_pdf_Export"):
            subprocess.run(
                ["soffice", "--headless", "--convert-to", target, "--outdir", str(tmp), str(tmp / "page.html")],
                check=True,
                capture_output=True,
                timeout=300,
            )
        for ext in ("docx", "odt", "pdf"):
            (HERE / f"report.{ext}").write_bytes((tmp / f"page.{ext}").read_bytes())


def epub():
    with zipfile.ZipFile(HERE / "book.epub", "w") as z:
        z.writestr("mimetype", "application/epub+zip")
        z.writestr(
            "META-INF/container.xml",
            '<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">'
            '<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>',
        )
        z.writestr(
            "OEBPS/content.opf",
            '<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><manifest>'
            '<item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>'
            '<item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>'
            '</manifest><spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>',
        )
        z.writestr(
            "OEBPS/text/ch1.xhtml",
            '<html><body><p>Chapter one</p><img src="../images/red.png" alt="Red &amp; bold cover"/></body></html>',
        )
        z.writestr(
            "OEBPS/text/ch2.xhtml",
            "<html><body><p>Chapter two<img src='../images/green.png'>"
            '<img src="http://example.org/remote.png"></p></body></html>',
        )
        z.writestr("OEBPS/images/red.png", RED)
        z.writestr("OEBPS/images/green.png", GREEN)


def xlsx():
    ct = (
        '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>'
        '<Default Extension="emf" ContentType="image/x-emf"/></Types>'
    )
    sheet = (
        '<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'xmlns:r="{NS_R}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row></sheetData>%s</worksheet>'
    )
    pic = (
        "<xdr:twoCellAnchor><xdr:pic><xdr:nvPicPr>%s<xdr:cNvPicPr/></xdr:nvPicPr>"
        '<xdr:blipFill><a:blip r:embed="%s"/></xdr:blipFill></xdr:pic></xdr:twoCellAnchor>'
    )
    with zipfile.ZipFile(HERE / "sheets.xlsx", "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", ct)
        z.writestr("_rels/.rels", f'<?xml version="1.0"?><Relationships xmlns="{NS_PKG}"><Relationship Id="rId1" Type="{NS_R}/officeDocument" Target="xl/workbook.xml"/></Relationships>')
        z.writestr(
            "xl/workbook.xml",
            f'<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{NS_R}">'
            '<sheets><sheet name="Data" sheetId="1" r:id="rId1"/><sheet name="Photos" sheetId="2" r:id="rId2"/></sheets></workbook>',
        )
        z.writestr(
            "xl/_rels/workbook.xml.rels",
            f'<?xml version="1.0"?><Relationships xmlns="{NS_PKG}">'
            f'<Relationship Id="rId1" Type="{NS_R}/worksheet" Target="worksheets/sheet1.xml"/>'
            f'<Relationship Id="rId2" Type="{NS_R}/worksheet" Target="worksheets/sheet2.xml"/></Relationships>',
        )
        z.writestr("xl/worksheets/sheet1.xml", sheet % "")
        z.writestr("xl/worksheets/sheet2.xml", sheet % '<drawing r:id="rId1"/>')
        z.writestr(
            "xl/worksheets/_rels/sheet2.xml.rels",
            f'<?xml version="1.0"?><Relationships xmlns="{NS_PKG}"><Relationship Id="rId1" Type="{NS_R}/drawing" Target="../drawings/drawing1.xml"/></Relationships>',
        )
        z.writestr(
            "xl/drawings/drawing1.xml",
            '<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" '
            f'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="{NS_R}">'
            + pic % ('<xdr:cNvPr id="2" name="Picture 1" descr="Green field photo"/>', "rId1")
            + pic % ('<xdr:cNvPr id="3" name="Logo"/>', "rId2")
            + "</xdr:wsDr>",
        )
        z.writestr(
            "xl/drawings/_rels/drawing1.xml.rels",
            f'<?xml version="1.0"?><Relationships xmlns="{NS_PKG}">'
            f'<Relationship Id="rId1" Type="{NS_R}/image" Target="../media/image1.png"/>'
            f'<Relationship Id="rId2" Type="{NS_R}/image" Target="../media/image2.emf"/></Relationships>',
        )
        z.writestr("xl/media/image1.png", GREEN)
        z.writestr("xl/media/image2.emf", b"\x01\x00\x00\x00 not decoded")


def ods():
    with zipfile.ZipFile(HERE / "sheets.ods", "w") as z:
        z.writestr("mimetype", "application/vnd.oasis.opendocument.spreadsheet")
        z.writestr(
            "content.xml",
            '<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
            'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
            'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink">'
            '<office:body><office:spreadsheet><table:table table:name="First"><table:table-row><table:table-cell/></table:table-row></table:table>'
            '<table:table table:name="Second"><table:shapes><draw:frame draw:name="Image 1"><draw:image xlink:href="Pictures/red.png"/>'
            "<svg:title>Red tile</svg:title><svg:desc>A red tile on the second sheet</svg:desc></draw:frame></table:shapes></table:table>"
            "</office:spreadsheet></office:body></office:document-content>",
        )
        z.writestr("Pictures/red.png", RED)


pptx()
writer_exports()
epub()
xlsx()
ods()
(HERE / "big.png").write_bytes(BLUE)
print("ok")
