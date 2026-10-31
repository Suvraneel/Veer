//! What a job does: a chain of per-file steps, or a whole-selection action.

use serde::{Deserialize, Serialize};

use crate::format::Format;

/// A region of an image or video frame, as fractions (0 to 1) of the
/// upright picture's width and height. Fractions keep the app and engine
/// agreeing on positions whatever the pixel size or orientation tags.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// True when the rectangle lies inside the picture and has an area.
    pub fn is_valid(&self) -> bool {
        const EPS: f64 = 1e-6;
        [self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.w > 0.0
            && self.h > 0.0
            && self.x + self.w <= 1.0 + EPS
            && self.y + self.h <= 1.0 + EPS
    }

    /// Pixel rectangle `(x, y, w, h)` inside a `width` x `height` picture,
    /// at least one pixel in each direction.
    pub fn to_pixels(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let px = |f: f64, size: u32| ((f.clamp(0.0, 1.0) * size as f64).round() as u32).min(size);
        let x0 = px(self.x, width).min(width.saturating_sub(1));
        let y0 = px(self.y, height).min(height.saturating_sub(1));
        let x1 = px(self.x + self.w, width).max(x0 + 1).min(width);
        let y1 = px(self.y + self.h, height).max(y0 + 1).min(height);
        (x0, y0, x1 - x0, y1 - y0)
    }
}

/// How redacted areas are covered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactStyle {
    /// Solid black. The only style that can't be partly reversed.
    #[default]
    Black,
    /// Large blocks of averaged colour.
    Pixelate,
    /// A heavy blur.
    Blur,
}

