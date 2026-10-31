**Radlet is a weapon wheel for your files.** Drag files in Finder, hold a key, and flick to a tool: convert, compress, crop, translate and dozens more, all on your Mac.

**New in 0.3.0:**

- **SVG drawings.** Convert any picture to SVG: it's traced into shapes that stay sharp at any size, in colour, black and white, Simple or Detailed.
- **Sharing.** Share a loadout or a skin as a file or a `radlet://` link, and add ready-made ones from the [gallery](https://radlet.vercel.app/gallery/).
- **Your shortcuts on the wheel.** Pick shortcuts from the Shortcuts app in Settings → Loadouts, and run them from the Control wheel.
- **Apple Intelligence, on this Mac** (macOS 26): Describe a picture as alt text, Summarize a document, rename files by what's in them, and make a loadout from a sentence.
- **A tidier Convert wheel.** Each kind of file shows the formats most people want; less known ones wait under More….
- **Bloom keeps its presets out** for the whole drag, and overshooting one still lands on the wheel.
- **AI agents** get the rest of the wheel's tools over MCP: combining documents and workbooks, QR codes, icon sets, folder tools, names, descriptions and summaries.

**Install:** download the `.dmg` below, open it and drag **Radlet** into **Applications**. Radlet runs on macOS 14 or later, on Apple silicon and Intel Macs.

<!-- not-notarized -->
Radlet isn't notarized by Apple yet, so macOS blocks it the first time you open it:

- **macOS 15 or later:** click **Done**, then open **System Settings → Privacy & Security**, find "Radlet was blocked to protect your Mac", click **Open Anyway** and confirm with Touch ID or your password.
- **macOS 14:** Control-click Radlet in Applications, choose **Open**, then click **Open** again.
<!-- /not-notarized -->

Everyday audio and video (MP4, MOV, M4A, MP3, WAV, AIFF, FLAC) work as they are, including joining clips, fitting videos under a size and burning in subtitles. Conversions that need FFmpeg (MP3 output, WebM, MKV, making GIFs, evening out volume, making subtitles) show a small badge on the wheel: choose one and Radlet offers to install FFmpeg: with Homebrew if you have it, otherwise by downloading Radlet's own build (about 55 MB, LGPL, checked before it's used).

**Getting started:** Radlet lives in the menu bar. The first time, a short setup guide lets you pick how the wheel looks, choose where results go and practise on some sample photos. After that, start dragging files in Finder and hold **Shift** to convert, **Command** for tools or **Control** for your loadouts. No drag needed: select files in Finder and press **⌃⌥V**. Radlet checks for updates once a day and asks before installing one. The README has the full guide.
