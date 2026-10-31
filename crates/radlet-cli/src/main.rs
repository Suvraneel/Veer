//! `radlet`: command-line front end to the Radlet engine, and the helper
//! process the Mac app talks to (`radlet serve`).

mod mcp;
mod serve;

use std::io::{self, BufReader, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context as _, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use radlet_core::job::{self, Event, Executor, RunOptions, Status};
use radlet_core::ops::{
    HashKind, IconKind, NumberPosition, NumberStyle, PageTurn, Rect, RedactStyle, SplitMode,
    TraceDetail,
};
use radlet_core::{
    catalog, format, Action, Capabilities, CompressLevel, Format, OutputLocation, Recipe, Step,
};

#[derive(Parser)]
#[command(
    name = "radlet",
    version,
    about = "Every tool for your files, on your own computer: convert, edit, tidy. Nothing leaves your machine."
)]
struct Cli {
    /// Print machine-readable JSON lines instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
struct RunArgs {
    /// Files to process.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Write outputs into this folder instead of next to each original.
    #[arg(long, short)]
    out: Option<PathBuf>,
    /// How many files to process at once.
    #[arg(long, short)]
    jobs: Option<usize>,
    /// Per-file time limit in seconds.
    #[arg(long)]
    timeout: Option<u64>,
    /// Run in this process instead of isolated workers (faster, less safe).
    #[arg(long)]
    inline: bool,
}

#[derive(Copy, Clone, ValueEnum)]
enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    fn kind(self) -> HashKind {
        match self {
            Algorithm::Md5 => HashKind::Md5,
            Algorithm::Sha1 => HashKind::Sha1,
            Algorithm::Sha256 => HashKind::Sha256,
            Algorithm::Sha512 => HashKind::Sha512,
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum Style {
    Black,
    Pixelate,
    Blur,
}

impl Style {
    fn redact(self) -> RedactStyle {
        match self {
            Style::Black => RedactStyle::Black,
            Style::Pixelate => RedactStyle::Pixelate,
            Style::Blur => RedactStyle::Blur,
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum IconSetArg {
    /// favicon.ico, PNGs, Apple touch and Android icons, manifest, HTML
    Favicon,
    /// A macOS .icns file
    Mac,
    /// AppIcon.appiconset for Xcode
    Ios,
}

fn parse_rect(s: &str) -> std::result::Result<Rect, String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| "expected four numbers: x,y,w,h".to_string())?;
    let [x, y, w, h] = v[..] else {
        return Err("expected four numbers: x,y,w,h".into());
    };
    let r = Rect { x, y, w, h };
    if r.is_valid() {
        Ok(r)
    } else {
        Err("the area must lie inside the picture (fractions from 0 to 1)".into())
    }
}

fn parse_pages(s: &str) -> std::result::Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("\"{s}\" isn't a page number"))
}

/// "90", "1:30", "1:02:03.5" → seconds.
fn parse_time(s: &str) -> std::result::Result<f64, String> {
    let bad = || format!("\"{s}\" isn't a time (try 90, 1:30 or 1:02:03)");
    let mut total = 0.0;
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() > 3 {
        return Err(bad());
    }
    for p in &parts {
        let v: f64 = p.trim().parse().map_err(|_| bad())?;
        if !v.is_finite() || v < 0.0 {
            return Err(bad());
        }
        total = total * 60.0 + v;
    }
    Ok(total)
}

/// "3:90" → turn page 3 by 90°.
fn parse_turn(s: &str) -> std::result::Result<PageTurn, String> {
    let (page, degrees) = s
        .split_once(':')
        .ok_or_else(|| format!("\"{s}\" should be PAGE:DEGREES, such as 3:90"))?;
    Ok(PageTurn {
        page: parse_pages(page)?,
        degrees: parse_degrees(degrees.trim())?,
    })
}

fn parse_size_arg(s: &str) -> std::result::Result<u64, String> {
    radlet_core::ops::parse_size(s)
        .ok_or_else(|| format!("\"{s}\" isn't a size (try 500KB or 2MB)"))
}

#[derive(Copy, Clone, ValueEnum)]
enum DetailArg {
    /// Few colours and smooth shapes, for logos
    Low,
    /// Most icons and illustrations
    Medium,
    /// More colours and small shapes kept
    High,
}

#[derive(Copy, Clone, ValueEnum)]
enum Position {
    BottomCenter,
    BottomRight,
    BottomLeft,
    TopCenter,
    TopRight,
    TopLeft,
}

#[derive(Copy, Clone, ValueEnum)]
enum NumberLook {
    /// 3
    Number,
    /// Page 3
    Page,
    /// Page 3 of 12
    PageOfTotal,
    /// 3 / 12
    Fraction,
}

#[derive(Copy, Clone, ValueEnum)]
enum StillFormat {
    Jpeg,
    Png,
}

#[derive(Copy, Clone, ValueEnum)]
enum SubtitleFormat {
    Srt,
    Vtt,
    Txt,
}

#[derive(Copy, Clone, ValueEnum)]
enum Level {
    Light,
    Balanced,
    Strong,
    /// Smallest JPEG or WebP that still looks the same
    Smart,
}

#[derive(Subcommand)]
enum Command {
    /// List every format and what it converts to on this machine.
    Formats,
    /// Show the wheel (conversions, tools, loadouts) for some files.
    Inspect {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Your loadouts folder, instead of the usual one.
        #[arg(long = "loadouts-dir", alias = "recipes-dir", value_name = "DIR")]
        recipes_dir: Option<PathBuf>,
    },
    /// Convert files to another format.
    Convert {
        #[command(flatten)]
        run: RunArgs,
        /// Target format (jpg, png, webp, pdf, mp3, mp4, srt, ...).
        #[arg(long, short)]
        to: String,
        /// Quality 1-100 for lossy formats.
        #[arg(long, short, value_parser = clap::value_parser!(u8).range(1..=100))]
        quality: Option<u8>,
    },
    /// Resize images. --max and --percent never enlarge; --width and
    /// --height are exact and keep the aspect ratio. SVGs are drawn at
    /// the size asked (--max sets the longest side; --percent may pass
    /// 100), as PNG.
    Resize {
        #[command(flatten)]
        run: RunArgs,
        /// Longest side in pixels.
        #[arg(long, group = "size")]
        max: Option<u32>,
        /// Scale to this percentage.
        #[arg(long, group = "size", value_parser = clap::value_parser!(u32).range(1..=1000))]
        percent: Option<u32>,
        /// Exact width in pixels.
        #[arg(long, group = "size")]
        width: Option<u32>,
        /// Exact height in pixels.
        #[arg(long, group = "size")]
        height: Option<u32>,
    },
    /// Crop images or videos to an area given as fractions: x,y,w,h
    /// (0.1,0.1,0.8,0.8 keeps the middle 80%).
    Crop {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_parser = parse_rect)]
        rect: Rect,
    },
    /// Cover areas of images or videos (repeat --rect for several), or
    /// black out text in PDFs for real (--text, --emails, --phones,
    /// --cards; macOS app).
    Redact {
        #[command(flatten)]
        run: RunArgs,
        /// An area to cover, as fractions x,y,w,h.
        #[arg(long = "rect", value_parser = parse_rect, required_unless_present_any = ["text", "emails", "phones", "cards"])]
        rects: Vec<Rect>,
        #[arg(long, value_enum, default_value = "black")]
        style: Style,
        /// A word or phrase to black out of PDFs, in any case. Repeat for
        /// several.
        #[arg(long, conflicts_with = "rects")]
        text: Vec<String>,
        /// Black out every email address in PDFs.
        #[arg(long, conflicts_with = "rects")]
        emails: bool,
        /// Black out every phone number in PDFs.
        #[arg(long, conflicts_with = "rects")]
        phones: bool,
        /// Black out every card number in PDFs.
        #[arg(long, conflicts_with = "rects")]
        cards: bool,
    },
    /// Split PDFs: every page (default), every N pages, or ranges.
    Split {
        #[command(flatten)]
        run: RunArgs,
        /// Pages per part.
        #[arg(long, conflicts_with = "ranges")]
        every: Option<u32>,
        /// Ranges such as "1-3, 5, 8-".
        #[arg(long)]
        ranges: Option<String>,
    },
    /// Keep and reorder PDF pages: --order 3 --order 1 keeps pages 3 and 1.
    /// --rotate 3:90 also turns page 3.
    Pages {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_parser = parse_pages)]
        order: Vec<u32>,
        #[arg(long, value_parser = parse_turn)]
        rotate: Vec<PageTurn>,
    },
    /// Number the pages of PDFs.
    Numbers {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_enum, default_value = "bottom-center")]
        position: Position,
        #[arg(long, value_enum, default_value = "number")]
        style: NumberLook,
        /// The first page's number.
        #[arg(long, default_value_t = 1)]
        start: u32,
        /// Text size in points.
        #[arg(long, default_value_t = 10.0)]
        size: f32,
        /// Leave the first page unnumbered.
        #[arg(long)]
        skip_first: bool,
    },
    /// Print faint text across every page of PDFs.
    Watermark {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        text: String,
        /// 0.05 to 1.
        #[arg(long, default_value_t = 0.15)]
        opacity: f32,
        /// Text size in points (default: fill the page).
        #[arg(long)]
        size: Option<f32>,
        /// Level across the middle instead of corner to corner.
        #[arg(long)]
        level: bool,
    },
    /// Lock PDFs with a password (AES-256).
    Protect {
        #[command(flatten)]
        run: RunArgs,
        /// Prefer setting RADLET_PASSWORD instead: other programs can see
        /// command-line arguments, and shells keep them in history.
        #[arg(long, env = "RADLET_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Remove the password from PDFs.
    Unlock {
        #[command(flatten)]
        run: RunArgs,
        /// Prefer setting RADLET_PASSWORD instead: other programs can see
        /// command-line arguments, and shells keep them in history.
        #[arg(long, env = "RADLET_PASSWORD", hide_env_values = true)]
        password: String,
    },
    /// Keep part of audio or video: --start 1:30 --end 2:45.
    Trim {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_parser = parse_time, default_value = "0")]
        start: f64,
        #[arg(long, value_parser = parse_time)]
        end: Option<f64>,
    },
    /// Make a GIF from a video, or part of one.
    Gif {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_parser = parse_time, default_value = "0")]
        start: f64,
        #[arg(long, value_parser = parse_time)]
        end: Option<f64>,
    },
    /// Save evenly spaced stills from videos.
    Frames {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, default_value_t = 10)]
        count: u32,
        #[arg(long, value_enum, default_value = "jpeg")]
        format: StillFormat,
    },
    /// Subtitles or a transcript from the speech in audio and video
    /// (on-device speech recognition; macOS app).
    Subtitles {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_enum, default_value = "srt")]
        to: SubtitleFormat,
        /// Language of the speech, such as en-US (default: the Mac's).
        #[arg(long)]
        language: Option<String>,
    },
    /// Even out the volume of audio and video (-16 LUFS).
    Normalize {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Rotate images clockwise.
    Rotate {
        #[command(flatten)]
        run: RunArgs,
        /// 90, 180 or 270.
        #[arg(long, default_value_t = 90, value_parser = parse_degrees)]
        degrees: u32,
    },
    /// Print the text in images and PDFs, recognised on-device (macOS app).
    Text {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Make images and scanned PDFs into PDFs you can search (macOS app).
    Searchable {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Remove the background of images, keeping the subject (macOS app).
    Cutout {
        #[command(flatten)]
        run: RunArgs,
        /// Trim the result to the subject.
        #[arg(long)]
        crop: bool,
    },
    /// Trace pictures into SVG drawings: logos, icons and line art that stay
    /// sharp at any size.
    Trace {
        #[command(flatten)]
        run: RunArgs,
        /// Dark parts as black shapes, the rest transparent.
        #[arg(long)]
        bw: bool,
        #[arg(long, value_enum, default_value = "medium")]
        detail: DetailArg,
    },
    /// Remove location, camera and other personal metadata.
    Strip {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Make files smaller, optionally to a target size such as 2MB.
    Compress {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_enum, default_value = "balanced")]
        level: Level,
        /// Largest acceptable size, such as 500KB or 2MB.
        #[arg(long, value_parser = parse_size_arg)]
        target: Option<u64>,
    },
    /// Combine images, PDFs, SVGs and documents, in order, into one PDF.
    Combine {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Join documents (TXT, Markdown, HTML, Word, RTF), in order, into one
    /// file in the first one's format; or videos into one MP4, or sound
    /// into one M4A.
    Join {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Draw subtitles into videos so every player shows them. Give each
    /// video and the subtitle file named after it (or one of each).
    Burn {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Make photos of paper pages into one scanned PDF: each page cut out,
    /// squared up and evened out, with searchable text (macOS app).
    Scan {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Count the words in documents and PDFs.
    Words {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Tidy text files: UTF-8, one kind of line ending, no spaces at line
    /// ends, one newline at the end.
    Clean {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Re-indent JSON files, or with --minify remove the spaces.
    Json {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        minify: bool,
        /// Put every object's keys in alphabetical order.
        #[arg(long)]
        sort_keys: bool,
    },
    /// Re-indent XML or SVG files, or with --minify remove the whitespace
    /// between tags and the comments. Elements holding text keep it exactly.
    Xml {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        minify: bool,
    },
    /// Make SVG files smaller without changing how they look: editor data,
    /// metadata, comments and whitespace removed.
    #[command(alias = "svgo")]
    OptimizeSvg {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Check that JSON, YAML, TOML, XML, SVG, CSV and TSV files are well
    /// formed; a problem is shown with its line and column.
    Check {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Print pictures, SVGs and fonts as data: URIs, for CSS and HTML.
    DataUri {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Encode files as Base64 (photo.png.b64), or decode .b64 files.
    Base64 {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Put files and folders, together, into one ZIP archive.
    Zip {
        #[command(flatten)]
        run: RunArgs,
        /// Lock every file in the ZIP with this password (AES-256). Prefer
        /// setting RADLET_ZIP_PASSWORD instead: other programs can see
        /// command-line arguments, and shells keep them in history.
        #[arg(long, env = "RADLET_ZIP_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
    /// Extract archives (ZIP, 7-Zip, RAR, TAR, disc images) beside them.
    Unzip {
        #[command(flatten)]
        run: RunArgs,
        /// The password of a locked ZIP. Prefer setting RADLET_PASSWORD
        /// instead.
        #[arg(long, env = "RADLET_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
    /// Copy two or more folders into one new folder.
    Merge {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Copy folders with every file from their subfolders at the top.
    Flatten {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Copy folders with their files grouped into Images, Documents, ...
    Sort {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Save a list of what's in folders: a tree, or with --csv a table.
    List {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        csv: bool,
    },
    /// Report files with identical contents in folders.
    Duplicates {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Put tables (CSV, TSV, JSON, Excel) into one Excel workbook.
    Workbook {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Print the contents of QR codes in pictures.
    Codes {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Make a QR code picture from each text file.
    Qr {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Make icons from pictures: a favicon set, a macOS .icns, or an iOS
    /// app icon set.
    Icons {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_enum, default_value = "favicon")]
        set: IconSetArg,
    },
    /// Hide the faces in pictures, found on-device (macOS app).
    Faces {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_enum, default_value = "pixelate")]
        style: Style,
    },
    /// Translate documents and subtitles on-device (macOS 26 app).
    Translate {
        #[command(flatten)]
        run: RunArgs,
        /// Language to translate into, such as fr or pt-BR.
        #[arg(long)]
        to: String,
        /// Language of the text (default: detected).
        #[arg(long)]
        from: Option<String>,
    },
    /// Print checksums (SHA-256, or --algorithm md5, sha1 or sha512).
    Checksum {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long, value_enum, default_value = "sha256")]
        algorithm: Algorithm,
    },
    /// Run a loadout by name, or from a .toml file.
    #[command(alias = "recipe")]
    Loadout {
        /// Loadout name, slug or path to a .toml file.
        loadout: String,
        #[command(flatten)]
        run: RunArgs,
        /// Your loadouts folder, instead of the usual one.
        #[arg(long = "loadouts-dir", alias = "recipes-dir", value_name = "DIR")]
        recipes_dir: Option<PathBuf>,
    },
    /// Describe pictures as alt text, written on-device by Apple
    /// Intelligence (macOS 26 app).
    Describe {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Summarise documents and PDFs into "<name> (summary).md", written
    /// on-device by Apple Intelligence (macOS 26 app).
    Summarize {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Run one of your shortcuts (from the Shortcuts app) on each file;
    /// what it returns is saved beside the file (macOS).
    Shortcut {
        /// The shortcut's name, as the Shortcuts app shows it.
        name: String,
        #[command(flatten)]
        run: RunArgs,
    },
    /// List your shortcuts (macOS).
    Shortcuts,
    /// Show what files reveal: location, device, names, dates, hidden
    /// content. `radlet strip` removes what's marked removable.
    Privacy {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// List your loadouts and the built-in ones.
    #[command(alias = "recipes")]
    Loadouts {
        /// Your loadouts folder, instead of the usual one.
        #[arg(long = "loadouts-dir", alias = "recipes-dir", value_name = "DIR")]
        recipes_dir: Option<PathBuf>,
    },
    /// Serve Radlet's tools to AI agents over the Model Context Protocol
    /// (stdin/stdout). Add `radlet mcp` as a local server in your client.
    Mcp {
        /// Your loadouts folder, instead of the usual one.
        #[arg(long = "loadouts-dir", alias = "recipes-dir", value_name = "DIR")]
        recipes_dir: Option<PathBuf>,
    },
    /// Speak the JSON-lines helper protocol on stdin/stdout (used by the app).
    Serve {
        /// Run jobs in this process instead of isolated workers.
        #[arg(long)]
        inline: bool,
    },
    /// Run one unit of work from stdin (used for isolation).
    #[command(hide = true)]
    Worker,
}

fn privacy(files: &[PathBuf], caps: &Capabilities, json_out: bool) -> Result<ExitCode> {
    let mut failed = false;
    for (i, path) in files.iter().enumerate() {
        let report = radlet_core::privacy::check(path, caps);
        if json_out {
            let line = match &report {
                Ok(r) => {
                    serde_json::json!({ "path": path, "format": r.format, "findings": r.findings })
                }
                Err(e) => {
                    serde_json::json!({ "path": path, "error": { "code": e.code(), "message": e.to_string() } })
                }
            };
            println!("{line}");
        } else {
            if i > 0 {
                println!();
            }
            println!("{}", path.display());
            match &report {
                Ok(r) if r.findings.is_empty() => println!("  Nothing found."),
                Ok(r) => {
                    for f in &r.findings {
                        let note = if f.removable {
                            ""
                        } else {
                            "  (stays after radlet strip)"
                        };
                        println!("  {:<22} {}{note}", f.label, f.detail);
                    }
                }
                Err(e) => println!("  ✗ {e}"),
            }
        }
        failed |= report.is_err();
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn parse_degrees(s: &str) -> std::result::Result<u32, String> {
    match s.parse::<u32>() {
        Ok(d @ (90 | 180 | 270)) => Ok(d),
        _ => Err("must be 90, 180 or 270".into()),
    }
}

/// Where saved loadouts live: `RADLET_LOADOUTS_DIR`, or the platform's
/// application-support folder. Until the app has moved them, loadouts
/// saved by an earlier build (when Radlet was Veer and loadouts were
/// recipes) are read from where that build kept them.
pub fn default_recipes_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RADLET_LOADOUTS_DIR") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let (new, old) = if cfg!(target_os = "macos") {
        let support = home.join("Library/Application Support");
        (
            support.join("Radlet/Loadouts"),
            support.join("Veer/recipes"),
        )
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        (base.join("radlet/loadouts"), base.join("veer/recipes"))
    };
    Some(if !new.exists() && old.is_dir() {
        old
    } else {
        new
    })
}

fn recipes(dir: Option<&PathBuf>) -> Vec<Recipe> {
    let mut all = Recipe::builtin();
    if let Some(d) = dir.cloned().or_else(default_recipes_dir) {
        let (mut found, errors) = Recipe::load_dir(&d);
        for (p, e) in errors {
            eprintln!("warning: skipped {}: {e}", p.display());
        }
        all.append(&mut found);
    }
    all
}

fn worker_executor() -> Option<(PathBuf, Vec<String>)> {
    std::env::current_exe()
        .ok()
        .map(|exe| (exe, vec!["worker".to_string()]))
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn run(args: &RunArgs, action: Action, json_out: bool) -> Result<ExitCode> {
    for f in &args.files {
        if !f.exists() {
            bail!("{} doesn't exist", f.display());
        }
    }
    let options = RunOptions {
        output: args
            .out
            .clone()
            .map(OutputLocation::Directory)
            .unwrap_or_default(),
        parallelism: args.jobs,
        timeout_secs: args.timeout,
    };
    let executor = match (args.inline, worker_executor()) {
        (false, Some((program, args))) => Executor::Subprocess { program, args },
        _ => Executor::Inline,
    };
    let is_checksum = matches!(action, Action::Checksum { .. });
    let is_text = matches!(
        action,
        Action::CopyText | Action::ReadCodes | Action::DataUri
    );
    let is_count = matches!(action, Action::WordCount | Action::CheckSyntax);
    let many = args.files.len() > 1;
    let units = job::plan(&args.files, &action, &options.output, &job::new_job_tag());
    install_signal_handlers();
    let cancel = &INTERRUPTED;
    let stdout = Mutex::new(io::stdout());
    // Text is printed at the end in the order the files were given, since
    // files finish in any order.
    let texts: Mutex<Vec<(usize, String, String)>> = Mutex::new(Vec::new());
    let on_event = |ev: Event| {
        let mut out = stdout.lock().expect("stdout");
        if json_out {
            match serde_json::to_string(&ev) {
                Ok(line) => {
                    let _ = writeln!(out, "{line}");
                }
                Err(e) => eprintln!("radlet: couldn't describe an event as JSON: {e}"),
            }
            return;
        }
        let Event::ItemFinished { outcome: o } = ev else {
            return;
        };
        let name = o
            .inputs
            .iter()
            .map(|p| file_name(p))
            .collect::<Vec<_>>()
            .join(", ");
        let reason = o
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_default();
        let line = match o.status {
            Status::Done if is_checksum => {
                format!("{}  {}", o.value.unwrap_or_default(), o.inputs[0].display())
            }
            Status::Done if is_count => {
                let v = o.value.unwrap_or_default();
                if many {
                    format!("{name}: {v}")
                } else {
                    v
                }
            }
            Status::Done if is_text => {
                texts
                    .lock()
                    .expect("texts")
                    .push((o.index, name, o.value.unwrap_or_default()));
                return;
            }
            Status::Done => {
                let mut out_name = o.output.as_deref().map(file_name).unwrap_or_default();
                if o.outputs.len() > 1 {
                    out_name = format!("{out_name} and {} more", o.outputs.len() - 1);
                } else if o.output.as_deref().is_some_and(std::path::Path::is_dir) {
                    out_name.push('/');
                }
                let sizes = match o.bytes_out {
                    Some(b) => format!("{} → {}", human_size(o.bytes_in), human_size(b)),
                    None => human_size(o.bytes_in),
                };
                format!(
                    "✓ {name} → {out_name}  ({sizes}, {:.1} s)",
                    o.millis as f64 / 1000.0
                )
            }
            Status::Skipped => format!("– {name}: {reason}"),
            Status::Failed => format!("✗ {name}: {reason}"),
            Status::Cancelled => format!("✗ {name}: cancelled"),
        };
        // Keep problems out of output meant for pipes (`radlet text a.png > a.txt`).
        if (is_text || is_checksum || is_count) && o.status != Status::Done {
            eprintln!("{line}");
        } else {
            let _ = writeln!(out, "{line}");
        }
    };
    let summary = job::run_batch(
        units,
        &executor,
        &options,
        Capabilities::global(),
        cancel,
        &on_event,
    );
    let mut texts = texts.into_inner().expect("texts");
    texts.sort_by_key(|t| t.0);
    for (_, name, text) in texts {
        if many {
            println!("==> {name} <==\n{text}\n");
        } else {
            println!("{text}");
        }
    }
    if !json_out && !is_checksum && !is_text && !is_count {
        eprintln!(
            "{} done, {} skipped, {} failed",
            summary.done, summary.skipped, summary.failed
        );
    }
    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("radlet: interrupted; partial files were removed");
        return Ok(ExitCode::from(130));
    }
    Ok(if summary.failed > 0 || summary.cancelled > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Set by Ctrl-C or SIGTERM. Running workers are then stopped and their
/// partial files deleted; a second Ctrl-C quits immediately.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(130) };
    }
}

fn install_signal_handlers() {
    #[cfg(unix)]
    // SAFETY: the handler only touches an atomic and calls _exit.
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Passwords read from the environment aren't passed on to the programs
    // Radlet runs (workers, FFmpeg, the Mac helper).
    std::env::remove_var("RADLET_PASSWORD");
    std::env::remove_var("RADLET_ZIP_PASSWORD");
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("radlet: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode> {
    let caps = Capabilities::global();
    let json_out = cli.json;
    match cli.command {
        Command::Formats => {
            if json_out {
                let rows: Vec<_> = Format::ALL
                    .iter()
                    .map(|f| serde_json::json!({ "format": f, "label": f.label(), "category": f.category(), "targets": catalog::targets(*f, caps) }))
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({ "capabilities": caps, "formats": rows })
                );
            } else {
                for f in Format::ALL {
                    let t: Vec<&str> = catalog::targets(*f, caps)
                        .iter()
                        .map(|t| t.label())
                        .collect();
                    let to = if t.is_empty() {
                        "(needs a helper program)".to_string()
                    } else {
                        t.join(", ")
                    };
                    println!("{:<9} → {to}", f.label());
                }
                let have = |b: bool| if b { "found" } else { "not found" };
                println!(
                    "\nFFmpeg: {}   FFprobe: {}   sips: {}",
                    have(caps.ffmpeg.is_some()),
                    have(caps.ffprobe.is_some()),
                    have(caps.sips.is_some())
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Inspect { files, recipes_dir } => {
            let detected: Vec<Option<Format>> =
                files.iter().map(|f| format::detect(f).ok()).collect();
            let wheel = catalog::wheel_for(&files, &detected, caps, &recipes(recipes_dir.as_ref()));
            if json_out {
                println!("{}", serde_json::to_string(&wheel)?);
            } else {
                for (f, d) in files.iter().zip(&detected) {
                    println!(
                        "{}: {}",
                        f.display(),
                        d.map(|d| d.label()).unwrap_or("unknown")
                    );
                }
                let list = |slots: &[radlet_core::Slot]| {
                    slots
                        .iter()
                        .map(|s| s.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                println!("Convert:  {}", list(&wheel.convert));
                println!("Tools:    {}", list(&wheel.tools));
                println!("Loadouts: {}", list(&wheel.recipes));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Convert {
            run: r,
            to,
            quality,
        } => {
            let to = Format::parse(&to).with_context(|| format!("unknown format \"{to}\""))?;
            // Folders: convert what's inside (a folder to ZIP or TAR is
            // packed as it is).
            // (A Keynote, Pages or Numbers package is a folder too, but a
            // document.)
            let folders = !r.files.is_empty()
                && r.files
                    .iter()
                    .all(|f| radlet_core::format::detect(f).is_ok_and(|k| k == Format::Folder));
            if folders && !matches!(to, Format::Zip | Format::Tar) {
                // PDF means documents made into PDFs; otherwise files of
                // the target's own kind (images to JPEG, audio to MP3...).
                let only = if to == Format::Pdf {
                    radlet_core::Category::Document
                } else {
                    to.category()
                };
                return run(
                    &r,
                    Action::ConvertInside {
                        steps: vec![Step::Convert { to, quality }],
                        only,
                        label: None,
                    },
                    json_out,
                );
            }
            run(&r, Action::single(Step::Convert { to, quality }), json_out)
        }
        Command::Resize {
            run: r,
            max,
            percent,
            width,
            height,
        } => {
            if [max, percent, width, height].iter().all(Option::is_none) {
                bail!("give one of --max, --percent, --width or --height");
            }
            run(
                &r,
                Action::single(Step::Resize {
                    max,
                    percent,
                    width,
                    height,
                }),
                json_out,
            )
        }
        Command::Crop { run: r, rect } => run(&r, Action::single(Step::Crop { rect }), json_out),
        Command::Redact {
            run: r,
            rects,
            style,
            text,
            emails,
            phones,
            cards,
        } => {
            let step = if rects.is_empty() {
                Step::RedactText {
                    words: text,
                    emails,
                    phones,
                    cards,
                }
            } else {
                Step::Redact {
                    rects,
                    style: style.redact(),
                }
            };
            run(&r, Action::single(step), json_out)
        }
        Command::Split {
            run: r,
            every,
            ranges,
        } => {
            let mode = match (every, ranges) {
                (Some(size), _) => SplitMode::Chunks { size },
                (None, Some(ranges)) => SplitMode::Ranges { ranges },
                (None, None) => SplitMode::EveryPage,
            };
            run(&r, Action::SplitPdf { mode }, json_out)
        }
        Command::Pages {
            run: r,
            order,
            rotate,
        } => run(
            &r,
            Action::single(Step::Pages {
                pages: order,
                rotate,
            }),
            json_out,
        ),
        Command::Rotate { run: r, degrees } => {
            run(&r, Action::single(Step::Rotate { degrees }), json_out)
        }
        Command::Strip { run: r } => run(&r, Action::single(Step::StripMetadata), json_out),
        Command::Numbers {
            run: r,
            position,
            style,
            start,
            size,
            skip_first,
        } => {
            let position = match position {
                Position::BottomCenter => NumberPosition::BottomCenter,
                Position::BottomRight => NumberPosition::BottomRight,
                Position::BottomLeft => NumberPosition::BottomLeft,
                Position::TopCenter => NumberPosition::TopCenter,
                Position::TopRight => NumberPosition::TopRight,
                Position::TopLeft => NumberPosition::TopLeft,
            };
            let style = match style {
                NumberLook::Number => NumberStyle::Number,
                NumberLook::Page => NumberStyle::Page,
                NumberLook::PageOfTotal => NumberStyle::PageOfTotal,
                NumberLook::Fraction => NumberStyle::Fraction,
            };
            let step = Step::PageNumbers {
                position,
                style,
                start,
                size: Some(size),
                skip_first,
            };
            run(&r, Action::single(step), json_out)
        }
        Command::Watermark {
            run: r,
            text,
            opacity,
            size,
            level,
        } => {
            let step = Step::Watermark {
                text,
                opacity,
                size,
                diagonal: !level,
            };
            run(&r, Action::single(step), json_out)
        }
        Command::Protect { run: r, password } => run(
            &r,
            Action::ProtectPdf {
                password: password.into(),
            },
            json_out,
        ),
        Command::Unlock { run: r, password } => run(
            &r,
            Action::UnlockPdf {
                password: password.into(),
            },
            json_out,
        ),
        Command::Trim { run: r, start, end } => {
            run(&r, Action::single(Step::Trim { start, end }), json_out)
        }
        Command::Gif { run: r, start, end } => {
            let mut steps = Vec::new();
            if start > 0.0 || end.is_some() {
                steps.push(Step::Trim { start, end });
            }
            steps.push(Step::Convert {
                to: Format::Gif,
                quality: None,
            });
            run(&r, Action::Steps { steps, label: None }, json_out)
        }
        Command::Subtitles {
            run: r,
            to,
            language,
        } => {
            let to = match to {
                SubtitleFormat::Srt => Format::Srt,
                SubtitleFormat::Vtt => Format::Vtt,
                SubtitleFormat::Txt => Format::Txt,
            };
            run(
                &r,
                Action::single(Step::Transcribe { to, language }),
                json_out,
            )
        }
        Command::Frames {
            run: r,
            count,
            format,
        } => {
            let format = match format {
                StillFormat::Jpeg => Format::Jpeg,
                StillFormat::Png => Format::Png,
            };
            run(
                &r,
                Action::ExtractFrames {
                    count,
                    format: Some(format),
                },
                json_out,
            )
        }
        Command::Normalize { run: r } => run(&r, Action::single(Step::NormalizeAudio), json_out),
        Command::Text { run: r } => run(&r, Action::CopyText, json_out),
        Command::Searchable { run: r } => run(&r, Action::single(Step::MakeSearchable), json_out),
        Command::Trace { run: r, bw, detail } => run(
            &r,
            Action::single(Step::Trace {
                color: !bw,
                detail: match detail {
                    DetailArg::Low => TraceDetail::Low,
                    DetailArg::Medium => TraceDetail::Medium,
                    DetailArg::High => TraceDetail::High,
                },
            }),
            json_out,
        ),
        Command::Cutout { run: r, crop } => run(
            &r,
            Action::single(Step::RemoveBackground { crop }),
            json_out,
        ),
        Command::Compress {
            run: r,
            level,
            target,
        } => {
            let level = match level {
                Level::Light => CompressLevel::Light,
                Level::Balanced => CompressLevel::Balanced,
                Level::Strong => CompressLevel::Strong,
                Level::Smart => CompressLevel::Smart,
            };
            run(
                &r,
                Action::single(Step::Compress {
                    level,
                    target_bytes: target,
                }),
                json_out,
            )
        }
        Command::Combine { run: r } => run(&r, Action::CombinePdf, json_out),
        Command::Join { run: r } => {
            let media = r.files.iter().all(|p| {
                radlet_core::format::detect(p).is_ok_and(|f| {
                    matches!(
                        f.category(),
                        radlet_core::format::Category::Audio | radlet_core::format::Category::Video
                    )
                })
            });
            let action = if media {
                Action::JoinMedia
            } else {
                Action::CombineText
            };
            run(&r, action, json_out)
        }
        Command::Burn { run: r } => run(&r, Action::BurnSubtitles, json_out),
        Command::Scan { run: r } => run(&r, Action::ScanDocuments, json_out),
        Command::Words { run: r } => run(&r, Action::WordCount, json_out),
        Command::Clean { run: r } => run(&r, Action::single(Step::CleanText), json_out),
        Command::Json {
            run: r,
            minify,
            sort_keys,
        } => run(
            &r,
            Action::single(Step::FormatJson { minify, sort_keys }),
            json_out,
        ),
        Command::Xml { run: r, minify } => {
            run(&r, Action::single(Step::FormatXml { minify }), json_out)
        }
        Command::OptimizeSvg { run: r } => run(&r, Action::single(Step::OptimizeSvg), json_out),
        Command::Check { run: r } => run(&r, Action::CheckSyntax, json_out),
        Command::DataUri { run: r } => run(&r, Action::DataUri, json_out),
        Command::Base64 { run: r } => run(&r, Action::Base64, json_out),
        Command::Zip { run: r, password } => {
            let action = match password {
                Some(p) => Action::ZipLocked { password: p.into() },
                None => Action::ZipAll,
            };
            run(&r, action, json_out)
        }
        Command::Unzip { run: r, password } => {
            let action = match password {
                Some(p) => Action::UnzipLocked { password: p.into() },
                None => Action::convert(Format::Folder),
            };
            run(&r, action, json_out)
        }
        Command::Merge { run: r } => run(&r, Action::MergeFolders, json_out),
        Command::Flatten { run: r } => run(&r, Action::Flatten, json_out),
        Command::Sort { run: r } => run(&r, Action::SortByKind, json_out),
        Command::List { run: r, csv } => run(
            &r,
            Action::ListContents {
                format: csv.then_some(Format::Csv),
            },
            json_out,
        ),
        Command::Duplicates { run: r } => run(&r, Action::FindDuplicates, json_out),
        Command::Workbook { run: r } => run(&r, Action::CombineWorkbook, json_out),
        Command::Codes { run: r } => run(&r, Action::ReadCodes, json_out),
        Command::Qr { run: r } => run(&r, Action::MakeQr, json_out),
        Command::Icons { run: r, set } => {
            let action = match set {
                IconSetArg::Favicon => Action::IconSet {
                    set: IconKind::Favicon,
                },
                IconSetArg::Ios => Action::IconSet { set: IconKind::Ios },
                IconSetArg::Mac => Action::convert(Format::Icns),
            };
            run(&r, action, json_out)
        }
        Command::Faces { run: r, style } => run(
            &r,
            Action::single(Step::BlurFaces {
                style: style.redact(),
            }),
            json_out,
        ),
        Command::Translate { run: r, to, from } => {
            run(&r, Action::single(Step::Translate { to, from }), json_out)
        }
        Command::Checksum { run: r, algorithm } => run(
            &r,
            Action::Checksum {
                algorithm: algorithm.kind(),
            },
            json_out,
        ),
        Command::Loadout {
            loadout,
            run: r,
            recipes_dir,
        } => {
            let found = Recipe::find(&loadout, &recipes(recipes_dir.as_ref()))?;
            run(&r, found.action(), json_out)
        }
        Command::Loadouts { recipes_dir } => {
            let all = recipes(recipes_dir.as_ref());
            if json_out {
                println!("{}", serde_json::to_string(&all)?);
            } else {
                for r in all {
                    println!("{:<24} {}", r.name, r.description.unwrap_or_default());
                }
                if let Some(d) = recipes_dir.or_else(default_recipes_dir) {
                    println!("\nYour loadouts folder: {}", d.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Privacy { files } => privacy(&files, caps, json_out),
        Command::Shortcut { name, run: r } => run(&r, Action::RunShortcut { name }, json_out),
        Command::Describe { run: r } => run(&r, Action::Describe, json_out),
        Command::Summarize { run: r } => run(&r, Action::Summarize, json_out),
        Command::Shortcuts => {
            let names = radlet_core::engines::shortcuts::list(caps)?;
            if json_out {
                println!("{}", serde_json::to_string(&names)?);
            } else {
                for n in names {
                    println!("{n}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Mcp { recipes_dir } => {
            let stdin = BufReader::new(io::stdin());
            let code = mcp::serve(
                stdin,
                Box::new(io::stdout()),
                caps,
                worker_executor(),
                recipes_dir,
            );
            Ok(ExitCode::from(code as u8))
        }
        Command::Serve { inline } => {
            let worker = if inline { None } else { worker_executor() };
            let stdin = BufReader::new(io::stdin());
            let code = serve::serve(stdin, Box::new(io::stdout()), caps, worker);
            Ok(ExitCode::from(code as u8))
        }
        Command::Worker => {
            let code = job::worker_main(&mut io::stdin(), &mut io::stdout(), caps);
            Ok(ExitCode::from(code as u8))
        }
    }
}