/// One transformation in a chain. Image steps are applied to decoded pixels
/// and encoded once at the end, so "resize then convert to JPEG" never
/// compresses twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Change format. `quality` is 1-100 for lossy formats. A PDF converted
    /// to an image format produces one image per page.
    Convert {
        to: Format,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<u8>,
    },
    /// Resize by exactly one of: longest side at most `max` pixels, a
    /// `percent` scale (both never enlarge), or an exact `width` or
    /// `height` keeping the aspect ratio. An SVG has no pixels to lose, so
    /// it is drawn afresh at the size asked: `max` sets its longest side
    /// and `percent` may go above 100.
    Resize {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        percent: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    /// Rotate clockwise by 90, 180 or 270 degrees: an image, or every page
    /// of a PDF.
    Rotate { degrees: u32 },
    /// Keep only `rect` of an image or video.
    Crop { rect: Rect },
    /// Cover `rects` of an image (any style) or video (black).
    Redact {
        rects: Vec<Rect>,
        #[serde(default)]
        style: RedactStyle,
    },
    /// Remove location, camera and other personal metadata. Lossless for
    /// JPEG, PNG, WebP, GIF, PDF and media files.
    StripMetadata,
    /// Make the file smaller while keeping its format where possible. With
    /// `target_bytes`, aim for a file at most that large.
    Compress {
        #[serde(default)]
        level: CompressLevel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_bytes: Option<u64>,
    },
    /// Keep these pages of a PDF, in this order (numbered from 1), turning
    /// any listed in `rotate`. Reorders, deletes and rotates in one step.
    Pages {
        pages: Vec<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rotate: Vec<PageTurn>,
    },
    /// Number every page of a PDF.
    PageNumbers {
        #[serde(default)]
        position: NumberPosition,
        #[serde(default)]
        style: NumberStyle,
        /// The first page's number.
        #[serde(default = "first_number")]
        start: u32,
        /// Text size in points (default 10).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        /// Leave the first page (a cover) unnumbered; numbering still
        /// counts it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        skip_first: bool,
    },
    /// Print `text` faintly across every page of a PDF.
    Watermark {
        text: String,
        /// 0.05 (barely there) to 1 (solid).
        #[serde(default = "watermark_opacity")]
        opacity: f32,
        /// Text size in points; by default it fills the page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        /// Corner to corner (default), or level across the middle.
        #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
        diagonal: bool,
    },
    /// Keep `start` to `end` seconds of audio or video (`end` absent: to
    /// the end). Cuts are exact, so the kept part is re-encoded.
    Trim {
        #[serde(default)]
        start: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<f64>,
    },
    /// Even out loudness to a steady level (EBU R128, -16 LUFS), as
    /// podcasts and streaming services do. Video is copied unchanged.
    NormalizeAudio,
    /// Make everything but the main subject transparent (Vision helper).
    /// With `crop`, trim the picture to the subject.
    RemoveBackground {
        #[serde(default)]
        crop: bool,
    },
    /// Turn an image or scanned PDF into a PDF whose text can be searched,
    /// selected and copied (Vision helper).
    MakeSearchable,
    /// Subtitles (`srt`, `vtt`) or a transcript (`txt`) of the speech in
    /// audio or video, by on-device speech recognition (Mac helper).
    /// `language` is a tag such as "en-US"; absent, the Mac's language.
    Transcribe {
        #[serde(default = "subtitle_format")]
        to: Format,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// Hide every face in a picture (found on-device by the Vision
    /// helper) as `style`: pixelated (default), blurred or black.
    BlurFaces {
        #[serde(default = "pixelate")]
        style: RedactStyle,
    },
    /// Translate a document or subtitles into the language `to` (a tag
    /// such as "fr" or "pt-BR"), on-device (Mac helper, macOS 26). `from`
    /// absent: detected.
    Translate {
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// Tidy a text file: UTF-8 without a byte-order mark, one kind of line
    /// ending (`\n`), no spaces at line ends, one newline at the end.
    CleanText,
    /// Re-indent a JSON file (two spaces), or with `minify` remove every
    /// space it doesn't need. Keys keep their order, or with `sort_keys`
    /// every object's keys go in alphabetical order, for stable diffs.
    FormatJson {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        minify: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        sort_keys: bool,
    },
    /// Re-indent an XML or SVG file (two spaces), or with `minify` remove
    /// the whitespace between tags and the comments. Elements that hold
    /// text keep it exactly, spaces included.
    FormatXml {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        minify: bool,
    },
    /// Make an SVG smaller without changing how it looks: editor data
    /// (Inkscape, Sketch, Illustrator), metadata, comments, empty groups
    /// and the whitespace between tags go.
    OptimizeSvg,
    /// Trace a picture into an SVG drawing of filled shapes, for logos,
    /// icons and line art that should stay sharp at any size. `color`
    /// (default) keeps the colours; without it dark areas become black
    /// shapes and the rest is left transparent. `detail` trades likeness
    /// for fewer, smoother shapes.
    Trace {
        #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
        color: bool,
        #[serde(default)]
        detail: TraceDetail,
    },
    /// Black out text in a PDF for real (Mac helper): every `words` phrase
    /// (any case), and with the flags every email address, phone number or
    /// card number. Pages with a match become pictures with black boxes
    /// and a fresh text layer, so nothing under a box can be copied.
    RedactText {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        words: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        emails: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        phones: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cards: bool,
    },
}

fn pixelate() -> RedactStyle {
    RedactStyle::Pixelate
}

fn subtitle_format() -> Format {
    Format::Srt
}

/// How closely a traced drawing follows the picture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceDetail {
    /// Few colours and smooth shapes: logos and flat artwork.
    Low,
    /// What most icons and illustrations want.
    #[default]
    Medium,
    /// Many colours and small shapes kept: detailed artwork.
    High,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressLevel {
    Light,
    #[default]
    Balanced,
    Strong,
    /// The smallest JPEG or WebP that still looks the same, found by
    /// measuring visual similarity (SSIM) against the original. Other
    /// files are compressed as Balanced.
    Smart,
}

impl CompressLevel {
    pub fn image_quality(self) -> u8 {
        match self {
            CompressLevel::Light => 85,
            CompressLevel::Balanced | CompressLevel::Smart => 72,
            CompressLevel::Strong => 55,
        }
    }

    pub fn video_crf(self) -> u8 {
        match self {
            CompressLevel::Light => 23,
            CompressLevel::Balanced | CompressLevel::Smart => 27,
            CompressLevel::Strong => 31,
        }
    }
}

impl Step {
    pub fn resize_max(max: u32) -> Step {
        Step::Resize {
            max: Some(max),
            percent: None,
            width: None,
            height: None,
        }
    }

    pub fn resize_percent(percent: u32) -> Step {
        Step::Resize {
            max: None,
            percent: Some(percent),
            width: None,
            height: None,
        }
    }

    pub fn compress(level: CompressLevel) -> Step {
        Step::Compress {
            level,
            target_bytes: None,
        }
    }

    /// Word appended to the output name when a step keeps the format.
    pub fn suffix(&self) -> Option<&'static str> {
        match self {
            Step::Convert { .. } => None,
            Step::Resize { .. } => Some("resized"),
            Step::Rotate { .. } => Some("rotated"),
            Step::Crop { .. } => Some("cropped"),
            Step::Redact { .. } => Some("redacted"),
            Step::StripMetadata => Some("clean"),
            Step::Compress { .. } => Some("compressed"),
            Step::Pages { .. } => Some("edited"),
            Step::PageNumbers { .. } => Some("numbered"),
            Step::Watermark { .. } => Some("watermarked"),
            Step::Trim { .. } => Some("trimmed"),
            Step::NormalizeAudio => Some("normalized"),
            Step::RemoveBackground { .. } => Some("cutout"),
            Step::MakeSearchable => Some("searchable"),
            Step::Transcribe { .. } => Some("transcript"),
            Step::CleanText => Some("cleaned"),
            Step::BlurFaces { .. } => Some("faces hidden"),
            // The output is named after the language instead.
            Step::Translate { .. } => Some("translated"),
            Step::FormatJson { minify: true, .. } => Some("minified"),
            Step::FormatJson {
                sort_keys: true, ..
            } => Some("sorted"),
            Step::FormatJson { .. } => Some("formatted"),
            Step::FormatXml { minify: true } => Some("minified"),
            Step::FormatXml { minify: false } => Some("formatted"),
            Step::OptimizeSvg => Some("optimized"),
            Step::RedactText { .. } => Some("redacted"),
            Step::Trace { .. } => Some("traced"),
        }
    }
}

fn first_number() -> u32 {
    1
}

fn watermark_opacity() -> f32 {
    0.15
}

fn yes() -> bool {
    true
}

/// One page to turn, in a [`Step::Pages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageTurn {
    /// Page number in the original PDF, from 1.
    pub page: u32,
    /// Clockwise: 90, 180 or 270.
    pub degrees: u32,
}

