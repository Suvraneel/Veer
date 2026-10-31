**Radlet is a weapon wheel for your files.** Drag files in Finder, hold a key, and flick to a tool: convert, compress, crop, translate and dozens more, all on your Mac.

**New in 0.4.0:**

- **Developer tools.** Format or minify JSON (with sorted keys if you like) and XML, optimize SVGs, check JSON, YAML, TOML, XML, SVG, CSV and TSV files ("Valid YAML", or the line and column of the first problem), copy a picture, SVG or font as a data URI, encode and decode Base64, and copy MD5, SHA-1 or SHA-512 checksums as well as SHA-256.
- **Wrong extensions, spotted.** When a file's name doesn't match what's inside (a .png that's really a JPEG, an iPhone photo named .jpg that's HEIC, a download with no extension), **Rename to .jpg** comes first on the wheel. **Rename…** can also change extensions on purpose, with a reminder that renaming doesn't convert.
- **FFmpeg without Homebrew.** When a job needs FFmpeg and Homebrew isn't installed, Radlet downloads its own build (about 55 MB, LGPL), checked before it's used.
- **On the command line and for AI agents:** `radlet check` (which exits with an error when a file has a problem, for scripts and CI), `xml`, `optimize-svg`, `data-uri`, `base64`, `fix-extension`, `json --sort-keys` and `checksum --algorithm`, and the new tools over MCP.

**Install:** download the `.dmg` below, open it and drag **Radlet** into **Applications**. Radlet runs on macOS 14 or later, on Apple silicon and Intel Macs.

<!-- not-notarized -->
Radlet isn't notarized by Apple yet, so macOS blocks it the first time you open it:

- **macOS 15 or later:** click **Done**, then open **System Settings → Privacy & Security**, find "Radlet was blocked to protect your Mac", click **Open Anyway** and confirm with Touch ID or your password.
- **macOS 14:** Control-click Radlet in Applications, choose **Open**, then click **Open** again.
<!-- /not-notarized -->

Everyday audio and video (MP4, MOV, M4A, MP3, WAV, AIFF, FLAC) work as they are, including joining clips, fitting videos under a size and burning in subtitles. Conversions that need FFmpeg (MP3 output, WebM, MKV, making GIFs, evening out volume, making subtitles) show a small badge on the wheel: choose one and Radlet offers to install FFmpeg: with Homebrew if you have it, otherwise by downloading Radlet's own build (about 55 MB, LGPL, checked before it's used).

**Getting started:** Radlet lives in the menu bar. The first time, a short setup guide lets you pick how the wheel looks, choose where results go and practise on some sample photos. After that, start dragging files in Finder and hold **Shift** to convert, **Command** for tools or **Control** for your loadouts. No drag needed: select files in Finder and press **⌃⌥V**. Radlet checks for updates once a day and asks before installing one. The README has the full guide.
