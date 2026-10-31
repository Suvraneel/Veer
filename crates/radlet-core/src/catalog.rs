//! Which conversions and tools are possible, given the formats in a
//! selection and the helper programs available on this machine.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

use crate::engines::avmedia;
use crate::error::Need;
use crate::format::{Category, Format};
use crate::ops::{
    Action, CompressLevel, HashKind, IconKind, Rect, RedactStyle, SplitMode, Step, TraceDetail,
};
use crate::recipe::Recipe;

/// Optional external programs. Everything else is built in.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Capabilities {
    /// FFmpeg, for audio and video.
    pub ffmpeg: Option<PathBuf>,
    /// FFprobe, for progress and lossless remuxing. Optional even with FFmpeg.
    pub ffprobe: Option<PathBuf>,
    /// macOS `sips`, for HEIC and AVIF.
    pub sips: Option<PathBuf>,
    /// The PDFium library, for turning PDF pages into images.
    #[serde(default)]
    pub pdfium: Option<PathBuf>,
    /// The app's `radlet-vision` helper (macOS), for text recognition,
    /// background removal and searchable PDFs.
    #[serde(default)]
    pub vision: Option<PathBuf>,
    /// The helper can also convert audio and video with AVFoundation (on a
    /// Mac). Used only when FFmpeg is missing.
    #[serde(default)]
    pub avfoundation: bool,
    /// macOS `textutil`, for Word 97 and OpenDocument text.
    #[serde(default)]
    pub textutil: Option<PathBuf>,
    /// libarchive's `bsdtar` (macOS's `tar`), for 7-Zip, RAR, TAR and ISO.
    #[serde(default)]
    pub bsdtar: Option<PathBuf>,
    /// Keynote, Pages and Numbers, which export their own documents (and
    /// PowerPoint, for Keynote).
    #[serde(default)]
    pub keynote: bool,
    #[serde(default)]
    pub pages: bool,
    #[serde(default)]
    pub numbers: bool,
    /// macOS's `shortcuts` tool, to run the person's own shortcuts.
    #[serde(default)]
    pub shortcuts: Option<PathBuf>,
    /// This Mac can have Apple Intelligence's on-device language model
    /// (macOS 26 on Apple silicon, through the Vision helper). Whether
    /// it's turned on is only known when it's asked.
    #[serde(default)]
    pub apple_intelligence: bool,
}

impl Capabilities {
    /// Looks for helper programs: `RADLET_FFMPEG`/`RADLET_FFPROBE` overrides,
    /// next to the running binary (bundled builds), then `PATH` and the
    /// usual Homebrew locations. `RADLET_NO_FFMPEG=1` turns FFmpeg off (a Mac
    /// still handles what AVFoundation can); `RADLET_NO_AVFOUNDATION=1` turns
    /// that off too.
    pub fn detect() -> Capabilities {
        let no_ffmpeg = std::env::var_os("RADLET_NO_FFMPEG").is_some_and(|v| v == "1");
        let ffmpeg = if no_ffmpeg {
            None
        } else {
            find_tool("ffmpeg", "RADLET_FFMPEG")
        };
        let ffprobe = if ffmpeg.is_some() {
            find_tool("ffprobe", "RADLET_FFPROBE")
        } else {
            None
        };
        let sips = Some(PathBuf::from("/usr/bin/sips")).filter(|p| is_executable(p));
        let no_vision = std::env::var_os("RADLET_NO_VISION").is_some_and(|v| v == "1");
        let vision = if no_vision {
            None
        } else {
            find_tool("radlet-vision", "RADLET_VISION")
        };
        let no_avfoundation = std::env::var_os("RADLET_NO_AVFOUNDATION").is_some_and(|v| v == "1");
        let avfoundation = cfg!(target_os = "macos") && vision.is_some() && !no_avfoundation;
        let textutil = Some(PathBuf::from("/usr/bin/textutil")).filter(|p| is_executable(p));
        // macOS's own tar is bsdtar; elsewhere it's a separate install.
        let bsdtar = find_tool("bsdtar", "RADLET_BSDTAR").or_else(|| {
            Some(PathBuf::from("/usr/bin/tar"))
                .filter(|p| cfg!(target_os = "macos") && is_executable(p))
        });
        use crate::engines::iwork::App;
        let no_iwork = std::env::var_os("RADLET_NO_IWORK").is_some_and(|v| v == "1");
        let iwork = |app: App| !no_iwork && app.installed();
        let apple_intelligence = vision.is_some() && apple_intelligence_possible();
        Capabilities {
            apple_intelligence,
            ffmpeg,
            ffprobe,
            sips,
            pdfium: find_pdfium(),
            vision,
            avfoundation,
            textutil,
            bsdtar,
            keynote: iwork(App::Keynote),
            pages: iwork(App::Pages),
            numbers: iwork(App::Numbers),
            shortcuts: std::env::var_os("RADLET_SHORTCUTS")
                .map(PathBuf::from)
                .or_else(|| cfg!(target_os = "macos").then(|| PathBuf::from("/usr/bin/shortcuts")))
                .filter(|p| is_executable(p)),
        }
    }

    /// Process-wide capabilities, detected once.
    pub fn global() -> &'static Capabilities {
        static CAPS: OnceLock<Capabilities> = OnceLock::new();
        CAPS.get_or_init(Capabilities::detect)
    }

    pub fn none() -> Capabilities {
        Capabilities::default()
    }

    pub fn media(&self) -> bool {
        self.ffmpeg.is_some()
    }

    /// Audio and video through the Mac helper's AVFoundation: what a Mac
    /// plays, without FFmpeg.
    pub fn native_media(&self) -> bool {
        self.ffmpeg.is_none() && self.avfoundation && self.vision.is_some()
    }

    /// Formats that can be decoded into pixels (an SVG is drawn).
    pub fn can_decode_raster(&self, f: Format) -> bool {
        f.is_builtin_raster() || f == Format::Svg || (self.sips.is_some() && f.is_sips_raster())
    }

    /// Documents whose text and structure can be read here.
    pub fn reads_document(&self, f: Format) -> bool {
        f.is_document_text() && (!f.needs_textutil() || self.textutil.is_some())
    }

    /// Formats that pixels can be encoded into.
    pub fn can_encode_raster(&self, f: Format) -> bool {
        f.is_builtin_raster()
            || matches!(f, Format::Icns | Format::Avif | Format::Jxl)
            || (self.sips.is_some() && f == Format::Heic)
    }
}

fn find_tool(name: &str, env_key: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_key).map(PathBuf::from) {
        return is_executable(&p).then_some(p);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir() {
        dirs.push(dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    // Where the app installs Radlet's own FFmpeg build, which holds only
    // these two.
    if matches!(name, "ffmpeg" | "ffprobe") {
        if let Some(dir) = installed_tools_dir() {
            dirs.push(dir);
        }
    }
    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/// Where the app puts programs it installs for Radlet (its FFmpeg build):
/// `~/Library/Application Support/Radlet/FFmpeg`.
pub fn installed_tools_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Radlet/FFmpeg"))
}

/// PDFium: `RADLET_PDFIUM`, or next to the running binary (the app bundles
/// it in Contents/Helpers), or in a sibling Frameworks folder.
fn find_pdfium() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RADLET_PDFIUM").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let name = if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    };
    let exe_dir = exe_dir()?;
    [exe_dir.join(name), exe_dir.join("../Frameworks").join(name)]
        .into_iter()
        .find(|p| p.is_file())
}

/// The folder holding the real binary, with symlinks resolved, so a
/// `/usr/local/bin/radlet` link into the app still finds its bundled
/// libraries and tools.
fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    exe.parent().map(Path::to_path_buf)
}

/// macOS 26 or later on Apple silicon: where Apple Intelligence's
/// on-device model can run. `RADLET_NO_AI=1` says no, and `RADLET_AI=1`
/// says yes (for tests with a stand-in helper).
fn apple_intelligence_possible() -> bool {
    if std::env::var_os("RADLET_NO_AI").is_some_and(|v| v == "1") {
        return false;
    }
    if std::env::var_os("RADLET_AI").is_some_and(|v| v == "1") {
        return true;
    }
    if !(cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")) {
        return false;
    }
    std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
        .ok()
        .and_then(|plist| macos_major(&plist))
        .is_some_and(|major| major >= 26)
}