/// Where page numbers go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberPosition {
    #[default]
    BottomCenter,
    BottomRight,
    BottomLeft,
    TopCenter,
    TopRight,
    TopLeft,
}

/// How page numbers read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberStyle {
    /// "3"
    #[default]
    Number,
    /// "Page 3"
    Page,
    /// "Page 3 of 12"
    PageOfTotal,
    /// "3 / 12"
    Fraction,
}

impl NumberStyle {
    pub fn label(self, n: u32, total: u32) -> String {
        match self {
            NumberStyle::Number => n.to_string(),
            NumberStyle::Page => format!("Page {n}"),
            NumberStyle::PageOfTotal => format!("Page {n} of {total}"),
            NumberStyle::Fraction => format!("{n} / {total}"),
        }
    }
}

/// How to split a PDF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum SplitMode {
    /// One PDF per page.
    EveryPage,
    /// Consecutive groups of `size` pages.
    Chunks { size: u32 },
    /// One PDF per range, such as "1-3, 5, 8-" (an open end runs to the
    /// last page).
    Ranges { ranges: String },
}

/// A request from the wheel, the CLI or a recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Run `steps` on every selected file; one output per input (a PDF
    /// turned into images gives one image per page).
    Steps {
        steps: Vec<Step>,
        /// Recipe name, used in output names when the format doesn't change.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Put every selected image, PDF, SVG and document, in order, into
    /// one PDF.
    CombinePdf,
    /// Join the selected documents, in order, into one file in the first
    /// one's format (Markdown when that can't be written).
    CombineText,
    /// Put the selected tables (CSV, TSV, JSON, Excel) into one Excel
    /// workbook, a sheet for each.
    CombineWorkbook,
    /// Split each selected PDF into several.
    SplitPdf { mode: SplitMode },
    /// Compute a checksum (SHA-256 unless `algorithm` says otherwise) for
    /// every selected file. Writes nothing.
    Checksum {
        #[serde(default, skip_serializing_if = "HashKind::is_default")]
        algorithm: HashKind,
    },
    /// Check that each JSON, YAML, TOML, XML, SVG, CSV or TSV file is
    /// well formed: "Valid JSON", or where the first problem is (line and
    /// column). Writes nothing.
    CheckSyntax,
    /// Each picture, SVG or font as a `data:` URI, for the clipboard, to
    /// paste into CSS or HTML. Writes nothing.
    DataUri,
    /// Encode each file as Base64 text (`photo.png.b64`), or decode a
    /// `.b64` file back into the file it holds.
    Base64,
    /// The text of every selected document, or the text recognised in an
    /// image or PDF, for the clipboard. Writes nothing.
    CopyText,
    /// Count the words in every selected document: "1,234 words · 6 min
    /// read". Writes nothing.
    WordCount,
    /// Put everything selected (files and folders) into one ZIP archive.
    ZipAll,
    /// The contents of every QR code in each picture, for the clipboard.
    /// Writes nothing.
    ReadCodes,
    /// A QR code picture (PNG) of each text file's contents.
    MakeQr,
    /// Icons made from each picture: a website's favicon set, or an Xcode
    /// app icon set.
    IconSet { set: IconKind },
    /// Copy two or more folders into one new folder. Files with the same
    /// name and different contents are both kept (the second renamed);
    /// identical ones are kept once.
    MergeFolders,
    /// A copy of each folder with every file from its subfolders at the
    /// top level.
    Flatten,
    /// A copy of each folder with its files grouped into Images,
    /// Documents, Audio and so on. Subfolders go into Folders, unchanged.
    SortByKind,
    /// A list of everything in each folder: an indented tree (`txt`,
    /// default) or a spreadsheet with sizes and dates (`csv`).
    ListContents {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<Format>,
    },
    /// Report files with identical contents in the selected folders (and
    /// files), compared byte for byte by SHA-256. Changes nothing.
    FindDuplicates,
    /// Run `steps` on every file of category `only` inside each folder
    /// (and its subfolders), into a new folder that mirrors its structure,
    /// named "<folder> (<label>)". Files the steps can't take are left out.
    ConvertInside {
        steps: Vec<Step>,
        only: crate::format::Category,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Save `count` evenly spaced stills from each video.
    ExtractFrames {
        #[serde(default = "frame_count")]
        count: u32,
        /// JPEG (default) or PNG.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<Format>,
    },
    /// Lock each PDF with a password (AES-256). Not available in recipes,
    /// so passwords are never saved in files.
    ProtectPdf { password: Secret },
    /// Remove the password from each PDF.
    UnlockPdf { password: Secret },
    /// Put everything selected into one ZIP locked with a password
    /// (AES-256). Not available in loadouts.
    ZipLocked { password: Secret },
    /// Extract each locked ZIP with its password. Not available in
    /// loadouts.
    UnzipLocked { password: Secret },
    /// Put the selected clips one after another in one file: videos into
    /// an MP4 (each fitted into the first one's shape), sound into an M4A.
    JoinMedia,
    /// Draw subtitles into each selected video's picture, so every player
    /// shows them. Each video takes the subtitle file with its name, or
    /// the only one selected.
    BurnSubtitles,
    /// Make the selected photos of paper pages into one scanned PDF: each
    /// page cut out, squared up, evened out, with searchable text (Mac
    /// helper).
    ScanDocuments,
    /// Run the person's shortcut `name` (from the Shortcuts app) on each
    /// file; what it returns is saved beside it. Not offered to AI agents.
    RunShortcut { name: String },
    /// A description of each picture, as alt text for the clipboard,
    /// written on-device by Apple Intelligence from what Vision sees in it.
    /// Writes nothing.
    Describe,
    /// A summary of each document or PDF, written on-device by Apple
    /// Intelligence, saved beside it as Markdown.
    Summarize,
}

/// A checksum algorithm for [`Action::Checksum`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashKind {
    Md5,
    Sha1,
    #[default]
    Sha256,
    Sha512,
}

