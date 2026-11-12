# PDFs

Lets Nora's agent read, create and review PDF files.

## What it can do

- **Read** PDFs by rendering their pages to images, with `pdfplumber` for text and tables, so figures and layout are kept.
- **Create** PDFs with `reportlab`: reports, handouts, forms and more, including Chinese, Japanese and Korean text.
- **Review** its own output page by page and fix clipped text, broken tables or missing glyphs before handing it back.

## Try asking

- "What are the key findings in this paper?"
- "Make a one-page PDF invoice from this table."
- "Check this PDF for layout problems."

## Requirements

The agent uses Poppler (`pdftoppm`) and the Python packages `pdfplumber` and `reportlab`. For CJK text it installs the Noto fonts first.
