# Format Converter

Lets Nora's agent convert files between formats: documents, images, audio and video.

## What it can do

| Files | Tool | Examples |
| --- | --- | --- |
| Documents | pandoc | Markdown, HTML, DOCX, LaTeX, PDF |
| Images | Python (Pillow) | PNG, JPEG, WebP, GIF, TIFF, ICO |
| Audio and video | ffmpeg | MP4, MOV, WebM, MP3, WAV, GIF |

## Try asking

- "Convert this Markdown file to a PDF."
- "Turn these PNGs into WebP at 80% quality."
- "Extract the audio of this video as an MP3."

## Requirements

The agent uses pandoc (with WeasyPrint for PDFs), Pillow and ffmpeg, and installs them when they're missing. Conversions run in Nora's local VM unless you ask for another environment.
