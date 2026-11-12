# Docs

Lets Nora's agent read, create, edit and review Word documents (`.docx`).

## What it can do

- **Read** a document the way you see it: it renders the pages to images and looks at them, so tables, figures and layout aren't lost.
- **Create** new documents with `python-docx`, with consistent headings, lists, tables and styles.
- **Edit** existing documents, including tracked changes and comments.
- **Review** its own work: after each batch of edits it renders the document again and fixes anything that looks wrong before handing it back.

## Try asking

- "Summarize the attached contract and list its deadlines."
- "Turn these notes into a two-page project proposal as a Word document."
- "Suggest edits to this report as tracked changes."

## Requirements

The agent uses LibreOffice (`soffice`) and Poppler (`pdftoppm`) to render pages, and the Python package `python-docx`. It installs `python-docx` when it's missing.