impl HashKind {
    pub fn is_default(&self) -> bool {
        *self == HashKind::Sha256
    }

    /// "SHA-256".
    pub fn label(self) -> &'static str {
        match self {
            HashKind::Md5 => "MD5",
            HashKind::Sha1 => "SHA-1",
            HashKind::Sha256 => "SHA-256",
            HashKind::Sha512 => "SHA-512",
        }
    }
}

/// Which icons [`Action::IconSet`] makes. (A macOS icon is a conversion
/// to ICNS.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconKind {
    /// favicon.ico, PNGs, the Apple touch icon, Android icons, a web
    /// manifest and the HTML that links them.
    Favicon,
    /// AppIcon.appiconset for Xcode.
    Ios,
}

/// A password. It travels as a plain string (the app and workers need
/// it), but `Debug` never shows it, so it can't reach a log by accident.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Secret(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Secret(s.to_string())
    }
}

fn frame_count() -> u32 {
    10
}

impl Action {
    pub fn convert(to: Format) -> Action {
        Action::Steps {
            steps: vec![Step::Convert { to, quality: None }],
            label: None,
        }
    }

    /// A SHA-256 checksum.
    pub fn checksum() -> Action {
        Action::Checksum {
            algorithm: HashKind::Sha256,
        }
    }