/// The major version in a SystemVersion.plist: 26 for "26.1".
fn macos_major(plist: &str) -> Option<u32> {
    let after = &plist[plist.find("<key>ProductVersion</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find('<')? + start;
    after[start..end].split('.').next()?.trim().parse().ok()
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

use Format::*;

const IMAGE_TARGETS: &[Format] = &[
    Jpeg, Png, Webp, Heic, Pdf, Tiff, Gif, Avif, Ico, Icns, Bmp, Jxl, Tga, Qoi,
];
/// A drawing keeps its transparency best as PNG; its PDF stays vector.
const SVG_TARGETS: &[Format] = &[
    Png, Jpeg, Webp, Pdf, Ico, Icns, Tiff, Gif, Avif, Jxl, Bmp, Tga, Qoi, Heic,
];
const AUDIO_TARGETS: &[Format] = &[Mp3, M4a, Wav, Flac, Aiff, Ogg, Opus, Wma];
const VIDEO_TARGETS: &[Format] = &[Mp4, Mov, Webm, Mkv, Gif, Mp3, M4a, Avi, Wmv];

/// The conversions most people want from `from`, most useful first; the
/// first four take the wheel's quickest directions. Whatever else
/// `targets` offers is for fewer people: those slots are marked `more` and
/// wait behind "More…".
fn popular_targets(from: Format) -> &'static [Format] {
    match from {
        // An animation is most often wanted as a video.
        Gif => &[Mp4, Png, Jpeg, Webp, Mov, Pdf],
        // Camera and design files go on to be edited or printed.
        Raw | Psd => &[Jpeg, Png, Tiff, Pdf, Heic, Webp],
        Svg => &[Png, Pdf, Jpeg, Webp],
        Pdf => &[Docx, Jpeg, Png, Txt, Md],
        Pptx | Key => &[Pdf, Key, Pptx, Docx, Txt, Md],
        Numbers => &[Pdf, Xlsx, Csv],
        Csv | Tsv => &[Xlsx, Csv, Json, Pdf, Md],
        Xlsx | Xls | Ods | Sqlite => &[Xlsx, Csv, Pdf, Json, Md],
        Json => &[Csv, Xlsx, Yaml],
        Yaml | Toml | Xml => &[Json, Yaml],
        Ttf | Otf | Woff | Woff2 => &[Woff2, Ttf, Otf],
        f => match f.category() {
            Category::Image => &[Jpeg, Png, Pdf, Webp, Heic, Svg, Txt],
            Category::Audio => &[Mp3, M4a, Wav, Flac],
            Category::Video => &[Mp4, Mov, Mp3, Gif, M4a, Webm],
            Category::Document | Category::Text => &[Pdf, Docx, Md, Txt, Html],
            Category::Subtitle => &[Srt, Vtt, Txt],
            Category::Archive => &[Folder],
            Category::Folder => &[Zip],
            Category::Data | Category::Font => &[],
        },
    }
}

/// True when converting `from` to `to` is for fewer people, and waits
/// behind "More…".
pub fn is_rare(from: Format, to: Format) -> bool {
    let popular = popular_targets(from);
    !popular.is_empty() && !popular.contains(&to)
}

/// The formats `from` can become: the popular ones first, most useful
/// first, then the rest (see `popular_targets`).
pub fn targets(from: Format, caps: &Capabilities) -> Vec<Format> {
    let mut out: Vec<Format> = Vec::new();
    if caps.can_decode_raster(from) {
        let list = if from == Svg {
            SVG_TARGETS
        } else {
            IMAGE_TARGETS
        };
        out.extend(
            list.iter()
                .copied()
                .filter(|&t| t == Pdf || caps.can_encode_raster(t)),
        );
    }
    // Pictures trace into drawings.
    if traces(from, caps) {
        out.push(Svg);
    }
    // Text recognition reads images too.
    if caps.vision.is_some() && caps.can_decode_raster(from) && from != Svg {
        out.push(Txt);
    }
    match from.category() {
        Category::Audio if caps.media() => out.extend_from_slice(AUDIO_TARGETS),
        Category::Video if caps.media() => out.extend_from_slice(VIDEO_TARGETS),
        Category::Audio | Category::Video if caps.native_media() => {
            out.extend_from_slice(avmedia::targets(from))
        }
        _ => {}
    }
    if from == Gif && caps.media() {
        out.extend_from_slice(&[Mp4, Webm, Mov]);
    } else if from == Gif && caps.native_media() {
        out.extend_from_slice(avmedia::FROM_GIF);
    }
    match from {
        Pdf => {
            out.push(Docx);
            if caps.pdfium.is_some() {
                out.extend_from_slice(&[Jpeg, Png, Webp, Tiff]);
            }
            out.extend_from_slice(&[Txt, Md, Html, Rtf]);
        }
        Srt | Vtt | Ass | Sbv | Ttml => out.extend_from_slice(&[Srt, Vtt, Txt, Ass, Ttml, Sbv]),
        Doc | Odt if caps.textutil.is_some() => {
            out.extend_from_slice(&[Pdf, Docx, Md, Txt, Html, Rtf, Epub])
        }
        Txt => out.extend_from_slice(&[Pdf, Docx, Md, Html, Rtf, Epub, Srt, Vtt]),
        Md => out.extend_from_slice(&[Pdf, Html, Docx, Txt, Rtf, Epub]),
        Html => out.extend_from_slice(&[Pdf, Md, Docx, Txt, Rtf, Epub]),
        Docx => out.extend_from_slice(&[Pdf, Md, Txt, Html, Rtf, Epub]),
        Rtf => out.extend_from_slice(&[Pdf, Docx, Md, Txt, Html, Epub]),
        Epub => out.extend_from_slice(&[Pdf, Docx, Md, Txt, Html, Rtf]),
        Pptx => {
            out.extend_from_slice(&[Pdf, Docx, Md, Txt, Html, Rtf, Epub]);
            out.extend(crate::engines::iwork::targets(caps, Pptx));
        }
        Key | Pages | Numbers => out.extend(crate::engines::iwork::targets(caps, from)),
        Csv => out.extend_from_slice(&[Xlsx, Json, Tsv, Md, Html, Pdf]),
        Tsv => out.extend_from_slice(&[Xlsx, Csv, Json, Md, Html, Pdf]),
        Json => out.extend_from_slice(&[Csv, Xlsx, Tsv, Yaml, Toml, Xml]),
        Yaml => out.extend_from_slice(&[Json, Toml]),
        Toml => out.extend_from_slice(&[Json, Yaml]),
        Xml => out.push(Json),
        Xlsx => out.extend_from_slice(&[Csv, Json, Tsv, Pdf, Html, Md]),
        Xls | Ods | Sqlite => out.extend_from_slice(&[Xlsx, Csv, Json, Tsv, Pdf, Html, Md]),
        Ttf | Otf => out.extend_from_slice(&[Woff2, Woff]),
        Woff | Woff2 => out.extend_from_slice(&[Ttf, Otf, Woff2, Woff]),
        Folder => {
            out.push(Zip);
            if caps.bsdtar.is_some() {
                out.push(Tar);
            }
        }
        Zip => out.push(Folder),
        f if f.needs_bsdtar() && caps.bsdtar.is_some() => out.push(Folder),
        _ => {}
    }
    let mut seen = Vec::with_capacity(out.len());
    out.retain(|&t| {
        t != from && !seen.contains(&t) && {
            seen.push(t);
            true
        }
    });
    // Stable: the rest keep the order they were added in.
    let popular = popular_targets(from);
    out.sort_by_key(|t| popular.iter().position(|p| p == t).unwrap_or(popular.len()));
    out
}

/// True for pictures that can be traced into an SVG drawing.
fn traces(f: Format, caps: &Capabilities) -> bool {
    f != Svg && caps.can_decode_raster(f)
}

pub fn can_convert(from: Format, to: Format, caps: &Capabilities) -> bool {
    targets(from, caps).contains(&to)
}

/// Targets offered for a whole selection: every file must either convert
/// to the target or already be in it, and at least one file must change.
pub fn selection_targets(formats: &[Option<Format>], caps: &Capabilities) -> Vec<Format> {
    let known: Vec<Format> = match formats.iter().copied().collect::<Option<Vec<_>>>() {
        Some(k) if !k.is_empty() => k,
        _ => return Vec::new(),
    };
    let mut candidates: Vec<Format> = Vec::new();
    for f in &known {
        for t in targets(*f, caps) {
            if !candidates.contains(&t) {
                candidates.push(t);
            }
        }
    }
    candidates.retain(|&t| known.iter().all(|&f| f == t || can_convert(f, t, caps)));
    // Popular for every file first, keeping the first file's order.
    candidates.sort_by_key(|&t| selection_rare(&known, t));
    candidates
}

/// True when `to` is for fewer people for some file in the selection.
fn selection_rare(known: &[Format], to: Format) -> bool {
    known.iter().any(|&f| f != to && is_rare(f, to))
}

/// Which formats a step accepts, used for tools and recipe validation.
pub fn step_accepts(step: &Step, f: Format, caps: &Capabilities) -> bool {
    match step {
        Step::Convert { to, .. } => f == *to || can_convert(f, *to, caps),
        Step::Resize { .. } => caps.can_decode_raster(f) && (caps.can_encode_raster(f) || f == Svg),
        Step::Rotate { .. } => (caps.can_decode_raster(f) && caps.can_encode_raster(f)) || f == Pdf,
        Step::PageNumbers { .. } | Step::Watermark { .. } => f == Pdf,
        Step::Trim { .. } => {
            (caps.media() && matches!(f.category(), Category::Audio | Category::Video))
                || (caps.native_media() && avmedia::trims(f))
        }
        Step::NormalizeAudio => {
            caps.media() && matches!(f.category(), Category::Audio | Category::Video)
        }
        Step::Crop { .. } | Step::Redact { .. } => {
            (caps.can_decode_raster(f) && caps.can_encode_raster(f))
                || (caps.media() && f.category() == Category::Video)
        }
        Step::Pages { .. } => f == Pdf,
        Step::StripMetadata => {
            caps.can_decode_raster(f) && caps.can_encode_raster(f)
                || f == Pdf
                || (caps.media() && matches!(f.category(), Category::Audio | Category::Video))
        }
        Step::Compress { target_bytes, .. } => {
            matches!(f, Jpeg | Png | Webp | Tiff | Bmp | Pdf)
                || (caps.sips.is_some() && f == Heic)
                // A size target works out a bitrate from the length, which
                // needs FFprobe.
                || (caps.media()
                    && f.category() == Category::Video
                    && (target_bytes.is_none() || caps.ffprobe.is_some()))
                // A Mac compresses to a smaller picture or to a size itself.
                || (caps.native_media() && avmedia::compresses(f))
        }
        Step::RemoveBackground { .. } => {
            caps.vision.is_some() && caps.can_decode_raster(f) && f != Svg
        }
        Step::MakeSearchable => {
            caps.vision.is_some() && (f == Pdf || (caps.can_decode_raster(f) && f != Svg))
        }
        Step::Transcribe { to, .. } => {
            caps.vision.is_some()
                && caps.media()
                && caps.ffprobe.is_some()
                && matches!(to, Srt | Vtt | Txt)
                && matches!(f.category(), Category::Audio | Category::Video)
        }
        // Not CSV or TSV: trailing tabs are empty cells, and quoted cells
        // can hold line breaks of their own.
        Step::CleanText => matches!(f, Txt | Md | Html | Json | Srt | Vtt),
        Step::BlurFaces { .. } => {
            caps.vision.is_some()
                && caps.can_decode_raster(f)
                && caps.can_encode_raster(f)
                && f != Svg
        }
        Step::Translate { to, from } => {
            caps.vision.is_some()
                && crate::engines::speech::valid_language(to)
                && from
                    .as_deref()
                    .is_none_or(crate::engines::speech::valid_language)
                && matches!(f, Txt | Md | Html | Docx | Rtf | Srt | Vtt)
        }
        Step::FormatJson { .. } => f == Json,
        Step::FormatXml { .. } => matches!(f, Xml | Svg),
        Step::OptimizeSvg => f == Svg,
        Step::RedactText { .. } => caps.vision.is_some() && f == Pdf,
        Step::Trace { .. } => traces(f, caps),
    }
}

/// Format a step produces from `f` (assuming it accepts it).
pub fn step_output(step: &Step, f: Format) -> Format {
    match step {
        Step::Convert { to, .. } => *to,
        // Transparency needs a format with an alpha channel.
        Step::RemoveBackground { .. } if matches!(f, Png | Webp | Tiff) => f,
        Step::RemoveBackground { .. } => Png,
        Step::MakeSearchable => Pdf,
        Step::Transcribe { to, .. } => *to,
        Step::Trace { .. } => Svg,
        // A drawing resized becomes pixels.
        Step::Resize { .. } if f == Svg => Png,
        Step::Compress { .. } if f.category() == Category::Video => Mp4,
        Step::Compress { .. } if matches!(f, Tiff | Bmp) => Jpeg,
        // Video crops and redactions are re-encoded as H.264.
        Step::Crop { .. } | Step::Redact { .. } if f.category() == Category::Video => {
            if f == Mov {
                Mov
            } else {
                Mp4
            }
        }
        _ => f,
    }
}

/// True if every step in the chain can run, starting from `f`.
pub fn chain_accepts(steps: &[Step], mut f: Format, caps: &Capabilities) -> bool {
    if steps.is_empty() {
        return false;
    }
    for step in steps {
        if !step_accepts(step, f, caps) {
            return false;
        }
        f = step_output(step, f);
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotGroup {
    Convert,
    Tool,
    Recipe,
}

/// One segment of the wheel. The shell sends `action` back unchanged to
/// run it, so the wheel needs no knowledge of formats or tools.
#[derive(Debug, Clone, Serialize)]
pub struct Slot {
    pub id: String,
    pub label: String,
    /// SF Symbol name.
    pub symbol: String,
    pub group: SlotGroup,
    pub action: Action,
    /// A tool window the shell should open to set options before running
    /// (`resize`, `crop`, `redact`, `compress`, `split_pdf`, `pages_pdf`).
    /// `action` is then the default the window starts from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<&'static str>,
    /// Quick presets of this slot, most useful first. The wheel's Orbit
    /// style shows the first two on its outer ring and Bloom fans out up
    /// to four; the Dial ignores them. A variant runs straight away, never
    /// through a window, and works for every file the slot does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,
    /// Set on a slot shown although it can't run yet, because this Mac
    /// lacks something it can get (FFmpeg, or Keynote, Pages or Numbers).
    /// The app offers to get it, then runs `action`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<Need>,
    /// A conversion fewer people want (a less known format): the app keeps
    /// it behind "More…" even when the wheel has room.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
}

/// One preset of a slot: a short label for the wheel ("Q 95", "50 %"), a
/// line saying what it does, and the action to send back.
#[derive(Debug, Clone, Serialize)]
pub struct Variant {
    /// Unique on the wheel: the slot's id, a dot, then the preset's key.
    pub id: String,
    pub label: String,
    pub hint: String,
    pub action: Action,
}

impl Slot {
    fn with_variants(mut self, variants: Vec<Variant>) -> Slot {
        self.variants = variants;
        self
    }
}

fn variant(slot: &str, key: &str, label: &str, hint: impl Into<String>, action: Action) -> Variant {
    Variant {
        id: format!("{slot}.{key}"),
        label: label.into(),
        hint: hint.into(),
        action,
    }
}

fn steps(steps: Vec<Step>) -> Action {
    Action::Steps { steps, label: None }
}

const MB: u64 = 1_000_000;

fn fit_under(megabytes: u64) -> Action {
    Action::single(Step::Compress {
        level: CompressLevel::Balanced,
        target_bytes: Some(megabytes * MB),
    })
}

/// Presets for converting to `to`. Quality presets need every file to be
/// an image (PDF pages and video frames take other routes); drawings
/// (`svgs`) get sizes instead.
fn convert_variants(
    to: Format,
    id: &str,
    images: bool,
    svgs: bool,
    combinable: bool,
) -> Vec<Variant> {
    let quality = |key: &str, label: &str, q: u8| {
        variant(
            id,
            key,
            label,
            format!("{} at quality {q}", to.label()),
            steps(vec![Step::Convert {
                to,
                quality: Some(q),
            }]),
        )
    };
    let scale = |key: &str, label: &str, percent: u32, hint: &str| {
        variant(
            id,
            key,
            label,
            format!("{} {hint}", to.label()),
            steps(vec![
                Step::resize_percent(percent),
                Step::Convert { to, quality: None },
            ]),
        )
    };
    match to {
        Png | Jpeg | Webp | Tiff if svgs => vec![
            scale("1x", "1×", 100, "at the size the SVG gives"),
            scale("2x", "2×", 200, "at twice the size the SVG gives"),
            scale("4x", "4×", 400, "at four times the size the SVG gives"),
        ],
        Jpeg | Webp if images => vec![
            variant(
                id,
                "smart",
                "Smart",
                format!("{}, the smallest that looks the same", to.label()),
                steps(vec![
                    Step::Convert { to, quality: None },
                    Step::compress(CompressLevel::Smart),
                ]),
            ),
            quality("best", "Best", 95),
            quality("small", "Small", 60),
        ],
        Heic if images => vec![quality("best", "Best", 92), quality("small", "Small", 60)],
        Svg if images => {
            let trace = |key: &str, label: &str, hint: &str, color: bool, detail: TraceDetail| {
                variant(
                    id,
                    key,
                    label,
                    hint,
                    Action::single(Step::Trace { color, detail }),
                )
            };
            vec![
                trace(
                    "color",
                    "Colour",
                    "Traced into shapes, keeping the colours",
                    true,
                    TraceDetail::Medium,
                ),
                trace(
                    "bw",
                    "B & W",
                    "Dark parts as black shapes, the rest transparent",
                    false,
                    TraceDetail::Medium,
                ),
                trace(
                    "simple",
                    "Simple",
                    "Few colours and smooth shapes, for logos",
                    true,
                    TraceDetail::Low,
                ),
                trace(
                    "detailed",
                    "Detailed",
                    "More colours and small shapes kept",
                    true,
                    TraceDetail::High,
                ),
            ]
        }
        Pdf if combinable => vec![variant(
            id,
            "combine",
            "One PDF",
            "Everything in one PDF, in order",
            Action::CombinePdf,
        )],
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Wheel {
    pub convert: Vec<Slot>,
    pub tools: Vec<Slot>,
    pub recipes: Vec<Slot>,
}

/// Adds the person's chosen shortcuts (`names`, from the Shortcuts app) to
/// the Control wheel, after their loadouts. Shortcuts take files, so a
/// selection with a folder gets none.
pub fn add_shortcuts(
    w: &mut Wheel,
    formats: &[Option<Format>],
    names: &[String],
    caps: &Capabilities,
) {
    if caps.shortcuts.is_none() || formats.is_empty() || formats.contains(&Some(Folder)) {
        return;
    }
    for name in names {
        if crate::engines::shortcuts::check_name(name).is_err() {
            continue;
        }
        let id = format!("shortcut:{name}");
        if w.recipes.iter().any(|s| s.id == id) {
            continue;
        }
        w.recipes.push(Slot {
            id,
            label: name.clone(),
            symbol: "square.2.layers.3d".into(),
            group: SlotGroup::Recipe,
            action: Action::RunShortcut { name: name.clone() },
            window: None,
            variants: Vec::new(),
            needs: None,
            more: false,
        });
    }
}

/// Builds the wheel for a selection. `None` entries are files Radlet doesn't
/// recognise; they still get file-level tools such as checksums.
pub fn wheel(formats: &[Option<Format>], caps: &Capabilities, recipes: &[Recipe]) -> Wheel {
    let mut w = Wheel::default();
    if formats.is_empty() {
        return w;
    }
    let known: Option<Vec<Format>> = formats.iter().copied().collect();
    let every =
        |test: &dyn Fn(Format) -> bool| known.as_ref().is_some_and(|k| k.iter().all(|&f| test(f)));
    let images = every(&|f| caps.can_decode_raster(f));
    let svgs = every(&|f| f == Svg);
    let pdfable = |f: Format| {
        f == Pdf
            || caps.can_decode_raster(f)
            || caps.reads_document(f)
            || f.is_workbook()
            || matches!(f, Csv | Tsv)
    };
    let combinable = known
        .as_ref()
        .is_some_and(|k| k.len() >= 2 && k.iter().all(|&f| pdfable(f)));
    let folders = every(&|f| f == Folder);
    let any_folder = formats.contains(&Some(Folder));

    for t in selection_targets(formats, caps) {
        let id = format!("to_{}", t.id());
        let variants = convert_variants(t, &id, images, svgs, combinable);
        let label = if t == Folder { "Unzip" } else { t.label() };
        w.convert.push(Slot {
            id,
            label: label.to_string(),
            symbol: t.category().symbol().to_string(),
            group: SlotGroup::Convert,
            action: Action::convert(t),
            window: None,
            variants,
            needs: None,
            more: known.as_deref().is_some_and(|k| selection_rare(k, t)),
        });
    }

    let all = |step: &Step| every(&|f| step_accepts(step, f, caps));
    let tool =
        |id: &str, label: &str, symbol: &str, action: Action, window: Option<&'static str>| Slot {
            id: id.into(),
            label: label.into(),
            symbol: symbol.into(),
            group: SlotGroup::Tool,
            action,
            window,
            variants: Vec::new(),
            needs: None,
            more: false,
        };
    let pdfs = every(&|f| f == Pdf);
    let single = formats.len() == 1;

    let half = Step::resize_percent(50);
    if svgs {
        let px = |key: &str, label: &str, max: u32| {
            variant(
                "resize",
                key,
                label,
                format!("Drawn with its longest side {max} pixels"),
                Action::single(Step::resize_max(max)),
            )
        };
        w.tools.push(
            tool(
                "resize",
                "Resize…",
                "arrow.up.left.and.arrow.down.right",
                Action::single(Step::resize_max(1024)),
                Some("resize"),
            )
            .with_variants(vec![
                px("1024", "1024 px", 1024),
                px("2048", "2048 px", 2048),
                px("512", "512 px", 512),
                variant(
                    "resize",
                    "4x",
                    "4×",
                    "Four times the size the SVG gives",
                    Action::single(Step::resize_percent(400)),
                ),
            ]),
        );
    } else if all(&half) {
        w.tools.push(
            tool(
                "resize",
                "Resize…",
                "arrow.up.left.and.arrow.down.right",
                Action::single(half.clone()),
                Some("resize"),
            )
            .with_variants(vec![
                variant(
                    "resize",
                    "half",
                    "50 %",
                    "Half the size",
                    Action::single(half),
                ),
                variant(
                    "resize",
                    "1920",
                    "1920 px",
                    "Longest side at most 1920 pixels",
                    Action::single(Step::resize_max(1920)),
                ),
                variant(
                    "resize",
                    "quarter",
                    "25 %",
                    "A quarter of the size",
                    Action::single(Step::resize_percent(25)),
                ),
                variant(
                    "resize",
                    "1280",
                    "1280 px",
                    "Longest side at most 1280 pixels",
                    Action::single(Step::resize_max(1280)),
                ),
            ]),
        );
    }
    let crop = Step::Crop { rect: Rect::FULL };
    if all(&crop) {
        w.tools.push(tool(
            "crop",
            "Crop…",
            "crop",
            Action::single(crop),
            Some("crop"),
        ));
        let redact = Step::Redact {
            rects: vec![],
            style: Default::default(),
        };
        w.tools.push(tool(
            "redact",
            "Redact…",
            "rectangle.dashed.badge.record",
            Action::single(redact),
            Some("redact"),
        ));
    }
    let compress = Step::compress(CompressLevel::Balanced);
    if all(&compress) {
        let c = |key: &str, label: &str, hint: &str, action: Action| {
            variant("compress", key, label, hint, action)
        };
        let strong = Action::single(Step::compress(CompressLevel::Strong));
        let presets = if every(&|f| f.category() == Category::Video) {
            vec![
                c(
                    "whatsapp",
                    "WhatsApp",
                    "Each video under 15 MB, for WhatsApp",
                    fit_under(15),
                ),
                c(
                    "discord",
                    "Discord",
                    "Each video under 19 MB, for Discord",
                    fit_under(19),
                ),
                c(
                    "email",
                    "Email",
                    "Each video under 14 MB, for email",
                    fit_under(14),
                ),
                c("strong", "Strong", "As small as it goes", strong),
            ]
        } else if every(&|f| matches!(f, Jpeg | Webp)) {
            vec![
                c(
                    "smart",
                    "Smart",
                    "The smallest that still looks the same",
                    Action::single(Step::compress(CompressLevel::Smart)),
                ),
                c("1mb", "1 MB", "Each file under 1 MB", fit_under(1)),
                c(
                    "5mb",
                    "5 MB",
                    "Each file under 5 MB, for website uploads",
                    fit_under(5),
                ),
                c("strong", "Strong", "As small as it goes", strong),
            ]
        } else if images {
            vec![
                c("1mb", "1 MB", "Each file under 1 MB", fit_under(1)),
                c(
                    "5mb",
                    "5 MB",
                    "Each file under 5 MB, for website uploads",
                    fit_under(5),
                ),
                c("strong", "Strong", "As small as it goes", strong),
            ]
        } else {
            vec![
                c(
                    "email",
                    "Email",
                    "Each file under 14 MB, for email",
                    fit_under(14),
                ),
                c("strong", "Strong", "As small as it goes", strong),
            ]
        };
        w.tools.push(
            tool(
                "compress",
                "Compress…",
                "archivebox",
                Action::single(compress),
                Some("compress"),
            )
            .with_variants(presets),
        );
    }
    if all(&Step::Rotate { degrees: 90 }) {
        w.tools.push(
            tool(
                "rotate_right",
                "Rotate right",
                "rotate.right",
                Action::single(Step::Rotate { degrees: 90 }),
                None,
            )
            .with_variants(vec![variant(
                "rotate_right",
                "180",
                "180°",
                "Turn upside down",
                Action::single(Step::Rotate { degrees: 180 }),
            )]),
        );
        w.tools.push(tool(
            "rotate_left",
            "Rotate left",
            "rotate.left",
            Action::single(Step::Rotate { degrees: 270 }),
            None,
        ));
    }
    let trim = Step::Trim {
        start: 0.0,
        end: None,
    };
    if all(&trim) {
        w.tools.push(tool(
            "trim",
            "Trim…",
            "timeline.selection",
            Action::single(trim),
            Some("trim"),
        ));
    }
    let videos = caps.media()
        && known
            .as_ref()
            .is_some_and(|k| k.iter().all(|f| f.category() == Category::Video));
    if videos {
        w.tools.push(tool(
            "make_gif",
            "Make GIF…",
            "photo.stack",
            Action::single(Step::Convert {
                to: Gif,
                quality: None,
            }),
            Some("make_gif"),
        ));
        if caps.ffprobe.is_some() {
            let stills = |count: u32| Action::ExtractFrames {
                count,
                format: None,
            };
            w.tools.push(
                tool(
                    "extract_frames",
                    "Save stills",
                    "film.stack",
                    stills(10),
                    None,
                )
                .with_variants(vec![
                    variant(
                        "extract_frames",
                        "5",
                        "5 stills",
                        "5 evenly spaced stills",
                        stills(5),
                    ),
                    variant(
                        "extract_frames",
                        "30",
                        "30 stills",
                        "30 evenly spaced stills",
                        stills(30),
                    ),
                ]),
            );
        }
    }
    let subtitles = Step::Transcribe {
        to: Srt,
        language: None,
    };
    if all(&subtitles) {
        w.tools.push(tool(
            "make_subtitles",
            "Make subtitles…",
            "captions.bubble",
            Action::single(subtitles),
            Some("subtitles"),
        ));
    }
    if all(&Step::NormalizeAudio) {
        w.tools.push(tool(
            "normalize_audio",
            "Even out volume",
            "speaker.wave.2",
            Action::single(Step::NormalizeAudio),
            None,
        ));
    }
    // Clips end to end: all videos, or all sound. Joining reads each clip's
    // length, so FFmpeg needs FFprobe; a Mac joins what it plays.
    let joins = |f: Format| {
        (caps.media() && caps.ffprobe.is_some()) || (caps.native_media() && avmedia::reads(f))
    };
    let all_video = every(&|f| f.category() == Category::Video);
    let all_audio = every(&|f| f.category() == Category::Audio);
    if formats.len() >= 2 && (all_video || all_audio) && every(&joins) {
        w.tools.push(tool(
            "join_media",
            "Join clips",
            "plus.rectangle.on.rectangle",
            Action::JoinMedia,
            None,
        ));
    }
    // Videos with their subtitle files.
    if let Some(k) = known.as_deref() {
        let is_video = |f: &&Format| f.category() == Category::Video;
        let videos = k.iter().filter(is_video).count();
        let subs = k.iter().filter(|f| f.is_subtitles()).count();
        let draws = caps.media()
            || (caps.native_media() && k.iter().filter(is_video).all(|f| avmedia::reads(*f)));
        if videos >= 1 && subs >= 1 && videos + subs == k.len() && draws {
            w.tools.push(tool(
                "burn_subtitles",
                "Burn in subtitles",
                "captions.bubble.fill",
                Action::BurnSubtitles,
                None,
            ));
        }
    }
    if pdfs {
        let split = |mode: SplitMode| Action::SplitPdf { mode };
        w.tools.push(
            tool(
                "split_pdf",
                "Split…",
                "scissors",
                split(SplitMode::EveryPage),
                Some("split_pdf"),
            )
            .with_variants(vec![
                variant(
                    "split_pdf",
                    "pages",
                    "Every page",
                    "One PDF per page",
                    split(SplitMode::EveryPage),
                ),
                variant(
                    "split_pdf",
                    "pairs",
                    "Pairs",
                    "One PDF per two pages",
                    split(SplitMode::Chunks { size: 2 }),
                ),
            ]),
        );
        if single {
            w.tools.push(tool(
                "pages_pdf",
                "Pages…",
                "square.grid.2x2",
                Action::single(Step::Pages {
                    pages: vec![],
                    rotate: vec![],
                }),
                Some("pages_pdf"),
            ));
        }
        w.tools.push(tool(
            "page_numbers",
            "Page numbers…",
            "number.square",
            Action::single(Step::PageNumbers {
                position: Default::default(),
                style: Default::default(),
                start: 1,
                size: None,
                skip_first: false,
            }),
            Some("page_numbers"),
        ));
        let mark = |text: &str| {
            Action::single(Step::Watermark {
                text: text.into(),
                opacity: 0.15,
                size: None,
                diagonal: true,
            })
        };
        w.tools.push(
            tool(
                "watermark",
                "Watermark…",
                "character.textbox",
                mark("DRAFT"),
                Some("watermark"),
            )
            .with_variants(vec![
                variant(
                    "watermark",
                    "draft",
                    "Draft",
                    "\"DRAFT\" across every page",
                    mark("DRAFT"),
                ),
                variant(
                    "watermark",
                    "copy",
                    "Copy",
                    "\"COPY\" across every page",
                    mark("COPY"),
                ),
                variant(
                    "watermark",
                    "sample",
                    "Sample",
                    "\"SAMPLE\" across every page",
                    mark("SAMPLE"),
                ),
            ]),
        );
        w.tools.push(tool(
            "pdf_password",
            "Password…",
            "lock.doc",
            Action::ProtectPdf {
                password: Default::default(),
            },
            Some("pdf_password"),
        ));
    }
    let redact_text = Step::RedactText {
        words: vec![],
        emails: false,
        phones: false,
        cards: false,
    };
    if all(&redact_text) {
        w.tools.push(tool(
            "redact_text",
            "Black out text…",
            "eye.slash",
            Action::single(redact_text),
            Some("redact_text"),
        ));
    }
    // Photos of paper pages, as a scanner would make them.
    if caps.vision.is_some() && images && !svgs && every(&|f| f != Svg) {
        w.tools.push(tool(
            "scan_document",
            "Scan document",
            "doc.viewfinder",
            Action::ScanDocuments,
            None,
        ));
    }
    // Documents.
    let readable = |f: Format| caps.reads_document(f) || f == Pdf;
    if every(&readable) {
        w.tools.push(tool(
            "word_count",
            "Word count",
            "text.word.spacing",
            Action::WordCount,
            None,
        ));
    }
    let vision_copy = all(&Step::MakeSearchable);
    let textual = |f: Format| {
        readable(f) || f.is_subtitles() || matches!(f, Csv | Tsv | Json | Yaml | Toml | Xml)
    };
    if !vision_copy && every(&textual) {
        w.tools.push(tool(
            "copy_text",
            "Copy text",
            "doc.on.clipboard",
            Action::CopyText,
            None,
        ));
    }
    if all(&Step::CleanText) {
        w.tools.push(tool(
            "clean_text",
            "Clean up text",
            "text.badge.checkmark",
            Action::single(Step::CleanText),
            None,
        ));
    }
    let json =
        |minify: bool, sort_keys: bool| Action::single(Step::FormatJson { minify, sort_keys });
    if all(&Step::FormatJson {
        minify: false,
        sort_keys: false,
    }) {
        w.tools.push(
            tool(
                "format_json",
                "Format JSON",
                "curlybraces",
                json(false, false),
                None,
            )
            .with_variants(vec![
                variant(
                    "format_json",
                    "minify",
                    "Minify",
                    "Every space it doesn't need removed",
                    json(true, false),
                ),
                variant(
                    "format_json",
                    "sort",
                    "Sort keys",
                    "Formatted, every object's keys in alphabetical order",
                    json(false, true),
                ),
            ]),
        );
    }
    let xml = |minify: bool| Action::single(Step::FormatXml { minify });
    if svgs {
        w.tools.push(
            tool(
                "optimize_svg",
                "Optimize SVG",
                "wand.and.rays",
                Action::single(Step::OptimizeSvg),
                None,
            )
            .with_variants(vec![variant(
                "optimize_svg",
                "format",
                "Format",
                "Re-indented to read, everything kept",
                xml(false),
            )]),
        );
    } else if all(&Step::FormatXml { minify: false }) {
        w.tools.push(
            tool(
                "format_xml",
                "Format XML",
                "chevron.left.forwardslash.chevron.right",
                xml(false),
                None,
            )
            .with_variants(vec![variant(
                "format_xml",
                "minify",
                "Minify",
                "No whitespace between tags, no comments",
                xml(true),
            )]),
        );
    }
    if known.as_ref().is_some_and(|k| {
        k.iter()
            .all(|&f| crate::engines::devtools::checks_syntax(f))
    }) {
        w.tools.push(tool(
            "check_syntax",
            "Check syntax",
            "checkmark.seal",
            Action::CheckSyntax,
            None,
        ));
    }
    if formats.len() >= 2 && every(&|f| caps.reads_document(f)) {
        w.tools.push(tool(
            "combine_text",
            "Combine into one",
            "doc.on.doc.fill",
            Action::CombineText,
            None,
        ));
    }

    let faces = |style: RedactStyle| Action::single(Step::BlurFaces { style });
    if all(&Step::BlurFaces {
        style: RedactStyle::Pixelate,
    }) {
        w.tools.push(
            tool(
                "blur_faces",
                "Hide faces",
                "face.dashed",
                faces(RedactStyle::Pixelate),
                None,
            )
            .with_variants(vec![
                variant(
                    "blur_faces",
                    "pixelate",
                    "Pixelate",
                    "Faces as large blocks",
                    faces(RedactStyle::Pixelate),
                ),
                variant(
                    "blur_faces",
                    "blur",
                    "Blur",
                    "Faces blurred",
                    faces(RedactStyle::Blur),
                ),
                variant(
                    "blur_faces",
                    "black",
                    "Black",
                    "Faces covered in black",
                    faces(RedactStyle::Black),
                ),
            ]),
        );
    }
    if every(&|f| f == Txt) {
        w.tools.push(tool(
            "make_qr",
            "Make QR code",
            "qrcode",
            Action::MakeQr,
            None,
        ));
    }
    let translate = Step::Translate {
        to: "en".into(),
        from: None,
    };
    if all(&translate) {
        w.tools.push(tool(
            "translate",
            "Translate…",
            "character.bubble",
            Action::single(translate),
            Some("translate"),
        ));
    }

    let cutout = Step::RemoveBackground { crop: false };
    if all(&cutout) {
        w.tools.push(
            tool(
                "remove_background",
                "Remove background",
                "person.and.background.dotted",
                Action::single(cutout),
                None,
            )
            .with_variants(vec![variant(
                "remove_background",
                "crop",
                "And crop",
                "Remove the background and crop to the subject",
                Action::single(Step::RemoveBackground { crop: true }),
            )]),
        );
    }
    if vision_copy {
        w.tools.push(tool(
            "copy_text",
            "Copy text",
            "text.viewfinder",
            Action::CopyText,
            None,
        ));
        w.tools.push(tool(
            "make_searchable",
            "Make searchable PDF",
            "doc.text.magnifyingglass",
            Action::single(Step::MakeSearchable),
            None,
        ));
    }
    // Written on-device by Apple Intelligence.
    if caps.apple_intelligence && caps.vision.is_some() {
        if every(&|f| crate::content::is_picture(f, caps)) {
            w.tools.push(tool(
                "describe",
                "Describe",
                "text.below.photo",
                Action::Describe,
                None,
            ));
        }
        if every(&|f| crate::content::has_text(f, caps)) {
            w.tools.push(tool(
                "summarize",
                "Summarize",
                "list.bullet.rectangle",
                Action::Summarize,
                None,
            ));
        }
    }
    if all(&Step::StripMetadata) {
        // Opens a report of what the files reveal; without that window
        // it removes the metadata straight away.
        w.tools.push(
            tool(
                "privacy_check",
                "Privacy check…",
                "hand.raised",
                Action::single(Step::StripMetadata),
                Some("privacy"),
            )
            .with_variants(vec![variant(
                "privacy_check",
                "strip",
                "Remove all",
                "Remove location, camera and other metadata now",
                Action::single(Step::StripMetadata),
            )]),
        );
        w.tools.push(tool(
            "strip_metadata",
            "Remove metadata",
            "eye.slash",
            Action::single(Step::StripMetadata),
            None,
        ));
    }
    // Pictures: QR codes and icon sets (after the everyday tools).
    if every(&|f| caps.can_decode_raster(f)) {
        // Vision reads barcodes too; the engine alone, QR codes.
        let label = if caps.vision.is_some() {
            "Read code"
        } else {
            "Read QR code"
        };
        w.tools.push(tool(
            "read_codes",
            label,
            "qrcode.viewfinder",
            Action::ReadCodes,
            None,
        ));
        let set = |kind: IconKind| Action::IconSet { set: kind };
        w.tools.push(
            tool(
                "make_icons",
                "Make icons",
                "app.badge",
                set(IconKind::Favicon),
                None,
            )
            .with_variants(vec![
                variant(
                    "make_icons",
                    "favicon",
                    "Favicons",
                    "favicon.ico, PNGs and the HTML for a website",
                    set(IconKind::Favicon),
                ),
                variant(
                    "make_icons",
                    "mac",
                    "macOS",
                    "A macOS .icns icon, 16 to 1024 pixels",
                    Action::convert(Icns),
                ),
                variant(
                    "make_icons",
                    "ios",
                    "iOS",
                    "An app icon set for Xcode",
                    set(IconKind::Ios),
                ),
            ]),
        );
    }
    if formats.len() >= 2 && every(&|f| f.is_workbook() || matches!(f, Csv | Tsv | Json)) {
        w.tools.push(tool(
            "combine_workbook",
            "Combine into workbook",
            "tablecells",
            Action::CombineWorkbook,
            None,
        ));
    }
    if combinable {
        w.tools.push(tool(
            "combine_pdf",
            "Combine into PDF",
            "doc.on.doc",
            Action::CombinePdf,
            None,
        ));
    }

    // Folders, and archives of anything.
    if formats.len() >= 2 {
        w.tools.push(tool(
            "zip_all",
            "Zip together",
            "doc.zipper",
            Action::ZipAll,
            None,
        ));
    }
    if folders {
        if formats.len() >= 2 {
            w.tools.push(tool(
                "merge_folders",
                "Merge folders",
                "folder.badge.plus",
                Action::MergeFolders,
                None,
            ));
        }
        w.tools.push(tool(
            "find_duplicates",
            "Find duplicates",
            "square.on.square",
            Action::FindDuplicates,
            None,
        ));
        w.tools.push(
            tool(
                "list_contents",
                "List contents",
                "list.bullet.indent",
                Action::ListContents { format: None },
                None,
            )
            .with_variants(vec![variant(
                "list_contents",
                "csv",
                "CSV",
                "A spreadsheet of every item, with sizes and dates",
                Action::ListContents { format: Some(Csv) },
            )]),
        );
        w.tools.push(tool(
            "flatten",
            "Flatten",
            "square.stack.3d.down.right",
            Action::Flatten,
            None,
        ));
        w.tools.push(tool(
            "sort_by_kind",
            "Sort by kind",
            "tray.2",
            Action::SortByKind,
            None,
        ));
    }
    // A folder, or several items, into a ZIP that needs a password.
    if folders || formats.len() >= 2 {
        w.tools.push(tool(
            "zip_password",
            "Zip with password…",
            "lock.rectangle.stack",
            Action::ZipLocked {
                password: Default::default(),
            },
            Some("zip_password"),
        ));
    }
    if known.as_ref().is_some_and(|k| {
        k.iter()
            .all(|&f| crate::engines::devtools::data_uri_type(f).is_some())
    }) {
        w.tools.push(tool(
            "data_uri",
            "Copy as data URI",
            "link",
            Action::DataUri,
            None,
        ));
    }
    if !any_folder {
        w.tools.push(tool(
            "base64",
            "Base64",
            "doc.plaintext",
            Action::Base64,
            None,
        ));
        let hash = |algorithm: HashKind| Action::Checksum { algorithm };
        w.tools.push(
            tool(
                "checksum",
                "Copy SHA-256",
                "number",
                Action::checksum(),
                None,
            )
            .with_variants(
                [HashKind::Md5, HashKind::Sha1, HashKind::Sha512]
                    .into_iter()
                    .map(|k| {
                        variant(
                            "checksum",
                            &k.label().to_lowercase().replace('-', ""),
                            k.label(),
                            format!("Copy the {} checksum", k.label()),
                            hash(k),
                        )
                    })
                    .collect(),
            ),
        );
    }

    // Offer only presets every file can take (a size target on video
    // needs FFprobe, for one).
    let runnable = |v: &Variant| match &v.action {
        Action::Steps { steps, .. } => known
            .as_ref()
            .is_some_and(|k| k.iter().all(|&f| chain_accepts(steps, f, caps))),
        _ => true,
    };
    for slot in w.convert.iter_mut().chain(w.tools.iter_mut()) {
        slot.variants.retain(&runnable);
    }

    if let Some(k) = &known {
        for r in recipes {
            if k.iter().all(|&f| chain_accepts(&r.steps, f, caps)) {
                w.recipes.push(Slot {
                    id: format!("recipe_{}", r.slug()),
                    label: r.name.clone(),
                    symbol: "wand.and.stars".into(),
                    group: SlotGroup::Recipe,
                    action: r.action(),
                    window: None,
                    variants: Vec::new(),
                    needs: None,
                    more: false,
                });
            }
        }
    }
    w
}

/// Conversions of what's inside folders: for each kind of file found
/// (`inside`, as from `folder::formats_inside`), the usual targets, run on
/// every such file into a mirrored folder.
/// Whether a file of format `f` is of the kind a folder conversion
/// covers: its category, except that "documents" also takes text,
/// Markdown and HTML, and pictures must be ones Radlet can decode (not the
/// icon files it makes).
pub fn inside_kind(only: Category, f: Format, caps: &Capabilities) -> bool {
    match only {
        Category::Document => {
            (f.category() == Category::Document && !f.needs_textutil()) || caps.reads_document(f)
        }
        Category::Image => f.category() == Category::Image && caps.can_decode_raster(f),
        other => f.category() == other,
    }
}

pub fn inside_slots(inside: &[Format], caps: &Capabilities) -> Vec<Slot> {
    let mut out = Vec::new();
    let has = |test: &dyn Fn(Format) -> bool| inside.iter().any(|&f| test(f));
    let slot = |id: &str, label: &str, kind: Category, to: Format, short: &str| Slot {
        id: format!("inside_{id}"),
        label: label.into(),
        symbol: kind.symbol().into(),
        group: SlotGroup::Convert,
        action: Action::ConvertInside {
            steps: vec![Step::Convert { to, quality: None }],
            only: kind,
            label: Some(short.into()),
        },
        window: None,
        variants: Vec::new(),
        needs: None,
        more: false,
    };
    let image = |f: Format| inside_kind(Category::Image, f, caps);
    for to in [Jpeg, Png, Webp] {
        // Worth offering when some image isn't already in that format.
        if has(&|f| image(f) && f != to) {
            out.push(slot(
                to.id(),
                &format!("Images to {}", to.label()),
                Category::Image,
                to,
                to.label(),
            ));
        }
    }
    if has(&|f| caps.reads_document(f)) {
        out.push(slot(
            "pdf",
            "Documents to PDF",
            Category::Document,
            Pdf,
            "PDF",
        ));
    }
    if caps.media() {
        if has(&|f| f.category() == Category::Audio && f != Mp3) {
            out.push(slot("mp3", "Audio to MP3", Category::Audio, Mp3, "MP3"));
        }
        if has(&|f| f.category() == Category::Video && f != Mp4) {
            out.push(slot("mp4", "Videos to MP4", Category::Video, Mp4, "MP4"));
        }
    } else if caps.native_media() {
        if has(&|f| f.category() == Category::Audio && f != M4a && avmedia::reads(f)) {
            out.push(slot("m4a", "Audio to M4A", Category::Audio, M4a, "M4A"));
        }
        if has(&|f| f == Mov) {
            out.push(slot("mp4", "Videos to MP4", Category::Video, Mp4, "MP4"));
        }
    }
    out
}

/// The wheel for chosen paths: [`wheel`], plus conversions of what's
/// inside when every path is a folder.
pub fn wheel_for(
    paths: &[PathBuf],
    formats: &[Option<Format>],
    caps: &Capabilities,
    recipes: &[Recipe],
) -> Wheel {
    let mut w = wheel(formats, caps, recipes);
    if !formats.is_empty() && formats.iter().all(|f| *f == Some(Folder)) {
        let inside = crate::engines::folder::formats_inside(paths, 20_000);
        w.convert.extend(inside_slots(&inside, caps));
    }
    // A locked ZIP is opened with its password; Unzip alone would fail.
    if !paths.is_empty()
        && formats.iter().all(|f| *f == Some(Zip))
        && paths.iter().any(|p| crate::engines::archive::is_locked(p))
    {
        w.tools.insert(
            0,
            Slot {
                id: "unzip_password".into(),
                label: "Unzip with password…".into(),
                symbol: "lock.open".into(),
                group: SlotGroup::Tool,
                action: Action::UnzipLocked {
                    password: Default::default(),
                },
                window: Some("unzip_password"),
                variants: Vec::new(),
                needs: None,
                more: false,
            },
        );
    }
    w
}

/// Capabilities with something more: FFmpeg, or one of Apple's apps.
fn with_need(caps: &Capabilities, need: Need) -> Capabilities {
    let mut more = caps.clone();
    match need {
        Need::Ffmpeg => {
            more.ffmpeg = Some("ffmpeg".into());
            more.ffprobe = Some("ffprobe".into());
        }
        Need::Keynote => more.keynote = true,
        Need::Pages => more.pages = true,
        Need::Numbers => more.numbers = true,
        Need::SpeechLanguage | Need::TranslationLanguage | Need::AppleIntelligence => {}
    }
    more
}

/// What, if this Mac got it, would let `step` run on `f`: FFmpeg first,
/// then (on a Mac) Keynote, Pages or Numbers.
pub fn would_accept_with(step: &Step, f: Format, caps: &Capabilities) -> Option<Need> {
    let mut options = Vec::new();
    if caps.ffmpeg.is_none() {
        options.push(Need::Ffmpeg);
    }
    if cfg!(target_os = "macos") {
        options.extend([Need::Keynote, Need::Pages, Need::Numbers]);
    }
    options
        .into_iter()
        .find(|&need| step_accepts(step, f, &with_need(caps, need)))
}

/// Adds what the selection could do with something this Mac can get: a
/// slot for each conversion or tool that needs FFmpeg, or (with `mac`)
/// Keynote, Pages or Numbers, marked with what it needs, after the slots
/// that work now. The app shows them with a badge and offers the install.
pub fn add_installable(w: &mut Wheel, formats: &[Option<Format>], caps: &Capabilities, mac: bool) {
    if formats.is_empty() {
        return;
    }
    let mut options: Vec<Need> = Vec::new();
    if caps.ffmpeg.is_none() {
        options.push(Need::Ffmpeg);
    }
    if mac {
        for (need, have) in [
            (Need::Keynote, caps.keynote),
            (Need::Pages, caps.pages),
            (Need::Numbers, caps.numbers),
        ] {
            if !have {
                options.push(need);
            }
        }
    }
    for need in options {
        let could = wheel(formats, &with_need(caps, need), &[]);
        for (have, extra) in [(&mut w.convert, could.convert), (&mut w.tools, could.tools)] {
            for mut slot in extra {
                if have.iter().any(|s| s.id == slot.id) {
                    continue;
                }
                slot.needs = Some(need);
                have.push(slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vision() -> Capabilities {
        Capabilities {
            vision: Some("/x/radlet-vision".into()),
            ..Default::default()
        }
    }

    #[test]
    fn installable_slots_say_what_they_need() {
        let mac = Capabilities {
            vision: Some("/x/radlet-vision".into()),
            avfoundation: true,
            ..Default::default()
        };
        let need = |w: &Wheel, id: &str| {
            w.convert
                .iter()
                .chain(&w.tools)
                .find(|s| s.id == id)
                .map(|s| s.needs)
        };
        // Matroska needs FFmpeg for everything.
        let mut w = wheel(&[Some(Mkv)], &mac, &[]);
        assert!(w.convert.is_empty());
        add_installable(&mut w, &[Some(Mkv)], &mac, true);
        assert_eq!(need(&w, "to_mp4"), Some(Some(Need::Ffmpeg)));
        // What works now stays as it is; the rest is added after it.
        let mut w = wheel(&[Some(Mov)], &mac, &[]);
        let before = w.convert.len();
        add_installable(&mut w, &[Some(Mov)], &mac, true);
        assert_eq!(need(&w, "to_mp4"), Some(None));
        assert_eq!(need(&w, "to_webm"), Some(Some(Need::Ffmpeg)));
        assert_eq!(need(&w, "make_gif"), Some(Some(Need::Ffmpeg)));
        assert!(w.convert[..before].iter().all(|s| s.needs.is_none()));
        assert!(serde_json::to_string(&w)
            .unwrap()
            .contains(r#""needs":"ffmpeg""#));
        // Pages documents need Pages; only a Mac offers it.
        let mut w = wheel(&[Some(Pages)], &mac, &[]);
        add_installable(&mut w, &[Some(Pages)], &mac, true);
        assert_eq!(need(&w, "to_pdf"), Some(Some(Need::Pages)));
        let mut w = wheel(&[Some(Pages)], &mac, &[]);
        add_installable(&mut w, &[Some(Pages)], &mac, false);
        assert_eq!(need(&w, "to_pdf"), None);
        // With FFmpeg, video has nothing left to install.
        let both = Capabilities {
            ffmpeg: Some("/x/ffmpeg".into()),
            ffprobe: Some("/x/ffprobe".into()),
            ..mac.clone()
        };
        let mut w = wheel(&[Some(Mkv)], &both, &[]);
        add_installable(&mut w, &[Some(Mkv)], &both, true);
        assert!(w.convert.iter().chain(&w.tools).all(|s| s.needs.is_none()));
    }

    #[test]
    fn vision_tools_on_the_wheel() {
        let ids = |w: &Wheel| w.tools.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        let image = wheel(&[Some(Png)], &vision(), &[]);
        let tools = ids(&image);
        for id in ["remove_background", "copy_text", "make_searchable"] {
            assert!(tools.contains(&id.to_string()), "{id} in {tools:?}");
        }
        assert!(image.convert.iter().any(|s| s.id == "to_txt"));
        // More than twelve is fine: the app puts the rest under More….

        let pdf = ids(&wheel(&[Some(Pdf)], &vision(), &[]));
        assert!(
            pdf.contains(&"copy_text".to_string()) && pdf.contains(&"make_searchable".to_string())
        );
        assert!(!pdf.contains(&"remove_background".to_string()));

        let none = ids(&wheel(&[Some(Png)], &Capabilities::none(), &[]));
        assert!(!none
            .iter()
            .any(|t| t == "copy_text" || t == "remove_background"));

        assert_eq!(
            step_output(&Step::RemoveBackground { crop: false }, Jpeg),
            Png
        );
        assert_eq!(
            step_output(&Step::RemoveBackground { crop: false }, Webp),
            Webp
        );
        assert_eq!(step_output(&Step::MakeSearchable, Png), Pdf);
    }

    fn media() -> Capabilities {
        Capabilities {
            ffmpeg: Some("/x/ffmpeg".into()),
            ..Default::default()
        }
    }

    #[test]
    fn png_targets_without_helpers() {
        let t = targets(Png, &Capabilities::none());
        assert_eq!(t.first(), Some(&Jpeg));
        assert!(t.contains(&Webp) && t.contains(&Pdf));
        assert!(!t.contains(&Png), "never offers the source format");
        assert!(!t.contains(&Heic), "HEIC needs sips");
    }

    #[test]
    fn media_needs_ffmpeg() {
        assert!(targets(Mp3, &Capabilities::none()).is_empty());
        let t = targets(Mp4, &media());
        assert!(t.contains(&Gif) && t.contains(&Mp3) && !t.contains(&Mp4));
        let g = targets(Gif, &media());
        assert!(g.contains(&Png) && g.contains(&Mp4));
    }

    #[test]
    fn heic_with_sips() {
        let caps = Capabilities {
            sips: Some("/usr/bin/sips".into()),
            ..Default::default()
        };
        assert!(targets(Heic, &caps).contains(&Jpeg));
        assert!(targets(Png, &caps).contains(&Heic));
        assert!(targets(Heic, &Capabilities::none()).is_empty());
    }

    #[test]
    fn selection_rules() {
        let caps = Capabilities::none();
        let mixed = selection_targets(&[Some(Png), Some(Jpeg)], &caps);
        assert!(mixed.contains(&Jpeg), "PNG converts, JPEG already is");
        assert!(mixed.contains(&Webp));
        assert!(selection_targets(&[Some(Png), Some(Srt)], &caps).is_empty());
        assert!(selection_targets(&[Some(Png), None], &caps).is_empty());
    }

    #[test]
    fn wheel_for_images() {
        let w = wheel(&[Some(Png), Some(Jpeg)], &Capabilities::none(), &[]);
        let ids: Vec<&str> = w.tools.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "resize",
                "crop",
                "redact",
                "compress",
                "rotate_right",
                "rotate_left",
                "privacy_check",
                "strip_metadata",
                "read_codes",
                "make_icons",
                "combine_pdf",
                "zip_all",
                "zip_password",
                "data_uri",
                "base64",
                "checksum"
            ]
        );
        let windows: Vec<_> = w.tools.iter().filter_map(|s| s.window).collect();
        assert_eq!(
            windows,
            [
                "resize",
                "crop",
                "redact",
                "compress",
                "privacy",
                "zip_password"
            ]
        );
        assert!(w.convert.iter().any(|s| s.id == "to_webp"));
    }

    #[test]
    fn pdf_tools_and_targets() {
        let w = wheel(&[Some(Pdf)], &Capabilities::none(), &[]);
        let ids: Vec<&str> = w.tools.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "compress",
                "rotate_right",
                "rotate_left",
                "split_pdf",
                "pages_pdf",
                "page_numbers",
                "watermark",
                "pdf_password",
                "word_count",
                "copy_text",
                "privacy_check",
                "strip_metadata",
                "base64",
                "checksum"
            ]
        );
        let windows: Vec<_> = w.tools.iter().filter_map(|s| s.window).collect();
        assert_eq!(
            windows,
            [
                "compress",
                "split_pdf",
                "pages_pdf",
                "page_numbers",
                "watermark",
                "pdf_password",
                "privacy"
            ]
        );
        assert_eq!(
            targets(Pdf, &Capabilities::none()),
            vec![Docx, Txt, Md, Html, Rtf]
        );
        let with_pdfium = Capabilities {
            pdfium: Some("/x/libpdfium.so".into()),
            ..Default::default()
        };
        assert_eq!(
            targets(Pdf, &with_pdfium),
            vec![Docx, Jpeg, Png, Txt, Md, Webp, Tiff, Html, Rtf]
        );
        // Several PDFs: split each, combine, but no single-file page editor.
        let w = wheel(&[Some(Pdf), Some(Pdf)], &Capabilities::none(), &[]);
        let ids: Vec<&str> = w.tools.iter().map(|s| s.id.as_str()).collect();
        assert!(!ids.contains(&"pages_pdf"));
        assert!(ids.contains(&"split_pdf") && ids.contains(&"combine_pdf"));
    }

    #[test]
    fn video_tools_need_ffmpeg() {
        let w = wheel(&[Some(Mp4)], &media(), &[]);
        let ids: Vec<&str> = w.tools.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "crop",
                "redact",
                "compress",
                "trim",
                "make_gif",
                "normalize_audio",
                "privacy_check",
                "strip_metadata",
                "base64",
                "checksum"
            ]
        );
        // Stills need FFprobe to know the length.
        let probed = Capabilities {
            ffprobe: Some("/x/ffprobe".into()),
            ..media()
        };
        let w = wheel(&[Some(Mp4)], &probed, &[]);
        assert!(w.tools.iter().any(|s| s.id == "extract_frames"));
        let audio = wheel(&[Some(Mp3)], &media(), &[]);
        let ids: Vec<&str> = audio.tools.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "trim",
                "normalize_audio",
                "privacy_check",
                "strip_metadata",
                "base64",
                "checksum"
            ]
        );
        let crop = Step::Crop { rect: Rect::FULL };
        assert_eq!(step_output(&crop, Mov), Mov);
        assert_eq!(step_output(&crop, Webm), Mp4);
        assert!(!step_accepts(&crop, Mp4, &Capabilities::none()));
    }

    /// Every preset is unique, named after its slot, short enough for the
    /// outer ring, and runs on every file its slot does.
    fn check_variants(w: &Wheel, formats: &[Format], caps: &Capabilities) {
        let mut seen = std::collections::HashSet::new();
        for s in w.convert.iter().chain(&w.tools).chain(&w.recipes) {
            assert!(s.variants.len() <= 4, "{} has too many presets", s.id);
            for v in &s.variants {
                assert!(v.id.starts_with(&format!("{}.", s.id)), "{}", v.id);
                assert!(seen.insert(v.id.clone()), "duplicate {}", v.id);
                assert!(
                    !v.label.is_empty() && v.label.chars().count() <= 10,
                    "{}",
                    v.label
                );
                assert!(!v.hint.is_empty());
                if let Action::Steps { steps, .. } = &v.action {
                    for &f in formats {
                        assert!(chain_accepts(steps, f, caps), "{} on {f:?}", v.id);
                    }
                }
            }
        }
    }

    fn labels<'a>(w: &'a [Slot], id: &str) -> Vec<&'a str> {
        let slot = w
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no {id}"));
        slot.variants.iter().map(|v| v.label.as_str()).collect()
    }

    #[test]
    fn apple_intelligence_tools_need_a_mac_that_can_have_it() {
        let mac = Capabilities {
            vision: Some("/x/radlet-vision".into()),
            ..Capabilities::none()
        };
        let ai = Capabilities {
            apple_intelligence: true,
            ..mac.clone()
        };
        let ids = |caps: &Capabilities, f: Format| {
            wheel(&[Some(f)], caps, &[])
                .tools
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        };
        assert!(ids(&ai, Png).contains(&"describe".to_string()));
        assert!(!ids(&ai, Png).contains(&"summarize".to_string()));
        assert!(ids(&ai, Pdf).contains(&"summarize".to_string()));
        assert!(ids(&ai, Md).contains(&"summarize".to_string()));
        assert!(
            !ids(&ai, Svg).contains(&"describe".to_string()),
            "drawings aren't photos"
        );
        assert!(!ids(&mac, Png).contains(&"describe".to_string()));
        assert!(!ids(&mac, Pdf).contains(&"summarize".to_string()));
        let mixed = wheel(&[Some(Png), Some(Pdf)], &ai, &[]);
        assert!(!mixed
            .tools
            .iter()
            .any(|s| s.id == "describe" || s.id == "summarize"));
    }

    #[test]
    fn macos_versions_from_the_system_plist() {
        let plist = "<dict>\n\t<key>ProductName</key>\n\t<string>macOS</string>\n\t<key>ProductVersion</key>\n\t<string>26.1</string>\n</dict>";
        assert_eq!(macos_major(plist), Some(26));
        assert_eq!(
            macos_major("<key>ProductVersion</key><string>15.6.1</string>"),
            Some(15)
        );
        assert_eq!(macos_major("<key>ProductName</key>"), None);
    }

    #[test]
    fn chosen_shortcuts_join_the_control_wheel() {
        let caps = Capabilities {
            shortcuts: Some("/usr/bin/shortcuts".into()),
            ..Capabilities::none()
        };
        let names = [
            "Resize for Web".to_string(),
            "Bad\nname".to_string(),
            "Resize for Web".to_string(),
        ];
        let mut w = wheel(&[Some(Png)], &caps, &[]);
        let before = w.recipes.len();
        add_shortcuts(&mut w, &[Some(Png)], &names, &caps);
        assert_eq!(w.recipes.len(), before + 1, "once, and never a bad name");
        let slot = w.recipes.last().unwrap();
        assert_eq!(slot.id, "shortcut:Resize for Web");
        assert!(matches!(&slot.action, Action::RunShortcut { name } if name == "Resize for Web"));
        // No shortcuts tool, or a folder: none.
        let mut none = wheel(&[Some(Png)], &Capabilities::none(), &[]);
        add_shortcuts(&mut none, &[Some(Png)], &names, &Capabilities::none());
        assert!(none.recipes.iter().all(|s| !s.id.starts_with("shortcut:")));
        let mut folder = wheel(&[Some(Folder)], &caps, &[]);
        add_shortcuts(&mut folder, &[Some(Folder)], &names, &caps);
        assert!(folder
            .recipes
            .iter()
            .all(|s| !s.id.starts_with("shortcut:")));
    }

    #[test]
    fn presets_for_the_outer_ring() {
        let caps = Capabilities::none();
        let w = wheel(&[Some(Png), Some(Jpeg)], &caps, &[]);
        check_variants(&w, &[Png, Jpeg], &caps);
        assert_eq!(labels(&w.convert, "to_jpeg"), ["Smart", "Best", "Small"]);
        assert_eq!(labels(&w.convert, "to_pdf"), ["One PDF"]);
        assert!(labels(&w.convert, "to_tiff").is_empty());
        assert_eq!(
            labels(&w.convert, "to_svg"),
            ["Colour", "B & W", "Simple", "Detailed"]
        );
        assert_eq!(
            labels(&w.tools, "resize"),
            ["50 %", "1920 px", "25 %", "1280 px"]
        );
        assert_eq!(labels(&w.tools, "compress"), ["1 MB", "5 MB", "Strong"]);
        assert_eq!(labels(&w.tools, "privacy_check"), ["Remove all"]);
        assert_eq!(labels(&w.tools, "checksum"), ["MD5", "SHA-1", "SHA-512"]);

        // One file has nothing to combine; JPEGs alone get smart quality.
        let one = wheel(&[Some(Jpeg)], &caps, &[]);
        check_variants(&one, &[Jpeg], &caps);
        assert!(labels(&one.convert, "to_pdf").is_empty());
        assert_eq!(labels(&one.tools, "compress")[0], "Smart");

        // Size targets for video need FFprobe for the length.
        let video = wheel(&[Some(Mp4)], &media(), &[]);
        check_variants(&video, &[Mp4], &media());
        assert_eq!(labels(&video.tools, "compress"), ["Strong"]);
        let probed = Capabilities {
            ffprobe: Some("/x/ffprobe".into()),
            ..media()
        };
        let video = wheel(&[Some(Mp4)], &probed, &[]);
        check_variants(&video, &[Mp4], &probed);
        assert_eq!(
            labels(&video.tools, "compress"),
            ["WhatsApp", "Discord", "Email", "Strong"]
        );
        assert_eq!(
            labels(&video.tools, "extract_frames"),
            ["5 stills", "30 stills"]
        );
        assert!(video.convert.iter().all(|s| s.variants.is_empty()));
        let target = Step::Compress {
            level: CompressLevel::Balanced,
            target_bytes: Some(MB),
        };
        assert!(!step_accepts(&target, Mp4, &media()));
        assert!(step_accepts(&target, Mp4, &probed));
        assert!(step_accepts(&target, Jpeg, &media()));

        let pdfs = wheel(&[Some(Pdf), Some(Pdf)], &caps, &[]);
        check_variants(&pdfs, &[Pdf], &caps);
        assert_eq!(labels(&pdfs.tools, "split_pdf"), ["Every page", "Pairs"]);
        assert_eq!(
            labels(&pdfs.tools, "watermark"),
            ["Draft", "Copy", "Sample"]
        );
        assert_eq!(labels(&pdfs.tools, "compress"), ["Email", "Strong"]);

        // Sent only when there are some.
        let json = serde_json::to_value(&one.tools).unwrap();
        let checksum = json
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "base64")
            .unwrap();
        assert!(checksum.get("variants").is_none());
        let resize = json
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "resize")
            .unwrap();
        assert_eq!(resize["variants"][1]["action"]["steps"][0]["max"], 1920);
        assert_eq!(resize["variants"][1]["id"], "resize.1920");
    }

    fn ids(w: &[Slot]) -> Vec<&str> {
        w.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn documents_convert_to_each_other() {
        let caps = Capabilities::none();
        for f in [Txt, Md, Html, Docx, Rtf] {
            let t = targets(f, &caps);
            assert_eq!(t[0], Pdf, "{f:?} to PDF first");
            for other in [Txt, Md, Html, Docx, Rtf] {
                assert_eq!(t.contains(&other), other != f, "{f:?} to {other:?}");
            }
        }
        let w = wheel(&[Some(Md)], &caps, &[]);
        assert_eq!(
            ids(&w.convert),
            ["to_pdf", "to_docx", "to_txt", "to_html", "to_rtf", "to_epub"]
        );
        assert_eq!(
            ids(&w.tools),
            [
                "word_count",
                "copy_text",
                "clean_text",
                "base64",
                "checksum"
            ]
        );
        assert_eq!(w.convert[1].label, "Word");

        // Several documents: combine into one, or into a PDF.
        let w = wheel(&[Some(Docx), Some(Md)], &caps, &[]);
        check_variants(&w, &[Docx, Md], &caps);
        assert_eq!(
            ids(&w.tools),
            [
                "word_count",
                "copy_text",
                "combine_text",
                "combine_pdf",
                "zip_all",
                "zip_password",
                "base64",
                "checksum"
            ]
        );
        assert_eq!(labels(&w.convert, "to_pdf"), ["One PDF"]);

        // Documents and pictures can share a PDF.
        let w = wheel(&[Some(Docx), Some(Png)], &caps, &[]);
        assert_eq!(ids(&w.convert), ["to_pdf"]);
        assert!(ids(&w.tools).contains(&"combine_pdf"));
        assert!(!ids(&w.tools).contains(&"combine_text"));
    }

    /// A Mac with everything Radlet can use.
    fn full_mac() -> Capabilities {
        let p = || Some(std::path::PathBuf::from("/x/tool"));
        Capabilities {
            ffmpeg: p(),
            ffprobe: p(),
            sips: p(),
            pdfium: p(),
            vision: p(),
            avfoundation: true,
            textutil: p(),
            bsdtar: p(),
            keynote: true,
            pages: true,
            numbers: true,
            ..Default::default()
        }
    }

    #[test]
    fn popular_conversions_come_first_and_the_rest_wait_behind_more() {
        let caps = full_mac();
        let ids = |w: &Wheel, more: bool| {
            w.convert
                .iter()
                .filter(|s| s.more == more)
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        };
        for &f in Format::ALL {
            let w = wheel(&[Some(f)], &caps, &[]);
            let first_rare = w.convert.iter().position(|s| s.more);
            if let Some(at) = first_rare {
                assert!(
                    w.convert[at..].iter().all(|s| s.more),
                    "{f:?}: popular before the rest"
                );
                assert!(at > 0, "{f:?}: something popular on the wheel");
            }
            assert!(
                ids(&w, false).len() <= 7,
                "{f:?}: the popular ones fit an 8-slot wheel beside More…"
            );
        }
        let photo = wheel(&[Some(Jpeg)], &caps, &[]);
        assert_eq!(
            ids(&photo, false),
            ["to_png", "to_pdf", "to_webp", "to_heic", "to_svg", "to_txt"]
        );
        assert!(ids(&photo, true).starts_with(&["to_tiff".into(), "to_gif".into()]));
        assert!(ids(&photo, true).contains(&"to_qoi".to_string()));
        let video = wheel(&[Some(Mov)], &caps, &[]);
        assert_eq!(ids(&video, true), ["to_mkv", "to_avi", "to_wmv"]);
        assert_eq!(ids(&wheel(&[Some(Gif)], &caps, &[]), false)[0], "to_mp4");
        // Popular only when it is for every file in the selection.
        let mixed = wheel(&[Some(Png), Some(Jpeg)], &caps, &[]);
        assert!(ids(&mixed, false).contains(&"to_png".to_string()));
        let gif_too = wheel(&[Some(Png), Some(Gif)], &caps, &[]);
        assert!(ids(&gif_too, true).contains(&"to_heic".to_string()));
        // What a Mac could get is ranked the same way.
        let mut mac = full_mac();
        mac.ffmpeg = None;
        mac.ffprobe = None;
        let mut w = wheel(&[Some(Mov)], &mac, &[]);
        add_installable(&mut w, &[Some(Mov)], &mac, false);
        let avi = w.convert.iter().find(|s| s.id == "to_avi").unwrap();
        assert!(avi.more && avi.needs == Some(Need::Ffmpeg));
        // Sent only when set.
        let json = serde_json::to_value(&photo.convert).unwrap();
        assert!(json[0].get("more").is_none());
        assert_eq!(json.as_array().unwrap().last().unwrap()["more"], true);
    }

    #[test]
    fn svg_is_an_image() {
        let caps = Capabilities::none();
        let t = targets(Svg, &caps);
        assert_eq!(&t[..4], [Png, Pdf, Jpeg, Webp]);
        assert!(t.contains(&Ico) && !t.contains(&Heic) && !t.contains(&Txt));
        let w = wheel(&[Some(Svg)], &caps, &[]);
        check_variants(&w, &[Svg], &caps);
        assert_eq!(labels(&w.convert, "to_png"), ["1×", "2×", "4×"]);
        assert!(
            labels(&w.convert, "to_pdf").is_empty(),
            "the PDF stays vector"
        );
        assert_eq!(
            labels(&w.tools, "resize"),
            ["1024 px", "2048 px", "512 px", "4×"]
        );
        assert_eq!(
            ids(&w.tools),
            [
                "resize",
                "optimize_svg",
                "check_syntax",
                "read_codes",
                "make_icons",
                "data_uri",
                "base64",
                "checksum"
            ]
        );
        let resize = Step::resize_max(512);
        assert_eq!(step_output(&resize, Svg), Png);
        assert!(chain_accepts(
            &[
                resize,
                Step::Convert {
                    to: Webp,
                    quality: None
                }
            ],
            Svg,
            &caps
        ));
        // With the Vision helper, still no OCR or cut-outs of drawings.
        let w = wheel(&[Some(Svg)], &vision(), &[]);
        assert!(!ids(&w.tools)
            .iter()
            .any(|t| *t == "remove_background" || *t == "copy_text"));
    }

    #[test]
    fn data_files() {
        let caps = Capabilities::none();
        assert_eq!(targets(Csv, &caps), [Xlsx, Json, Pdf, Md, Tsv, Html]);
        assert_eq!(targets(Json, &caps), [Csv, Xlsx, Yaml, Tsv, Toml, Xml]);
        assert_eq!(targets(Xlsx, &caps), [Csv, Pdf, Json, Md, Tsv, Html]);
        assert_eq!(targets(Yaml, &caps), [Json, Toml]);
        assert_eq!(targets(Xml, &caps), [Json]);
        assert_eq!(targets(Xls, &caps), [Xlsx, Csv, Pdf, Json, Md, Tsv, Html]);
        assert_eq!(targets(Ods, &caps)[0], Xlsx);
        let w = wheel(&[Some(Csv), Some(Xlsx)], &caps, &[]);
        assert!(ids(&w.tools).contains(&"combine_workbook"));
        assert!(ids(&w.tools).contains(&"combine_pdf"));
        let w = wheel(&[Some(Json)], &caps, &[]);
        assert_eq!(
            ids(&w.tools),
            [
                "copy_text",
                "clean_text",
                "format_json",
                "check_syntax",
                "base64",
                "checksum"
            ]
        );
        let w = wheel(&[Some(Csv), Some(Tsv)], &caps, &[]);
        assert_eq!(
            ids(&w.convert),
            ["to_xlsx", "to_json", "to_pdf", "to_md", "to_csv", "to_tsv", "to_html"]
        );
        assert!(
            !ids(&w.tools).contains(&"clean_text"),
            "tables aren't trimmed"
        );
    }

    #[test]
    fn folders_and_archives() {
        let caps = Capabilities::none();
        let one = wheel(&[Some(Folder)], &caps, &[]);
        assert_eq!(ids(&one.convert), ["to_zip"]);
        assert_eq!(
            ids(&one.tools),
            [
                "find_duplicates",
                "list_contents",
                "flatten",
                "sort_by_kind",
                "zip_password"
            ]
        );
        assert_eq!(labels(&one.tools, "list_contents"), ["CSV"]);
        check_variants(&one, &[Folder], &caps);
        let two = wheel(&[Some(Folder), Some(Folder)], &caps, &[]);
        assert_eq!(
            ids(&two.tools),
            [
                "zip_all",
                "merge_folders",
                "find_duplicates",
                "list_contents",
                "flatten",
                "sort_by_kind",
                "zip_password"
            ]
        );
        // A folder with files: zip them together, nothing else.
        let mixed = wheel(&[Some(Folder), Some(Png)], &caps, &[]);
        assert!(mixed.convert.is_empty());
        assert_eq!(ids(&mixed.tools), ["zip_all", "zip_password"]);
        let zip = wheel(&[Some(Zip)], &caps, &[]);
        assert_eq!(ids(&zip.convert), ["to_folder"]);
        assert_eq!(zip.convert[0].label, "Unzip");
        assert_eq!(ids(&zip.tools), ["base64", "checksum"]);
    }

    #[test]
    fn conversions_inside_folders() {
        let caps = Capabilities::none();
        let s = inside_slots(&[Jpeg, Heic, Md, Mp3], &caps);
        let ids: Vec<&str> = s.iter().map(|s| s.id.as_str()).collect();
        // HEIC needs sips; JPEG is already JPEG; no FFmpeg, no audio.
        assert_eq!(ids, ["inside_png", "inside_webp", "inside_pdf"]);
        assert_eq!(s[0].label, "Images to PNG");
        let json = serde_json::to_value(&s[0].action).unwrap();
        assert_eq!(json["kind"], "convert_inside");
        assert_eq!(json["only"], "image");
        assert_eq!(json["label"], "PNG");
        assert!(inside_slots(&[Txt, Zip], &caps)
            .iter()
            .all(|s| s.id == "inside_pdf"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.heic"), "x").unwrap();
        let w = wheel_for(&[dir.path().to_path_buf()], &[Some(Folder)], &caps, &[]);
        assert_eq!(w.convert[0].id, "to_zip");
        assert!(
            w.convert.iter().all(|s| s.id != "inside_jpeg"),
            "HEIC without sips"
        );
    }

    #[test]
    fn developer_tools_where_they_fit() {
        let caps = Capabilities::none();
        let ids = |w: &Wheel| w.tools.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        let presets = |w: &Wheel, id: &str| {
            w.tools
                .iter()
                .find(|s| s.id == id)
                .map(|s| {
                    s.variants
                        .iter()
                        .map(|v| v.label.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let json = wheel(&[Some(Json)], &caps, &[]);
        assert_eq!(presets(&json, "format_json"), ["Minify", "Sort keys"]);
        let xml = wheel(&[Some(Xml)], &caps, &[]);
        assert!(ids(&xml).contains(&"format_xml".to_string()));
        assert_eq!(presets(&xml, "format_xml"), ["Minify"]);
        let svg = wheel(&[Some(Svg)], &caps, &[]);
        assert!(
            !ids(&svg).contains(&"format_xml".to_string()),
            "SVG gets Optimize instead"
        );
        assert_eq!(presets(&svg, "optimize_svg"), ["Format"]);
        for f in [Yaml, Toml, Csv, Tsv, Json, Xml, Svg] {
            assert!(
                ids(&wheel(&[Some(f)], &caps, &[])).contains(&"check_syntax".to_string()),
                "{f:?}"
            );
        }
        assert!(!ids(&wheel(&[Some(Md)], &caps, &[])).contains(&"check_syntax".to_string()));
        // Only when every file has one.
        assert!(
            !ids(&wheel(&[Some(Json), Some(Md)], &caps, &[])).contains(&"check_syntax".to_string())
        );
        for f in [Woff2, Png, Svg] {
            assert!(
                ids(&wheel(&[Some(f)], &caps, &[])).contains(&"data_uri".to_string()),
                "{f:?}"
            );
        }
        assert!(!ids(&wheel(&[Some(Pdf)], &caps, &[])).contains(&"data_uri".to_string()));
        assert!(!ids(&wheel(&[Some(Folder)], &caps, &[])).contains(&"base64".to_string()));
        // Steps the editor and loadouts can chain.
        let sorted = Step::FormatJson {
            minify: false,
            sort_keys: true,
        };
        assert_eq!(sorted.suffix(), Some("sorted"));
        assert!(step_accepts(&Step::FormatXml { minify: true }, Svg, &caps));
        assert!(!step_accepts(&Step::OptimizeSvg, Xml, &caps));
        let json = serde_json::to_value(Action::single(sorted)).unwrap();
        assert_eq!(
            json["steps"][0],
            serde_json::json!({"op": "format_json", "sort_keys": true})
        );
        // A checksum still reads and writes as it always did.
        assert_eq!(
            serde_json::to_value(Action::checksum()).unwrap(),
            serde_json::json!({"kind": "checksum"})
        );
        let md5: Action = serde_json::from_str(r#"{"kind":"checksum","algorithm":"md5"}"#).unwrap();
        assert_eq!(
            md5,
            Action::Checksum {
                algorithm: HashKind::Md5
            }
        );
    }

    #[test]
    fn unknown_files_still_get_checksum() {
        let w = wheel(&[None], &Capabilities::none(), &[]);
        assert!(w.convert.is_empty());
        let ids: Vec<&str> = w.tools.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["base64", "checksum"]);
    }

    #[test]
    fn chains() {
        let caps = Capabilities::none();
        let web = [
            Step::resize_max(1600),
            Step::Convert {
                to: Webp,
                quality: Some(80),
            },
            Step::StripMetadata,
        ];
        assert!(chain_accepts(&web, Jpeg, &caps));
        assert!(!chain_accepts(&web, Pdf, &caps));
        assert!(!chain_accepts(&[], Jpeg, &caps));
    }

    #[test]
    fn a_mac_without_ffmpeg() {
        let mac = Capabilities {
            vision: Some("/x/radlet-vision".into()),
            avfoundation: true,
            ..Default::default()
        };
        assert!(mac.native_media() && !mac.media());
        assert_eq!(targets(Mov, &mac), [Mp4, M4a, Wav, Aiff]);
        assert_eq!(targets(Mp3, &mac), [M4a, Wav, Aiff]);
        assert!(targets(Mkv, &mac).is_empty(), "Matroska needs FFmpeg");
        let ids = |w: &Wheel| w.tools.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        let video = ids(&wheel(&[Some(Mov)], &mac, &[]));
        assert!(video.contains(&"trim".to_string()) && video.contains(&"compress".to_string()));
        assert!(!video.contains(&"make_gif".to_string()), "GIFs need FFmpeg");
        let w = wheel(&[Some(Mp4)], &mac, &[]);
        let compress = w.tools.iter().find(|s| s.id == "compress").unwrap();
        assert!(
            compress.variants.iter().any(|v| v.id == "compress.discord"),
            "a Mac fits video to a size itself"
        );
        assert_eq!(
            targets(Gif, &mac),
            [Mp4, Png, Jpeg, Webp, Mov, Pdf, Tiff, Avif, Ico, Icns, Bmp, Jxl, Tga, Qoi, Svg, Txt]
        );
        let clips = ids(&wheel(&[Some(Mov), Some(Mp4)], &mac, &[]));
        assert!(clips.contains(&"join_media".to_string()), "{clips:?}");
        let subtitled = ids(&wheel(&[Some(Mov), Some(Srt)], &mac, &[]));
        assert!(
            subtitled.contains(&"burn_subtitles".to_string()),
            "{subtitled:?}"
        );
        let mkv = ids(&wheel(&[Some(Mkv), Some(Srt)], &mac, &[]));
        assert!(
            !mkv.contains(&"burn_subtitles".to_string()),
            "Matroska needs FFmpeg"
        );
        let audio = ids(&wheel(&[Some(Mp3)], &mac, &[]));
        assert!(
            !audio.contains(&"trim".to_string()),
            "no MP3 encoder to trim into"
        );
        assert!(ids(&wheel(&[Some(Wav)], &mac, &[])).contains(&"trim".to_string()));
        // With FFmpeg, FFmpeg does it all.
        let both = Capabilities {
            ffmpeg: Some("/x/ffmpeg".into()),
            ..mac.clone()
        };
        assert!(!both.native_media());
        assert!(targets(Mov, &both).contains(&Webm));
    }

    #[test]
    fn formats_read_through_macos() {
        let mac = Capabilities {
            sips: Some("/usr/bin/sips".into()),
            textutil: Some("/usr/bin/textutil".into()),
            bsdtar: Some("/usr/bin/tar".into()),
            ..Default::default()
        };
        for f in [Raw, Psd, Jxl, Jp2] {
            assert!(targets(f, &mac).starts_with(&[Jpeg, Png]), "{f}");
            assert!(targets(f, &Capabilities::none()).is_empty(), "{f}");
        }
        assert_eq!(targets(Doc, &mac), [Pdf, Docx, Md, Txt, Html, Rtf, Epub]);
        assert!(targets(Odt, &Capabilities::none()).is_empty());
        for f in [SevenZ, Rar, Tar, Iso] {
            assert_eq!(targets(f, &mac), [Folder], "{f}");
            assert!(targets(f, &Capabilities::none()).is_empty(), "{f}");
        }
        assert_eq!(targets(Folder, &mac), [Zip, Tar]);
        assert_eq!(targets(Ass, &mac), [Srt, Vtt, Txt, Ttml, Sbv]);
        let ids = |w: &Wheel| w.tools.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        let doc = ids(&wheel(&[Some(Doc)], &mac, &[]));
        assert!(doc.contains(&"word_count".to_string()), "{doc:?}");
        assert!(!ids(&wheel(&[Some(Doc)], &Capabilities::none(), &[]))
            .contains(&"word_count".to_string()));
        let read = wheel(
            &[Some(Png)],
            &Capabilities {
                vision: Some("/x".into()),
                ..Default::default()
            },
            &[],
        );
        assert_eq!(
            read.tools
                .iter()
                .find(|s| s.id == "read_codes")
                .unwrap()
                .label,
            "Read code"
        );
    }
}
