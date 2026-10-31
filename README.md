<div align="center">

<a href="https://cdn.jsdelivr.net/gh/Suvraneel/radlet@main/.github/media/demo.mp4"><img src=".github/media/demo.gif" alt="Radlet in action: three HEIC photos are dragged in Finder, Shift is held, a wheel of formats opens under the pointer, and dropping on JPEG puts three JPEGs next to the originals" width="100%"></a>

<h1>Radlet</h1>

<p><strong>A weapon wheel for your files.</strong><br>
Drag files, hold a key, flick to a tool. Convert, compress, crop, translate and dozens more, without leaving Finder. A free file converter and toolkit for macOS that lives in the menu bar and does all the work on your Mac.</p>

<p>
  <a href="https://cdn.jsdelivr.net/gh/Suvraneel/radlet@main/.github/media/demo.mp4"><strong>▶ Watch the 20-second demo</strong></a> (with sound)
  &nbsp;·&nbsp;
  <a href="https://radlet.vercel.app"><strong>Website</strong></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/Suvraneel/radlet/releases/latest"><strong>Download</strong></a>
</p>

<p>
  <img alt="macOS 14+" src="https://img.shields.io/badge/macOS-14%2B-111111?logo=apple&logoColor=white">
  <img alt="Swift 5.10" src="https://img.shields.io/badge/Swift-5.10-F05138?logo=swift&logoColor=white">
  <img alt="SwiftUI and AppKit" src="https://img.shields.io/badge/SwiftUI%20%2B%20AppKit-0A84FF?logo=apple&logoColor=white">
  <img alt="Rust 1.88+" src="https://img.shields.io/badge/Rust-1.88%2B-B7410E?logo=rust&logoColor=white">
  <img alt="Model Context Protocol server" src="https://img.shields.io/badge/MCP-server-6E56CF">
</p>

</div>

## Why

Doing anything to a file on a Mac usually means a detour: open Preview and export, or upload it to a converter website and download the result. Radlet takes the drag you're already making and adds one key, the way a game's weapon wheel puts all your gear one flick away. Hold **Shift** mid-drag and a wheel of formats opens under the pointer. Drop on **JPEG** and the JPEGs appear next to the originals.

- **No window to open.** The wheel appears where the pointer is, in the middle of a Finder drag. Hold **Command** instead for tools, or **Control** for your loadouts. Or select files and press **⌃⌥V**.
- **Nothing you convert leaves your Mac.** Files are never uploaded, and there are no accounts or telemetry. Text recognition, background removal, face detection, subtitles and translation use Apple's on-device frameworks. Radlet only goes online to check for updates and, if you say yes, to download FFmpeg.
- **Nothing gets overwritten.** Results land beside the originals under a free name, and every job can be undone from History.

**Why "Radlet"?** It's **rad**ial + **-let**. Radial, because it's a wheel (in German and Dutch, *Rad* even means wheel). *-let*, because it's a small one, as in *droplet* or *booklet*: it opens under your pointer and gets out of the way. More importantly, *rad* is slang for awesome. The wheel tries to live up to that.

## How it works

```
Finder drag + Shift ──▶ DragWatcher ──▶ WheelController ──inspect──▶ radlet serve ──▶ catalog::wheel
                                              │                          │
                                         user drops                     run
                                              ▼                          ▼
                                     HUD (progress, Undo) ◀──events── job::run_batch ──▶ one `radlet worker`
                                                                                         process per file
```

Radlet is two programs. The **Mac app** (Swift) spots the drag, draws the wheel and shows progress. The **engine** (Rust) does the work. It runs as `radlet serve`, a helper process the app talks to in JSON lines over stdin and stdout.

1. **Spotting the drag.** The app listens to global mouse events and, only while a drag is in progress, polls the modifier keys and the drag pasteboard. That's why it needs no Accessibility or Input Monitoring permission.
2. **Filling the wheel.** The app sends the dragged paths to `inspect`. The engine detects each file's format and returns three sets of slots (convert, tools, loadouts), each carrying an opaque `action`.
3. **Running the job.** On drop, the app sends that action back unchanged. The wheel knows nothing about formats, so a new conversion, tool or loadout in the engine shows up on the wheel with no app changes.
4. **One worker per file.** `job::run_batch` runs each file in its own `radlet worker` process with a time limit. A crash or hang fails only that file, its partial outputs are deleted, and progress events stream back to the HUD.
5. **Helpers on demand.** For Apple's Vision, Speech and Translation frameworks, the engine runs `radlet-vision`, a small Swift helper bundled in the app. Audio and video go through FFmpeg when it's installed, HEIC and AVIF through macOS's `sips`, and PDF to image through a bundled PDFium.

### Guarantees

- **Never overwrites.** Outputs are written to a hidden temporary file and hard-linked into place under a free name (`photo 2.jpg`), which fails if the name is taken. Originals are never modified. With "Replace originals" on, the app moves them to the Trash, where Undo can find them.
- **Tidy multi-file results.** Up to four pages or parts are saved next to the original; more go into one folder, such as `report (pages)`.
- **One bad file fails only itself.** Crashes and hangs are reported per file.
- **Upright and colour-correct.** EXIF orientation is applied to pixels, and ICC profiles are carried into PNG, JPEG, WebP, TIFF and PDF outputs.
- **Bounded inputs.** Images up to 40,000 px a side and 1 GiB decoded; PDFs decompress to at most 512 MB; text files up to 64 MB; SVGs drawn at most 16,384 px a side; ZIPs extract at most 1,000 times their size (and 64 GB); folders up to 500,000 items.
- **Local only.** Files are worked on here and never uploaded; no accounts or telemetry. The only network use is the daily update check (which asks before installing) and downloading FFmpeg when you accept.
- **Cleans up after itself.** When stdin closes (the app quit or crashed), the helper cancels its jobs and exits. A worker whose supervisor dies kills itself and its FFmpeg.

## Features

### The wheel

<p align="center">
  <img src="docs/images/skins.png" alt="The wheel in its six built-in skins: Glass, Instrument, Tactical, Neon, Sunset and Porcelain">
  <br>
  <sub>Six built-in skins, or write your own in CSS. Previews rendered from Radlet's design files.</sub>
</p>