    pub fn single(step: Step) -> Action {
        Action::Steps {
            steps: vec![step],
            label: None,
        }
    }

    /// True for actions that run once over the whole selection rather
    /// than once per file.
    pub fn is_whole_selection(&self) -> bool {
        matches!(
            self,
            Action::CombinePdf
                | Action::CombineText
                | Action::CombineWorkbook
                | Action::ZipAll
                | Action::ZipLocked { .. }
                | Action::MergeFolders
                | Action::FindDuplicates
                | Action::JoinMedia
                | Action::BurnSubtitles
                | Action::ScanDocuments
        )
    }
}

/// Parses a size such as "2MB", "500 KB", "1.5m" or "800000" into bytes
/// (decimal units, as Finder shows them).
pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase().replace(' ', "");
    let (num, mult) = if let Some(n) = t.strip_suffix("gb").or_else(|| t.strip_suffix('g')) {
        (n, 1e9)
    } else if let Some(n) = t.strip_suffix("mb").or_else(|| t.strip_suffix('m')) {
        (n, 1e6)
    } else if let Some(n) = t.strip_suffix("kb").or_else(|| t.strip_suffix('k')) {
        (n, 1e3)
    } else if let Some(n) = t.strip_suffix('b') {
        (n, 1.0)
    } else {
        (t.as_str(), 1.0)
    };
    let v: f64 = num.parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| (v * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_json_shape_is_stable() {
        let a = Action::Steps {
            steps: vec![
                Step::resize_max(1600),
                Step::Convert {
                    to: Format::Webp,
                    quality: Some(80),
                },
                Step::StripMetadata,
            ],
            label: Some("Web-ready".into()),
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"steps","steps":[{"op":"resize","max":1600},{"op":"convert","to":"webp","quality":80},{"op":"strip_metadata"}],"label":"Web-ready"}"#
        );
        let back: Action = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn new_steps_round_trip() {
        let json = r#"{"kind":"steps","steps":[{"op":"crop","rect":{"x":0.1,"y":0.2,"w":0.5,"h":0.5}},{"op":"redact","rects":[{"x":0,"y":0,"w":0.25,"h":0.1}],"style":"pixelate"},{"op":"compress","level":"strong","target_bytes":2000000},{"op":"pages","pages":[3,1,2]}]}"#;
        let a: Action = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Action>(&back).unwrap(), a);
        let split: Action = serde_json::from_str(
            r#"{"kind":"split_pdf","mode":{"by":"ranges","ranges":"1-3, 5"}}"#,
        )
        .unwrap();
        assert_eq!(
            split,
            Action::SplitPdf {
                mode: SplitMode::Ranges {
                    ranges: "1-3, 5".into()
                }
            }
        );
        let redact: Step = serde_json::from_str(r#"{"op":"redact","rects":[]}"#).unwrap();
        assert_eq!(
            redact,
            Step::Redact {
                rects: vec![],
                style: RedactStyle::Black
            }
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let bad = r#"{"kind":"steps","steps":[{"op":"rotate","degrees":90,"oops":1}]}"#;
        assert!(serde_json::from_str::<Action>(bad).is_err());
        let bad_rect = r#"{"op":"crop","rect":{"x":0,"y":0,"w":1,"h":1,"z":2}}"#;
        assert!(serde_json::from_str::<Step>(bad_rect).is_err());
    }

    #[test]
    fn rects() {
        assert!(Rect::FULL.is_valid());
        assert!(!Rect {
            x: 0.5,
            y: 0.0,
            w: 0.6,
            h: 1.0
        }
        .is_valid());
        assert!(!Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 1.0
        }
        .is_valid());
        assert!(!Rect {
            x: f64::NAN,
            y: 0.0,
            w: 0.5,
            h: 0.5
        }
        .is_valid());
        assert_eq!(
            Rect {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.5
            }
            .to_pixels(400, 200),
            (100, 100, 200, 100)
        );
        assert_eq!(
            Rect {
                x: 0.999,
                y: 0.999,
                w: 0.001,
                h: 0.001
            }
            .to_pixels(10, 10),
            (9, 9, 1, 1)
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2MB"), Some(2_000_000));
        assert_eq!(parse_size("1.5 m"), Some(1_500_000));
        assert_eq!(parse_size("500kb"), Some(500_000));
        assert_eq!(parse_size("800000"), Some(800_000));
        assert_eq!(parse_size("0"), None);
        assert_eq!(parse_size("lots"), None);
    }

    /// The exact JSON the Mac tool windows send (see the Swift
    /// `ToolActionTests.testActionShapes`).
    #[test]
    fn passwords_travel_but_never_print() {
        let a: Action =
            serde_json::from_str(r#"{"kind":"protect_pdf","password":"tiger-42"}"#).unwrap();
        assert!(!format!("{a:?}").contains("tiger"));
        assert!(serde_json::to_string(&a).unwrap().contains("tiger-42"));
        let Action::ProtectPdf { password } = a else {
            panic!("wrong action")
        };
        assert_eq!(password.expose(), "tiger-42");
    }

    #[test]
    fn tool_window_actions_are_accepted() {
        let sent = [
            r#"{"kind":"steps","steps":[{"op":"resize","width":1200}]}"#,
            r#"{"kind":"steps","steps":[{"op":"crop","rect":{"h":1,"w":0.5,"x":0.25,"y":0}}]}"#,
            r#"{"kind":"steps","steps":[{"op":"redact","rects":[{"h":0.25,"w":0.25,"x":0.5,"y":0.5}],"style":"pixelate"}]}"#,
            r#"{"kind":"steps","steps":[{"level":"strong","op":"compress","target_bytes":2000000}]}"#,
            r#"{"kind":"split_pdf","mode":{"by":"chunks","size":3}}"#,
            r#"{"kind":"split_pdf","mode":{"by":"ranges","ranges":"1-3, 5"}}"#,
            r#"{"kind":"split_pdf","mode":{"by":"every_page"}}"#,
            r#"{"kind":"steps","steps":[{"op":"pages","pages":[3,1,2]}]}"#,
            // Watch-folder presets.
            r#"{"kind":"steps","steps":[{"op":"convert","to":"jpeg"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"png"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"webp"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"pdf"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"mp4"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"mp3"}]}"#,
            r#"{"kind":"steps","steps":[{"level":"balanced","op":"compress"}]}"#,
            r#"{"kind":"steps","steps":[{"max":1920,"op":"resize"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"strip_metadata"}]}"#,
            r#"{"kind":"steps","label":"Web","steps":[{"op":"strip_metadata"}]}"#,
            // Vision tools.
            r#"{"kind":"copy_text"}"#,
            r#"{"kind":"steps","steps":[{"op":"remove_background"}]}"#,
            r#"{"kind":"steps","steps":[{"crop":true,"op":"remove_background"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"make_searchable"}]}"#,
            // Newer tool windows (Swift `ExtraActionTests.testShapes`).
            r#"{"kind":"steps","steps":[{"op":"page_numbers","position":"bottom_right","size":10,"skip_first":true,"start":5,"style":"page_of_total"}]}"#,
            r#"{"kind":"steps","steps":[{"diagonal":true,"op":"watermark","opacity":0.2,"text":"DRAFT"}]}"#,
            r#"{"kind":"protect_pdf","password":"pw"}"#,
            r#"{"kind":"unlock_pdf","password":"pw"}"#,
            r#"{"kind":"zip_locked","password":"pw"}"#,
            r#"{"kind":"unzip_locked","password":"pw"}"#,
            r#"{"kind":"steps","steps":[{"emails":true,"op":"redact_text","words":["Jane Doe"]}]}"#,
            r#"{"kind":"join_media"}"#,
            r#"{"kind":"burn_subtitles"}"#,
            r#"{"kind":"scan_documents"}"#,
            r#"{"kind":"steps","steps":[{"end":4,"op":"trim","start":1.5}]}"#,
            r#"{"kind":"steps","steps":[{"end":2,"op":"trim","start":1},{"op":"convert","to":"gif"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"pages","pages":[2,1],"rotate":[{"degrees":90,"page":2}]}]}"#,
            r#"{"kind":"extract_frames"}"#,
            // Subtitles and Smart compression (Swift `SpeechAndSmartTests`).
            r#"{"kind":"steps","steps":[{"op":"transcribe","to":"srt"}]}"#,
            r#"{"kind":"steps","steps":[{"language":"en-GB","op":"transcribe","to":"vtt"}]}"#,
            r#"{"kind":"steps","steps":[{"level":"smart","op":"compress"}]}"#,
            // Documents, folders and data files.
            r#"{"kind":"steps","steps":[{"op":"clean_text"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"format_json"}]}"#,
            r#"{"kind":"steps","steps":[{"minify":true,"op":"format_json"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"docx"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"zip"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"folder"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"resize","percent":400},{"op":"convert","to":"png"}]}"#,
            r#"{"kind":"combine_text"}"#,
            r#"{"kind":"combine_workbook"}"#,
            r#"{"kind":"read_codes"}"#,
            r#"{"kind":"make_qr"}"#,
            r#"{"kind":"icon_set","set":"favicon"}"#,
            r#"{"kind":"icon_set","set":"ios"}"#,
            r#"{"kind":"steps","steps":[{"op":"blur_faces"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"blur_faces","style":"black"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"translate","to":"fr"}]}"#,
            r#"{"kind":"steps","steps":[{"from":"de","op":"translate","to":"pt-BR"}]}"#,
            r#"{"kind":"steps","steps":[{"op":"convert","to":"icns"}]}"#,
            r#"{"kind":"word_count"}"#,
            r#"{"kind":"zip_all"}"#,
            r#"{"kind":"merge_folders"}"#,
            r#"{"kind":"flatten"}"#,
            r#"{"kind":"sort_by_kind"}"#,
            r#"{"kind":"list_contents"}"#,
            r#"{"format":"csv","kind":"list_contents"}"#,
            r#"{"kind":"find_duplicates"}"#,
            r#"{"kind":"convert_inside","only":"image","steps":[{"op":"convert","to":"jpeg"}],"label":"JPEG"}"#,
        ];
        for json in sent {
            let a: Action = serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
            if let Action::Steps { steps, .. } = &a {
                for s in steps {
                    match s {
                        Step::Crop { rect } => assert!(rect.is_valid()),
                        Step::Redact { rects, .. } => assert!(rects.iter().all(Rect::is_valid)),
                        _ => {}
                    }
                }
            }
        }
    }
}