- **Three wheels on one gesture.** **Shift** converts, **Command** opens tools, **Control** runs your loadouts. Drop on a slot to run it, or in the middle to cancel.
- **Styles** (Settings → Wheel). **Dial**, one ring, is the default. Under **Advanced** are **Orbit**, which adds an outer ring of presets behind each slot (JPEG → Smart or Best, Compress → Email or Discord) and widens it when you push into a slot with more, and **Bloom**, where resting on a slot or flicking past it fans out its presets. In both, presets stay out for as long as you keep dragging, until you rest on another slot: overshooting, crossing the centre, passing over a neighbouring slot on the way to a side preset, or briefly leaving the wheel doesn't lose them. The highlight glides between slots, borders are slightly sticky so it doesn't flicker, and a Force Touch trackpad taps as you cross into a slot.
- **Slots that stay put.** The wheel holds eight (or 6, 10 or 12), and anything more is under **More…**. Conversions are ranked for each kind of file, and less known formats wait under **More…** (see [What's on the wheel](#whats-on-the-wheel)). The slots you use most stay on the wheel, but nothing moves around: the most useful go up, right, down and left, and stay there.
- **Skins.** Glass (the plain system look and the default), Instrument, Tactical, Neon, Sunset and Porcelain, plus your own as small CSS files of custom properties (`--accent: #00FF9C;`), starting from any built-in with `--base`. Radlet picks up an edited skin the next time the wheel opens and lists any mistakes with line numbers. See [docs/skins.md](docs/skins.md).

### Convert (Shift)

| Kind | What you get |
| --- | --- |
| **Images** | JPEG, PNG, WebP, HEIC, AVIF, JPEG XL, TIFF, GIF, BMP, ICO, ICNS, TGA and QOI to each other or to PDF. Any picture also traces into an **SVG** drawing of filled shapes that stays sharp at any size, for logos, icons and line art: in colour, in black and white, Simple (few colours, smooth shapes) or Detailed. Camera RAW (DNG, CR2, CR3, NEF, ARW, RAF, ORF, RW2 and more), Photoshop PSD and JPEG 2000 read in too, through macOS. Moving pictures keep every frame and its timing between GIF, WebP and PNG (APNG), and a GIF becomes an MP4 or MOV video. A picture converts to **Text** through text recognition. A PDF converted to JPEG, PNG, WebP or TIFF gives one image per page. |
| **SVG** | Treated as an image: PNG, JPEG, WebP, ICO and the rest, drawn sharp at 1×, 2× or 4× (at least 1024 px on its longest side by default). SVG to PDF stays vector. |
| **Icons** | Any picture or SVG to a macOS ICNS (16 to 1024 px) or an ICO. |
| **Documents** | TXT, Markdown, HTML, Word (.docx), RTF and **EPUB** e-books convert to each other and to PDF, keeping headings, lists, quotes, code, tables, bold, italic and links. Word 97 (.doc) and OpenDocument (.odt) files read in too, through macOS, and so does the text of **PowerPoint** slides (titles, bullets, tables and speaker notes). A PDF converts to **Word**, text, Markdown, HTML or RTF; on a Mac, scanned pages are read with text recognition first. |
| **Keynote, Pages, Numbers** | When the apps are installed, Radlet asks them to export: Pages to PDF, Word, EPUB, RTF, text, Markdown or HTML; Numbers to PDF, Excel, CSV or JSON; Keynote to PDF, PowerPoint or text. PowerPoint files go to PDF or Keynote through Keynote too, so slides look as they were made. A document you have open stays open. |
| **Data** | CSV, TSV, JSON and Excel (.xlsx) convert to each other (numbers stay numbers, IDs with leading zeros stay text); old Excel (.xls) and OpenDocument (.ods) workbooks read in too, and so do **SQLite** databases (a sheet per table, read without changing the file). A table also becomes a Markdown or HTML table, or a PDF. A workbook of several sheets becomes one CSV per sheet. JSON converts to and from **YAML**, **TOML** and **XML** (YAML anchors and merge keys are expanded; XML attributes become `@name` keys). |
| **Audio and video** | Out of the box, through macOS: MP4 ↔ MOV (without re-encoding when the codecs allow), video or sound to M4A, WAV and AIFF, from MP4, MOV, M4A, MP3, WAV, AIFF and FLAC, plus trimming, compressing (also to fit a size), joining and subtitling video. With FFmpeg: MP3, M4A, WAV, FLAC, AIFF, OGG, Opus and WMA; MP4, MOV, WebM, MKV, AVI, WMV and GIF. Video is remuxed without re-encoding when the codecs allow. |
| **Subtitles** | SRT, WebVTT, ASS/SSA, SubViewer (SBV) and TTML (DFXP) to each other or to plain text. |
| **Fonts** | TTF and OTF to WOFF2 or WOFF for the web, and web fonts back to TTF or OTF. |
| **Folders** | Drop a folder for **Images to JPEG**, **Documents to PDF**, **Audio to MP3** and so on. Results go into a copy of the folder ("Trip (JPEG)") with the same subfolders, and anything that couldn't be converted is listed in "Not converted.txt". |
| **Archives** | A folder becomes a ZIP (or a .tar.gz) and a ZIP becomes a folder; on a Mac, so do 7-Zip, RAR, TAR (.tar.gz, .tar.bz2, .tar.xz) and ISO disc images. ZIPs made with Deflate, Deflate64, LZMA, XZ or Zstandard open, and so do locked ones with their password (**Unzip with password…**). Extraction never writes outside its folder, skips links and `__MACOSX` clutter, and refuses archive bombs. |


#### What's on the wheel

Each kind of file puts the conversions most people want on the wheel, most useful first, so the first four land up, right, down and left. Less known formats wait under **More…** even when the wheel has room, with the ones you use most at the top. A file's own format is left out.

| Files | On the wheel | Under More… |
| --- | --- | --- |
| Photos (JPEG, PNG, HEIC, WebP, AVIF, TIFF and the rest) | JPEG, PNG, PDF, WebP, HEIC, SVG, Text | TIFF, GIF, AVIF, ICO, ICNS, BMP, JPEG XL, TGA, QOI |
| Camera RAW, Photoshop | JPEG, PNG, TIFF, PDF, HEIC, WebP | GIF, AVIF, ICO, ICNS, BMP, JPEG XL, TGA, QOI, SVG, Text |
| GIF | MP4, PNG, JPEG, WebP, MOV, PDF | HEIC, TIFF, AVIF, ICO, ICNS, BMP, JPEG XL, TGA, QOI, SVG, Text, WebM |
| SVG | PNG, PDF, JPEG, WebP | ICO, ICNS, TIFF, GIF, AVIF, JPEG XL, BMP, TGA, QOI, HEIC |
| Video | MP4, MOV, MP3, GIF, M4A, WebM | MKV, AVI, WMV (and WAV, AIFF without FFmpeg) |
| Audio | MP3, M4A, WAV, FLAC | AIFF, OGG, Opus, WMA |
| PDF | Word, JPEG, PNG, Text, Markdown | WebP, TIFF, HTML, RTF |
| Word, Pages, Markdown, HTML, text | PDF, Word, Markdown, Text, HTML | RTF, EPUB (and SRT, VTT for text) |
| PowerPoint | PDF, Keynote, Word, Text, Markdown | HTML, RTF, EPUB |
| Excel, CSV, old workbooks, SQLite | Excel, CSV, PDF, JSON, Markdown | TSV, HTML |
| JSON | CSV, Excel, YAML | TSV, TOML, XML |
| Subtitles | SRT, VTT, Text | ASS, TTML, SBV |

When only one format would wait under **More…**, it stays on the wheel, since **More…** would take its slot anyway. So RTF, EPUB and TSV files, Keynote, Numbers, YAML, TOML, XML, fonts, archives and folders show everything on the wheel. Conversions that need FFmpeg, Keynote, Pages or Numbers follow the same ranking, after the ones that work now.

### Tools (Command)

Tools ending in "…" open a window first, with a live preview where it helps.

| Group | Tools |
| --- | --- |
| **Images** | **Resize…** (percentage, longest side, exact width or height) · **Crop…** with aspect presets · **Redact…** with black boxes, pixelate or blur · Rotate · **Make icons** (favicons with the HTML for `<head>`, a macOS .icns or an iOS app icon set) |
| **Compress…** | By quality level or to a target size such as 2 MB. **Smart** (the default) finds the smallest JPEG or WebP that still looks the same by measuring visual similarity (SSIM), instead of guessing a quality number. **Fit for** presets fill in sizes that get through email, Discord and WhatsApp. PDFs shrink too. |
| **PDF** | **Split…** (every page, every few pages, or ranges like `1-3, 5, 8-`) · **Pages…** to reorder, delete and rotate · **Page numbers…** · **Watermark…** · **Password…** (AES-256; passwords are never saved or logged) · Combine into PDF · **Make searchable PDF** · **Black out text…**: words, phrases, email addresses, phone and card numbers removed for real, in the text, form fields and pictures of text, so nothing under a box can be copied · **Scan document**: photos of paper pages, each found, squared up and evened out, into one searchable PDF |
| **Audio and video** | **Trim…** · **Make GIF…** · **Save stills** (ten evenly spaced frames) · **Even out volume** (-16 LUFS, as podcasts do) · **Make subtitles…** as SRT, WebVTT or a transcript, with on-device speech recognition · **Join clips** (videos into one MP4, each fitted into the first one's shape, or sound into one M4A) · **Burn in subtitles** (choose a video and its subtitle file; every player shows them) |
| **Privacy** | **Privacy check…** shows what a file gives away before you share it: GPS location, camera and serial number, names, dates, editing apps, and hidden content such as earlier versions kept inside PDFs. **Remove Metadata** saves cleaned copies. **Hide faces** pixelates, blurs or blacks out every face Vision finds. |
| **Text and AI** | **Copy text** from pictures and scanned PDFs · **Remove background** to a transparent PNG · **Translate…** documents and subtitles on device (macOS 26), keeping headings, lists, tables and timings · **Read code** (QR codes, and on a Mac barcodes too: EAN, UPC, Code 128, PDF417, Data Matrix, Aztec and more) / **Make QR code** · With Apple Intelligence (macOS 26 on Apple silicon), written on your Mac: **Describe** a picture as alt text for the clipboard, and **Summarize** a document or PDF into `report (summary).md` |
| **Documents and data** | **Word count** ("1,234 words · 6 min read") · Copy text · **Clean up text** · **Combine into one** · **Combine into workbook** (several tables into one .xlsx, a sheet each) |
| **Developers** | **Format JSON**, with **Minify** and **Sort keys** presets, keeping numbers and strings exactly as written · **Format XML** / Minify, leaving the text inside elements exactly as it was · **Optimize SVG**: editor data (Inkscape, Sketch, Illustrator), metadata, comments, empty groups and whitespace removed, drawing the same pixel for pixel · **Check syntax** for JSON, YAML, TOML, XML, SVG, CSV and TSV: "Valid JSON", or the line and column of the first problem · **Copy as data URI** for pictures, SVGs and fonts (up to 2 MB), ready for CSS or HTML · **Base64**: `photo.png` becomes `photo.png.b64`, and a `.b64` file turns back into what it holds |
| **Folders** (on copies) | **Zip together** · **Zip with password…** (AES-256) · **Merge folders** (identical files kept once) · **Flatten** · **Sort by kind** · **List contents** (a tree with sizes, or a CSV) · **Find duplicates** |
| **Everyday** | **Rename to .jpg** comes first on the wheel when a file's name doesn't match what it holds (a .png that is really a JPEG, an iPhone photo named .jpg that is HEIC, a download with no extension), found by reading the file itself · **Rename…** with a pattern such as `Trip {date} {n}`, previewed first, keeping each extension unless you choose Lowercase or a new one (with a reminder that renaming doesn't convert); `{content}` (**What's in it**) names each picture or document by what's in it, such as `Red apple on a table.jpg`, from Apple Intelligence or, without it, the text and things Vision recognises · **Copy path** · **Copy name** · **Open in Terminal** · **Share…** (AirDrop, Mail and Messages as presets) · **Copy image** · Copy SHA-256, with MD5, SHA-1 and SHA-512 as presets |

### Around the wheel

- **History** (menu bar → History…): the last 50 jobs, each with Undo. Undo moves results to the Trash, puts replaced originals back and restores old names after Rename.
- **Drag results out** of the progress window into Finder, Mail or any app, or press **Next…** to open the wheel again for the results (convert to PDF, then compress it).
- **Convert Clipboard** (menu bar): converts files copied in Finder, or a copied image such as a screenshot, and puts the result back on the clipboard.
- **Replace originals** (Settings → Output, off by default): originals go to the Trash, and a same-format result takes the original's name.
- **Watch folders** (Settings → Watch): files added to a folder are converted, compressed or run through a loadout automatically, into a `Converted` folder inside.
- **Loadouts** (Control wheel): your own sets of steps, like a game loadout for your files. **Save as Loadout…** in a tool window keeps its settings, and **Settings → Loadouts** edits them. With Apple Intelligence, the sparkles button there makes one from a sentence, such as "under 2 MB, as PDF": the steps are drafted on your Mac, checked like any loadout, and opened unsaved for you to review.
- **Share loadouts and skins.** The share button in **Settings → Loadouts**, and **Share** under **Settings → Wheel → Skin**, copy a `radlet://` link or save a `.radletloadout` or `.radletskin` file. Opening one shows what it holds (a loadout's steps in words, a skin on a live wheel) and adds it only if you say so; nothing runs by itself, and a name already taken gets a number. Ready-made ones are in the [gallery](https://radlet.vercel.app/gallery/).
- **What a conversion needs, offered.** Slots that need something this Mac doesn't have yet show a badge: FFmpeg for more audio and video, Keynote, Pages or Numbers for their documents. Choosing one explains what's missing and helps get it: Radlet installs FFmpeg with Homebrew (or, once one is published, its own FFmpeg build), and opens the App Store page for Apple's free apps. Anything installed meanwhile, in Terminal or the App Store, is found the next time you choose it. A job that needs a speech or translation language download offers to open the right settings pane.
- **Other ways in.** Finder's right-click **Services → Convert with Radlet** (or **Radlet Tools**), **Open With → Radlet**, **⌃⌥V** in Finder for the selected files, and the menu bar (**Convert Files…**, **Tools for Files…**, **Run a Loadout…**, **Convert Finder Selection**). Each opens the wheel for the keyboard: arrow keys to choose, Return to run, Tab to switch wheels, Escape to cancel.
- **Your shortcuts on the wheel.** The shortcuts button in **Settings → Loadouts** lists your shortcuts from the Shortcuts app; the ones you tick join the Control wheel after your loadouts. Each file goes in as the shortcut's input, and what it returns is saved beside the file, such as `photo (Resize for Web).jpg`; a shortcut that returns nothing (it shared or sent the file) saves nothing. AI agents can't run them.
- **Command line, Shortcuts and AI agents.** The same engine is the `radlet` command. `radlet mcp` is a Model Context Protocol server, so Claude and other AI apps can do what the wheel does on your files, locally: convert, compress, combine, clean, read and make QR codes, make icons, tidy folders, subtitle, describe, summarize and run loadouts. Password tools and your own shortcuts aren't offered to agents. See [Reference](#reference).

## Tech stack

| Layer | Built with |
| --- | --- |
| Mac app | Swift 5.10 as a Swift package (no Xcode project), AppKit + SwiftUI, macOS 14+. It avoids SwiftUI macros so it builds with just the Command Line Tools. |
| On-device intelligence | Apple's Vision (text, subject masks, faces), Speech (subtitles), Translation (macOS 26) and the system text engine for PDF layout, all in the `radlet-vision` helper |
| Engine and CLI | Rust 2021 (1.88+): `clap`, `serde`, `image` (with `ravif` for AVIF) + `webp`, `zune-jpegxl`, `png`, `lopdf`, `resvg` + `svg2pdf`, `pulldown-cmark`, `calamine` + `rust_xlsxwriter`, `rusqlite` (SQLite, built in), `zip`, `brotli`, `qrcode` + `rqrr`, `kamadak-exif`, `yaml-rust2`, `toml`, `quick-xml` |
| Media | AVFoundation (in the helper) for everyday audio and video, FFmpeg and ffprobe (optional, from Homebrew) for the rest; macOS `sips` for HEIC, RAW, PSD and JPEG 2000, `textutil` for Word 97 and ODT, `tar` (libarchive) for 7-Zip, RAR, TAR and ISO, and AppleScript for Keynote, Pages and Numbers exports; PDFium for PDF to image |
| Updates | [Sparkle](https://sparkle-project.org) (MIT), from an appcast attached to each GitHub release and checked with an EdDSA key |
| Protocols | JSON lines between app and engine (`radlet serve`), Model Context Protocol for AI agents (`radlet mcp`) |
| Quality | 240+ Rust tests and 90+ Swift tests, `rustfmt`, `clippy -D warnings`, `cargo deny` licence checks, GitHub Actions on Linux and macOS |

## Install

Download the `.dmg` from the [latest release](https://github.com/Suvraneel/radlet/releases/latest), open it and drag **Radlet** into **Applications**. It runs on macOS 14 or later, on Apple silicon and Intel Macs. Everyday audio and video (MP4, MOV, M4A, MP3, WAV, AIFF, FLAC) work as they are. Conversions that need FFmpeg (MP3 output, WebM, MKV, GIFs, loudness, making subtitles) are on the wheel too, marked with a small badge: choose one and Radlet offers to install FFmpeg with Homebrew if you have it, then carries on. Without Homebrew it downloads Radlet's own FFmpeg build (about 55 MB, LGPL, checked against the checksum built into the app). Radlet checks for updates once a day and asks before installing one (**Settings → General**).

Radlet isn't notarized by Apple yet, so macOS blocks it the first time you open it. On macOS 15 or later, open **System Settings → Privacy & Security** and click **Open Anyway**. On macOS 14, Control-click Radlet in Applications and choose **Open**.

## Run it locally

To build Radlet yourself, you need macOS 14 or later, Xcode or just the Command Line Tools (`xcode-select --install`), and [Rust](https://rustup.rs).

```sh
git clone https://github.com/Suvraneel/radlet.git
cd radlet
mac/scripts/run.sh             # builds Radlet, replaces /Applications/Radlet.app and starts it
mac/scripts/run.sh --check     # the same, plus a self-test of text recognition
```

For audio and video beyond what macOS plays, install FFmpeg. Images, PDFs, text and everyday MP4, MOV and M4A work without it.

```sh
brew install ffmpeg
```

Radlet lives in the menu bar (a circle-in-a-circle icon), not the Dock. The first time, a short setup guide lets you pick the wheel's style and skin, where results go and your shortcuts, checks for FFmpeg, and opens a folder of sample photos to practise on: its checklist ticks off as you convert, use a tool and press ⌃⌥V. It's in the menu bar later as **Setup and Tutorial…**. Start dragging files in Finder, then hold **Shift**. An app built on your own Mac opens without Gatekeeper's warning.

### The engine on its own (macOS or Linux)

```sh
cargo test --all               # about 200 unit and end-to-end tests
cargo build --release
./target/release/radlet formats  # what converts to what on this machine
```

The PDF-to-image tests are skipped unless `RADLET_PDFIUM` points at a PDFium library.

| Variable | What it does |
| --- | --- |
| `RADLET_PDFIUM` | Path to `libpdfium.dylib` (or `.so`), for PDF to image outside the app |
| `RADLET_FFMPEG`, `RADLET_FFPROBE` | Use these binaries instead of the ones on `PATH` or in Homebrew's usual locations |
| `RADLET_NO_FFMPEG`, `RADLET_NO_AVFOUNDATION` | Set to `1` to act as if FFmpeg (or, on a Mac, AVFoundation too) weren't there |
| `RADLET_BSDTAR` | Path to libarchive's `bsdtar`, for 7-Zip, RAR, TAR and ISO (on a Mac, the system's `tar` is used) |
| `RADLET_VISION` | Path to the `radlet-vision` helper, when the engine isn't running inside the app |
| `RADLET_HELPER` | Run the Swift app against this `radlet` binary, such as a debug build |
| `RADLET_LOADOUTS_DIR` | Where loadouts live (default `~/Library/Application Support/Radlet/Loadouts/`) |
| `RADLET_PASSWORD` | The password for `radlet protect`, `radlet unlock` and `radlet unzip`, kept out of `ps` |
| `RADLET_ZIP_PASSWORD` | The password `radlet zip` locks a new ZIP with |
| `RADLET_NO_IWORK` | Set to `1` to act as if Keynote, Pages and Numbers weren't installed |

The engine also finds an FFmpeg the app installed, in `~/Library/Application Support/Radlet/FFmpeg`.

### Working on the Mac app

```sh
swift run --package-path mac RadletLab     # the Wheel Lab: every style and state, plus a skin editor
swift test --package-path mac            # needs full Xcode; the Command Line Tools have no XCTest
RADLET_HELPER=$PWD/target/debug/radlet swift run --package-path mac Radlet   # against a debug engine
```

Outside the app, point `RADLET_VISION` at `$(swift build --package-path mac --show-bin-path)/radlet-vision`. `radlet-vision selftest` checks that text recognition and searchable PDFs work on this Mac. Keep SwiftUI macros such as `@State` out of the app, so it still builds with just the Command Line Tools.

The Swift tests cover the app's pure logic in `RadletKit`: wheel hit-testing for every style, the skin parser and built-in skins, protocol framing, HUD state, the crop and redact editors, "Replace originals" naming and the watch-folder scanner. A contract test decodes a real `radlet serve` session, and CI re-records that session with the freshly built engine, so an engine change that would break the app fails the build.

### Sharing a build

`mac/scripts/build-app.sh` makes `dist/Radlet.app` (ad-hoc signed). `mac/scripts/make-dmg.sh` packs `dist/Radlet-<version>.dmg`, a zip and their SHA-256 checksums. The disk image opens on a styled window (drag Radlet onto Applications) laid out by [dmgbuild](https://github.com/dmgbuild/dmgbuild), which the script installs into `target/dmgbuild` the first time; without Python 3 or a network it makes a plain image instead. Its background is in `mac/dmg`, and the app icon is `mac/Resources/AppIcon.icns`. Set `UNIVERSAL=1` for Apple silicon and Intel, `VERSION=0.3.0` to stamp a version, and `SKIP_PDFIUM=1` to leave out PDF to image. The build downloads a pinned PDFium once into `target/pdfium`.

Unless they're signed with a Developer ID and notarized, whoever opens one has to allow it once ([docs/release-notes.md](docs/release-notes.md) has the steps). With a certificate, `SIGN_IDENTITY="Developer ID Application: …" mac/scripts/make-dmg.sh` signs the app, its helpers, Sparkle and the disk image with the hardened runtime, and `mac/scripts/notarize.sh` notarizes and staples them (with an App Store Connect API key, or a `notarytool` keychain profile).

### Publishing a release

Releases come from version tags. Bump `version` in `Cargo.toml` (and run `cargo build`, so `Cargo.lock` follows), merge it, then tag that commit on `main`:

```sh
git tag v0.3.0
git push origin v0.3.0
```

[The release workflow](.github/workflows/release.yml) builds the app for Apple silicon and Intel, checks every binary and the bundled helpers, and publishes the `.dmg`, zip and checksums on the Releases page, with [docs/release-notes.md](docs/release-notes.md) above GitHub's list of what changed. It stops early if the tag doesn't match `Cargo.toml`. A tag with a hyphen, such as `v0.3.0-beta.1`, becomes a prerelease. Re-running the workflow replaces a release's files, and publishes the release again if deleting and re-pushing its tag turned it into a draft.

**Updates (once).** Run `mac/scripts/setup-updates.sh` on your Mac and commit `mac/Resources/Info.plist`. It makes an EdDSA key in your login keychain, writes its public half into Info.plist and stores the private half as the `SPARKLE_ED_PRIVATE_KEY` secret with `gh`. From then on every release also carries an `appcast.xml`, signed for installed copies, and the workflow checks the signature against the app's key before publishing. Copies from before the key was added don't update themselves; they need one manual download.

**Signing and notarization (once you have a Developer ID).** Add these repository secrets and the next tag is signed, notarized and stapled, and its release notes drop the "Open Anyway" steps:

| Secret | What goes in it |
| --- | --- |
| `MACOS_CERTIFICATE_P12` | Your **Developer ID Application** certificate and key, exported from Keychain Access as .p12, then base64 (`base64 -i cert.p12 \| pbcopy`) |
| `MACOS_CERTIFICATE_PASSWORD` | The password you gave the .p12 |
| `NOTARY_KEY_P8` | An App Store Connect API key (Users and Access → Integrations, with the Developer role): the contents of its `AuthKey_….p8` |
| `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID` | That key's ID and your issuer ID, from the same page |

**FFmpeg for Macs without Homebrew (once per FFmpeg version).** Run the [FFmpeg workflow](.github/workflows/ffmpeg.yml) from the Actions tab with a tag such as `ffmpeg-8.0.1-1`. It builds an LGPL FFmpeg and ffprobe from pinned sources with [mac/scripts/build-ffmpeg.sh](mac/scripts/build-ffmpeg.sh), for Apple silicon and Intel, signs and notarizes them with the secrets above (until there's a Developer ID they're ad-hoc signed, like Homebrew's own programs, which is enough because the app downloads them itself rather than through a browser), and publishes them as a release that's never marked latest. Then pin it in the app and commit the result:

```sh
mac/scripts/pin-ffmpeg.sh ffmpeg-8.0.1-1 <SHA-256 from the run's summary>
```

That checks the download is the one the run built, shows who signed it, and writes its URL, size and SHA-256 into `mac/Resources/ffmpeg-build.json`; the app refuses any download that doesn't match. A published build is never replaced: to rebuild, run the workflow with the next build number (`ffmpeg-8.0.1-2`). The release carries every source's licence and `SOURCES.txt`, and next to the download, `ffmpeg-<version>-source.tar`: every archive the build used, unchanged, with the build script, as the LGPL asks. Settings → Advanced → Open source links to that release. Until a build is pinned, Install FFmpeg uses Homebrew, or explains how to get it.

### Upgrading from a Veer build

Radlet was called Veer in earlier builds. When it starts, it carries everything over: settings, loadouts (then called recipes), skins and History. A few things need a hand:

- **Command-line tool:** the old `/usr/local/bin/veer` link points at the old app. Use **Settings → Advanced → Install Command-Line Tool…** to add `radlet`, then delete the old link.
- **Start at login:** turn it on again in **Settings → General**.
- **AI agents:** the MCP server is now `radlet`; copy the new configuration from **Settings → Advanced**.
- **Environment variables:** `VEER_*` variables are now `RADLET_*`, and `VEER_RECIPES_DIR` is `RADLET_LOADOUTS_DIR`.
- **Delete `Veer.app`** from Applications once Radlet is running.

### The landing page

`site/` is a single static page, served from two places. **[radlet.vercel.app](https://radlet.vercel.app)** is the canonical address: the page names it in its canonical link and share-card tags, so search engines and link previews use it. **[suvraneel.github.io/radlet](https://suvraneel.github.io/radlet/)** is a mirror.

- **Vercel:** import the repository and set **Root Directory** to `site`, leaving **Include files outside the root directory in the Build Step** on. `site/vercel.json` copies the demo video and the skins image in from `.github/media` and `docs/images`, so they aren't stored twice.
- **GitHub Pages:** in the repository's **Settings → Pages**, set **Source** to **GitHub Actions**. `.github/workflows/pages.yml` copies the same two files in and publishes the page on every push to `main` that changes it.

**The gallery** (`site/gallery`, at [radlet.vercel.app/gallery](https://radlet.vercel.app/gallery/)) lists loadouts and skins anyone can open in Radlet with one click. Each is a plain file: a `.radletloadout` (the same TOML as the loadouts folder) in `loadouts/`, or a `.radletskin` (the skin's CSS) in `skins/`. To add one, put the file there and list its path in `site/gallery/items.json`. The page reads the files itself, so there's nothing to build. The Rust tests read every loadout as the app would and check the folder and the list match; the Swift tests read every skin with the app's own parser.

If the media can't be copied, the page loads it from GitHub through jsDelivr. `python3 -m http.server --directory site` previews it locally (copy those two files into `site/media/` first, or the page uses the copies on GitHub).

### Troubleshooting

Radlet keeps a diagnostics log at `~/Library/Logs/Radlet/radlet.log`: what the drag watcher saw, wheel choices and helper restarts. It records file counts and extensions, never names or paths. **Menu bar → Troubleshooting → Copy Diagnostics** copies the system details and the last 200 log lines, ready for a bug report.

## Project structure

```
crates/
  radlet-core/            the engine: format detection, the catalogue behind the wheel,
                        jobs and workers, never-overwrite naming, loadouts, privacy check
    src/engines/        one module per kind: image, pdf, doc, docx, sheet, media,
                        archive, folder, svg, icons, codes, vision, speech, translate…
  radlet-cli/             the `radlet` binary: the CLI, `radlet serve` (for the app), `radlet mcp`
mac/
  Sources/Radlet/         the menu bar app: drag watcher, wheel, progress HUD, settings, tool windows
  Sources/RadletKit/      unit-tested logic: protocol models, wheel geometry, skins, history, rename
  Sources/RadletUI/       draws and drives the wheel, shared by the app and the Wheel Lab
  Sources/RadletLab/      the Wheel Lab, a storybook and skin editor for the wheel
  Sources/radlet-vision/  the Vision, Speech and Translation helper the engine runs
  Tests/RadletKitTests/   Swift tests, including the live protocol contract
  scripts/              run.sh, build-app.sh, make-dmg.sh, contract-session.sh
examples/loadouts/      a sample loadout
docs/                   wheel styles and skins, release notes, images
site/                   the landing page: radlet.vercel.app, mirrored on GitHub Pages
.github/                CI and Pages workflows, and the README's demo media
deny.toml               licence policy: every dependency must be permissive
```

## Reference

<details>
<summary><strong>CLI</strong></summary>

```sh
radlet convert *.heic --to jpg              # one output per input, beside the original
radlet convert talk.mkv --to mp4            # remuxes without re-encoding when codecs allow
radlet convert report.pdf --to png          # one image per page (needs PDFium)
radlet text receipt.jpg                     # prints the recognised text (macOS app)
radlet convert scan.png --to txt            # saves it as scan.txt instead
radlet searchable scan.pdf                  # "scan (searchable).pdf"
radlet cutout portrait.jpg --crop           # transparent PNG trimmed to the subject
radlet convert logo.png --to svg            # traced into shapes; or: radlet trace logo.png --bw --detail low
radlet resize photos/*.jpg --max 1600 --out ~/Desktop/small
radlet resize banner.png --width 1200       # exact width, keeps the aspect ratio
radlet crop photo.jpg --rect 0.1,0.1,0.8,0.8 # fractions x,y,w,h: keeps the middle 80%
radlet redact scan.png --rect 0.6,0.05,0.35,0.1 --style black
radlet redact contract.pdf --text "Jane Doe" --emails --phones # removed for real; macOS app
radlet scan page1.jpg page2.jpg             # "page1 (scanned).pdf"; macOS app
radlet strip IMG_0042.jpg                   # lossless: drops GPS/EXIF, keeps orientation and colour profile
radlet compress clip.mov --level strong
radlet compress photo.jpg --target 500KB
radlet compress photo.jpg --level smart     # smallest file that still looks the same
radlet split book.pdf --ranges "1-3, 5, 8-" # or --every 10; default: every page
radlet pages deck.pdf --order 3 --order 1 --order 2 --rotate 1:90
radlet numbers report.pdf --style page-of-total --position bottom-right --skip-first
radlet watermark contract.pdf --text DRAFT --opacity 0.2
RADLET_PASSWORD=secret radlet protect statement.pdf # env keeps the password out of `ps`
RADLET_PASSWORD=secret radlet unlock "statement (protected).pdf"
radlet trim interview.m4a --start 1:30 --end 12:05
radlet gif clip.mov --start 3 --end 6.5
radlet join intro.mp4 talk.mov outro.mp4    # "intro (joined).mp4"; sound files join into M4A
radlet burn talk.mp4 talk.srt               # "talk (subtitled).mp4"
radlet convert loop.gif --to webp           # keeps every frame; or png (APNG), mp4
radlet frames talk.mp4 --count 12 --format png
radlet normalize podcast.wav
radlet privacy IMG_0042.jpg report.pdf      # what they reveal; radlet strip removes it
radlet subtitles talk.mp4 --language en-US  # talk.srt (--to vtt or txt); macOS app
radlet mcp                                  # serve Radlet's tools to an AI agent
radlet combine scan1.png scan2.png notes.pdf  # documents and SVGs go in too
radlet convert notes.md --to docx           # or pdf, html, rtf, txt; and back again
radlet convert logo.svg --to png            # at least 1024 px; radlet resize logo.svg --max 4096
radlet convert orders.csv --to json         # csv, tsv, json, md and html tables
radlet convert orders.csv --to xlsx         # and back: radlet convert book.xlsx --to csv
radlet convert app.sqlite --to xlsx         # a sheet per table
radlet convert Brand.ttf --to woff2         # and back: radlet convert Brand.woff2 --to ttf
radlet convert book.md --to epub            # and back, or to pdf, docx, txt
radlet convert deck.pptx --to md            # slide text; --to pdf goes through Keynote
radlet convert Report.pages --to docx       # through Pages; Numbers and Keynote too
radlet workbook jan.csv feb.csv mar.csv     # "jan (combined).xlsx", a sheet each
radlet convert Photos --to jpeg             # "Photos (JPEG)": the pictures inside, converted
radlet codes poster.jpg                     # prints what its QR code says
radlet qr wifi.txt                          # "wifi (QR code).png"
radlet icons logo.svg --set favicon         # or --set mac (logo.icns), --set ios
radlet faces party.jpg --style blur         # Vision; macOS app
radlet translate notes.md --to fr           # "notes (fr).md"; macOS 26 app
radlet words essay.docx                     # 1,234 words · 6 min read
radlet join week1.md week2.md               # "week1 (combined).md"
radlet json data.json --minify              # or without --minify to re-indent; --sort-keys orders keys
radlet xml feed.xml                         # re-indent; --minify removes whitespace between tags
radlet optimize-svg logo.svg                # "logo (optimized).svg", drawing the same
radlet check config.yaml data.json          # "Valid YAML", or the line and column; exits 1 on a problem
radlet data-uri icon.svg                    # prints data:image/svg+xml;base64,...
radlet base64 photo.png                     # photo.png.b64; radlet base64 photo.png.b64 decodes it
radlet fix-extension download photo.png     # renamed to what they really hold; --dry-run to preview
radlet clean notes.txt                      # UTF-8, \n line endings, no trailing spaces
radlet zip Photos notes.txt                 # Archive.zip; radlet convert Photos --to zip for one each
RADLET_ZIP_PASSWORD=secret radlet zip Photos # every file locked with AES-256
radlet unzip Archive.zip                    # RADLET_PASSWORD=secret for a locked one
radlet merge "Photos 2023" "Photos 2024"    # "Photos 2023 (merged)"
radlet flatten Downloads                    # or: radlet sort, radlet list --csv, radlet duplicates
radlet checksum installer.dmg               # SHA-256; --algorithm md5, sha1 or sha512
radlet loadout "Web-ready image" *.png
radlet shortcut "Resize for Web" *.png     # one of your shortcuts; `radlet shortcuts` lists them
radlet describe photo.jpg                   # alt text, by Apple Intelligence (macOS 26 app)
radlet summarize report.pdf                 # "report (summary).md"; Apple Intelligence
radlet --json convert a.png --to webp       # JSON lines for scripts
```

Exit codes: `0` everything done or skipped, `1` at least one file failed, `2` usage error.

**Settings → Advanced → Install Command-Line Tool** links `/usr/local/bin/radlet` to the app's helper.

</details>

<details>
<summary><strong>Shortcuts</strong></summary>

After installing the command-line tool, any shortcut can use Radlet through a **Run Shell Script** action. Set **Pass Input** to **as arguments**, then use a script such as:

```sh
/usr/local/bin/radlet convert --to jpeg "$@"
```

Other examples: `radlet compress --target 2MB "$@"`, `radlet resize --max 1600 "$@"`, `radlet loadout "Web-ready image" "$@"`. Add `--json` to get one JSON line per file, which **Get Dictionary from Input** can read. Use the full path, because Shortcuts doesn't read your shell's `PATH`.

</details>

<details>
<summary><strong>Loadouts</strong></summary>

A loadout is a named set of steps in a TOML file. Put loadouts in `~/Library/Application Support/Radlet/Loadouts/` (or `$RADLET_LOADOUTS_DIR`), and they appear on the Control wheel and in `radlet loadouts`. Loadouts used to be called recipes: `radlet recipe`, `radlet recipes` and older files still work.

```toml
name = "Web-ready image"
description = "Longest side 2048 px, WebP at quality 80, no metadata"

[[steps]]
op = "resize"
max = 2048

[[steps]]
op = "convert"
to = "webp"
quality = 80

[[steps]]
op = "strip_metadata"
```

Steps: `convert {to, quality?}`, `resize {max | percent | width | height}`, `rotate {degrees}`, `crop {rect}`, `redact {rects, style?}`, `strip_metadata`, `compress {level?: light|balanced|strong|smart, target_bytes?}`, `pages {pages, rotate?}`, `page_numbers {position?, style?, start?, size?, skip_first?}`, `watermark {text, opacity?, size?, diagonal?}`, `trim {start?, end?}`, `normalize_audio`, `remove_background {crop?}`, `trace {color?, detail?: low|medium|high}`, `make_searchable`, `transcribe {to?: srt|vtt|txt, language?}`, `blur_faces {style?: pixelate|blur|black}`, `translate {to, from?}`, `redact_text {words?, emails?, phones?, cards?}`, `clean_text`, `format_json {minify?, sort_keys?}`, `format_xml {minify?}`, `optimize_svg`. A `rect` is `{x, y, w, h}` in fractions of the picture; times are in seconds. Image steps share one decode and one encode, so chains never compress twice.

You don't have to write TOML: **Save as Loadout…** in a tool window, or **Settings → Loadouts**, writes the file for you. Saving never overwrites another loadout; a name that's taken gets a number. Passwords can't go in a loadout. [examples/loadouts/thumbnail.toml](examples/loadouts/thumbnail.toml) is a starting point.

</details>

<details>
<summary><strong>AI agents (<code>radlet mcp</code>)</strong></summary>

`radlet mcp` is a Model Context Protocol server on stdin and stdout; **Settings → Advanced** has the configuration to paste into Claude or another AI app. Agents get the wheel's tools, run by the same engine: originals are never changed, results never overwrite a file, and nothing is uploaded.

| Tools | What they do |
| --- | --- |
| `inspect_files` | What each file is, what it can become, which steps fit it, and the extension it should have when its name is wrong |
| `convert`, `compress`, `run_steps` | Any conversion; compressing by level or to a size; chains of the loadout steps above |
| `combine_into_pdf`, `combine_documents`, `combine_into_workbook` | Several files into one PDF, one document, or one Excel workbook |
| `split_pdf`, `scan_documents` | PDFs into parts; photos of pages into one searchable PDF |
| `extract_text`, `count_words`, `read_codes`, `checksum`, `privacy_check`, `check_syntax`, `data_uri` | Reading without writing: text, word counts, QR codes and barcodes, checksums (SHA-256, MD5, SHA-1, SHA-512), what a file reveals, whether a data file is well formed, data: URIs |
| `make_qr`, `make_icons`, `base64` | A QR code of some text or a text file; favicon and Xcode icon sets; Base64 encoding and decoding |
| `convert_folder`, `merge_folders`, `flatten_folders`, `sort_by_kind`, `list_folder_contents`, `find_duplicates`, `zip` | Folders: convert what's inside, merge, flatten, sort, list, find duplicates, zip |
| `join_clips`, `burn_subtitles`, `extract_frames`, `make_subtitles` | Audio and video (offered when this Mac can do them) |
| `suggest_names`, `describe_pictures`, `summarize` | A name from what's in a file; alt text and summaries by Apple Intelligence, when it's on |
| `list_loadouts`, `run_loadout` | Your loadouts |

Password tools and your own shortcuts aren't offered: passwords never pass through an agent's conversation, and an agent can only run steps, not shortcuts.

</details>

<details>
<summary><strong>Helper protocol (<code>radlet serve</code>)</strong></summary>

JSON, one object per line. Every request gets exactly one reply with the same `id`. Loadouts keep their earlier name, recipes, in the protocol.

| Method | Params | Result |
| --- | --- | --- |
| `hello` | none | `version`, `protocol` (1), `capabilities` (`ffmpeg`, `ffprobe`, `sips`, `pdfium`, `vision`, `avfoundation`, `apple_intelligence`) |
| `inspect` | `paths`, `recipes_dir?`, `installs?`, `shortcuts?` | `files` (detected formats, and `fix_extension` when a file's name doesn't match what it holds), `wheel` (`convert`, `tools`, `recipes` slots). A slot may carry `variants`: presets with a short `label`, a `hint` and their own `action`, shown by the Orbit and Bloom styles. With `installs`, slots that need something this Mac can get come after the rest, with `needs`: `ffmpeg`, `keynote`, `pages` or `numbers`. Conversions come most useful first; `more: true` marks one to a format fewer people want, which the app keeps behind More…. `shortcuts` (names) adds those shortcuts to the `recipes` slots, as `run_shortcut` actions. |
| `shortcuts` | none | `shortcuts`: the names in the Shortcuts app (macOS) |
| `content_names` | `paths` | `names`: a short name for what's in each file (or `null`), for Rename's `{content}` |
| `run` | `paths`, `action`, `options?`, `isolate?` | `job`, `units`, then events: `item_started`, `item_progress`, `item_finished`, `job_finished`. An outcome lists everything it created in `outputs` (several page images, or one folder). A failed outcome's `error` may carry `need` (the four above, `speech_language` or `translation_language`). |
| `refresh` | none | `ffmpeg`, `keynote`, `pages`, `numbers`. Looks again for FFmpeg and the iWork apps, after one was installed. |
| `cancel` | `job` | `cancelled` |
| `recipes` | `recipes_dir?` | `recipes` (built-ins marked `builtin`, the user's with their `path`), `errors` |
| `privacy` | `paths` | `reports`: per file, `findings` (`kind`, `label`, `detail`, `removable`, `latitude`/`longitude` for places) or an `error` |
| `save_recipe` | `recipe`, `recipes_dir?`, `replace?` | `path`. A new file never overwrites; `replace` rewrites one loadout file in the loadouts folder. |
| `draft_recipe` | `text` | `recipe`: a loadout drafted on-device by Apple Intelligence from a sentence (at most 300 characters), checked, not saved. Without Apple Intelligence: an error with code `needs` |
| `parse_recipe` | `text` | `recipe`: a shared loadout (TOML, at most 16 KB) checked and read, not saved |
| `recipe_toml` | `recipe` | `text`: the loadout as TOML, to share as a file or link |
| `shutdown` | none | `{}`, then the helper exits |

</details>

## Current status

Version 0.3.0, working towards 1.0.

- **Releases come from tags.** Each `v*` tag builds a universal `.dmg` and publishes it on the Releases page, with an appcast for installed copies once the update key is set up. Until the Developer ID secrets are added, builds are ad-hoc signed and not notarized, so the first open needs **Open Anyway**.
- **What CI checks.** On Linux: formatting, clippy, every engine test (including the end-to-end, protocol, FFmpeg and PDFium tests) and the licence policy. On macOS: the engine tests again, the Swift tests, a live protocol contract against the freshly built engine, the app bundle build, and a smoke test of the bundled helpers, including `radlet-vision selftest`.
- **Tested on a Mac.** The newest pieces (Hide faces, Translate, Share, Copy image, Next… and ⌃⌥V) have been run on a real Mac, on top of the Linux end-to-end tests.
- **About the demo video.** The wheel frames are drawn by Radlet's own SwiftUI code, using slots the real engine returned for three HEIC photos. The Finder window, pointer and progress HUD were recreated for the video, and the photos are generated. Its title and end cards were re-lettered when Veer became Radlet.

<details>
<summary><strong>Known limits</strong></summary>

- Resize, rotate and crop refuse moving pictures (animated GIF, WebP and PNG) rather than flatten them. Converting one to a still format keeps the first frame. Removing metadata from a GIF is lossless and keeps every frame.
- Word files are read for their text and structure: headings, lists, quotes, code, tables, bold, italic and links. Pictures, comments, footnotes, headers and footers aren't carried over, and Word output uses Calibri and simple styles. Word 97 and ODT files go through macOS's `textutil`, which keeps the text and simple formatting (paragraphs, lists, tables, bold and italic) but usually not heading levels. PowerPoint files give their text, not their layout, unless Keynote is installed to make the PDF.
- PDF to Word keeps the text, in paragraphs and lists, not the page layout, columns or pictures.
- Without FFmpeg, a Mac converts only what AVFoundation reads (MP4, MOV, M4A, MP3, WAV, AIFF, FLAC) into MP4, MOV, M4A, WAV or AIFF, plus GIFs into video. More than two sound channels are mixed to stereo in WAV and AIFF.
- Camera RAW files are developed with macOS's default settings, as Preview shows them.
- Password-protected 7-Zip and RAR archives can't be opened (macOS's libarchive doesn't decrypt them); locked ZIPs can. An XML file converted to JSON and back keeps its elements, attributes and text, but not comments or the order of mixed text and elements.
- Excel workbooks are read for their values: a formula gives the result Excel last saved, and formatting, charts and merged cells aren't carried over. Workbooks are refused if they'd unpack to more than 512 MB.
- Hide faces finds the faces Vision detects; very small, turned-away or covered faces can be missed, so check the result before sharing. Read code reads QR codes everywhere, and barcodes too on a Mac (through Vision).
- Describe and Summarize need Apple Intelligence turned on (macOS 26 on Apple silicon); without it, Radlet offers to open its settings. The on-device model reads text, not pictures, so a description is written from what Vision recognises in the picture and any text in it: check it before publishing. Summaries read up to about 48,000 characters, from the start, and are marked as written by Apple Intelligence.
- Translate needs macOS 26 and the two languages downloaded in System Settings → General → Language & Region → Translation Languages. Each paragraph is translated as a whole, so bold or italic words inside one come out plain.
- Without the Mac app, documents become PDF through a built-in writer with the standard PDF fonts (Helvetica, Courier), which cover Western European languages; other letters print as "?".
- Text recognition, Remove background and searchable PDFs need the Mac app (macOS 14 or later); the engine on Linux doesn't offer them. A searchable PDF made from a PDF is redrawn page by page, so links, form fields and bookmarks aren't carried over.
- PDF to image needs the bundled PDFium. Password-protected PDFs are refused by the other PDF tools; remove the password first with **Password…**.
- A PDF with only an owner password (editing restrictions, no password to open it) opens without one, so Radlet's PDF tools treat it as unlocked and their results drop those restrictions.
- Make subtitles needs the Mac app (macOS 14 or later), permission for speech recognition (Radlet asks the first time) and the language's on-device model, which macOS downloads when Dictation is turned on for it. macOS grants this per app, so from Terminal or an AI app subtitles work only if that app has been allowed speech recognition. Accuracy is that of macOS dictation: good for clear speech, weaker with music, crosstalk or strong accents.
- Smart compression measures JPEG and WebP; other formats use Balanced. If the original is already as small as it can get without visible change, Radlet says so instead of making a worse copy.
- Page numbers and watermarks use Helvetica, so they take Latin letters, numbers and punctuation only.
- Shrinking a PDF recompresses its 8-bit RGB and greyscale pictures; text, vector drawings and CMYK pictures are left as they are.
- Trim, Make GIF and Even out volume re-encode what they keep, so cuts are exact but take as long as a conversion.
- Crop and Redact need a preview, so the app can't crop video formats AVFoundation can't open (MKV, WebM, AVI). The CLI can.
- Compress to a target size for video needs `ffprobe` and works out a bitrate from the duration, so the result lands near the target rather than exactly on it.
- PDF to text only returns selectable text. Run **Make searchable PDF** on a scan first.

</details>

## Roadmap

- [x] Publish the first release.
- [x] Updates inside the app (Sparkle): run `mac/scripts/setup-updates.sh` once to switch them on.
- [ ] Sign with a Developer ID and notarize, so the first open needs no workaround. The workflow is ready; it needs the certificate and API key as secrets.
- [x] Rename the GitHub repository to `radlet`.
- [x] Publish the landing page on Vercel and GitHub Pages.
- [x] Everyday audio and video without FFmpeg, through AVFoundation.
- [x] RAW, PSD, JPEG XL, JPEG 2000, Word 97, ODT, old Excel, ODS, YAML, TOML, XML, ASS, SBV, TTML, 7-Zip, RAR, TAR and ISO; PDF to Word; barcodes.
- [x] PowerPoint files, password-protected ZIPs and other ZIP compression methods.
- [x] Trace pictures into SVG drawings.
- [x] Share loadouts and skins as files and links, with a gallery on the website.
- [x] Your own shortcuts on the Control wheel.
- [x] On-device Apple Intelligence: Describe, Summarize, names from what's in a file, and a loadout from a sentence.
- [x] Every wheel tool for AI agents over MCP, except passwords and shortcuts.
- [ ] Native Shortcuts actions (App Intents), in a release built with Xcode.

## License

There's no open-source licence yet. `Cargo.toml` marks the code `LicenseRef-Proprietary`, so all rights are reserved for now, even though the source is public.

Every dependency is permissively licensed (MIT, Apache-2.0, BSD, Zlib, ISC), and `cargo deny check licenses` enforces it. See [THIRD_PARTY.md](THIRD_PARTY.md).

Radlet is clean-room work. No code, assets or text were taken from other apps, and no other app's source code was opened. Behaviour was specified from public material only: websites, store listings and reviews.
