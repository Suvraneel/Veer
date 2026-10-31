//! Runs one unit of work: a step chain on one file, a PDF combine or
//! split, or a checksum. Image steps share one decode and one encode;
//! file-based steps (media, PDF, text) pass files through a private scratch
//! folder. Final outputs are written straight into the destination folder
//! and then published under free names.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::catalog::{self, Capabilities};
use crate::engines::image::{self as img, EncodeOptions, Picture, ResizeSpec};
use crate::engines::media::{self, MediaJob};
use crate::engines::{
    anim, archive, data, devtools, doc, docpdf, folder, font, iwork, meta, pdf, pdfedit, pdfium,
    sheet, speech, svg, text, trace, vision,
};
use crate::error::{Error, Result};
use crate::format::{self, display_name, Category, Format};
use crate::naming::{self, OutputLocation};
use crate::ops::{CompressLevel, SplitMode, Step, TraceDetail};

/// Everything a unit needs from its surroundings.
pub struct Context<'a> {
    pub caps: &'a Capabilities,
    pub cancel: &'a AtomicBool,
    pub progress: &'a mut dyn FnMut(f32),
    pub output: &'a OutputLocation,
    /// Marks this unit's temporary files so a supervisor can clean them up.
    pub temp_tag: &'a str,
}

impl Context<'_> {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// What a unit produced.
#[derive(Debug, Default)]
pub struct Produced {
    /// The main output (the first of `outputs`).
    pub output: Option<PathBuf>,
    /// Every top-level item created: files next to the original, or one
    /// folder holding many files.
    pub outputs: Vec<PathBuf>,
    /// A computed value such as a checksum.
    pub value: Option<String>,
}

impl Produced {
    pub(crate) fn one(path: PathBuf) -> Produced {
        Produced {
            output: Some(path.clone()),
            outputs: vec![path],
            value: None,
        }
    }

    pub(crate) fn many(paths: Vec<PathBuf>) -> Produced {
        Produced {
            output: paths.first().cloned(),
            outputs: paths,
            value: None,
        }
    }
}

/// DPI for PDF pages turned into images (print quality).
pub const PDF_RENDER_DPI: u32 = 300;

enum State {
    File {
        path: PathBuf,
        fmt: Format,
    },
    Pixels {
        pic: Picture,
        fmt: Format,
        quality: Option<u8>,
        smallest: bool,
        /// A size or quality goal for the encoder.
        goal: Option<Goal>,
    },
}

/// What a compressing encoder aims for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Goal {
    /// At most this many bytes.
    Bytes(u64),
    /// The smallest file that still looks the same (Smart).
    LooksSame,
}

impl State {
    fn format(&self) -> Format {
        match self {
            State::File { fmt, .. } | State::Pixels { fmt, .. } => *fmt,
        }
    }
}

struct Px {
    pic: Picture,
    fmt: Format,
    quality: Option<u8>,
    smallest: bool,
    goal: Option<Goal>,
}

impl Px {
    fn into_state(self) -> State {
        State::Pixels {
            pic: self.pic,
            fmt: self.fmt,
            quality: self.quality,
            smallest: self.smallest,
            goal: self.goal,
        }
    }

    fn with_img(self, img: image::DynamicImage) -> Px {
        Px {
            pic: Picture {
                img,
                icc: self.pic.icc,
            },
            ..self
        }
    }
}

fn pixels(state: State, caps: &Capabilities) -> Result<Px> {
    match state {
        State::Pixels {
            pic,
            fmt,
            quality,
            smallest,
            goal,
        } => Ok(Px {
            pic,
            fmt,
            quality,
            smallest,
            goal,
        }),
        State::File { path, fmt } => Ok(Px {
            pic: img::decode(&path, fmt, caps)?,
            fmt,
            quality: None,
            smallest: false,
            goal: None,
        }),
    }
}

fn validate_chain(steps: &[Step], from: Format, caps: &Capabilities) -> Result<Format> {
    let mut fmt = from;
    for step in steps {
        if !catalog::step_accepts(step, fmt, caps) {
            // Something this Mac can get would do it: say so, so the app
            // can offer it.
            if let Some(need) = catalog::would_accept_with(step, fmt, caps) {
                return Err(match need {
                    crate::error::Need::Ffmpeg => Error::FfmpegMissing,
                    need => Error::Needs {
                        need,
                        message: format!(
                            "{} files need {} on this Mac, free from Apple in the App Store",
                            fmt.label(),
                            match need {
                                crate::error::Need::Keynote => "Keynote",
                                crate::error::Need::Pages => "Pages",
                                _ => "Numbers",
                            }
                        ),
                    },
                });
            }
            return Err(match step {
                Step::Convert { to, .. } => Error::UnsupportedRoute {
                    from: fmt.label().into(),
                    to: to.label().into(),
                },
                // Say what's missing rather than blaming the file.
                Step::Transcribe { .. }
                    if caps.vision.is_none()
                        && matches!(fmt.category(), Category::Audio | Category::Video) =>
                {
                    Error::Missing(
                        "subtitles use on-device speech recognition in the Radlet app on macOS"
                            .into(),
                    )
                }
                other => Error::UnsupportedInput(format!(
                    "{} can't be applied to {fmt} files",
                    step_name(other)
                )),
            });
        }
        fmt = catalog::step_output(step, fmt);
    }
    Ok(fmt)
}

/// Runs `steps` on `input` and publishes the result.
pub fn run_steps(
    input: &Path,
    steps: &[Step],
    label: Option<&str>,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    let caps = ctx.caps;
    let from = format::detect(input)?;
    let name = display_name(input);
    if steps.is_empty() {
        return Err(Error::Invalid("no steps to run".into()));
    }
    if let [Step::Convert { to, .. }] = steps {
        if *to == from {
            return Err(Error::NothingToDo(format!("{name} is already {to}")));
        }
        // Folders and archives aren't files to pass along a chain.
        match (from, *to) {
            (Format::Folder, Format::Zip) => {
                return zip_all(std::slice::from_ref(&input.to_path_buf()), None, ctx)
            }
            (Format::Folder, Format::Tar) => return tar_folder(input, ctx),
            (Format::Zip, Format::Folder) => return unzip(input, ctx),
            (f, Format::Folder) if f.needs_bsdtar() => return unzip(input, ctx),
            _ => {}
        }
    }
    // Validate the whole chain before touching anything.
    let final_fmt = validate_chain(steps, from, caps)?;

    // A workbook of several sheets becomes one table per sheet.
    if let ([Step::Convert { to, .. }], true) = (steps, from.is_workbook()) {
        if matches!(to, Format::Csv | Format::Tsv | Format::Json) {
            return sheets_to_files(input, *to, ctx);
        }
    }

    // A PDF turned into images fans out: one image per page.
    if let (Format::Pdf, Some(Step::Convert { to, quality })) = (from, steps.first()) {
        if caps.can_encode_raster(*to) {
            return pdf_to_images(input, *to, *quality, &steps[1..], final_fmt, label, ctx);
        }
    }

    let dir = ctx.output.dir_for(input);
    let out_tmp = naming::temp_in(&dir, ctx.temp_tag, final_fmt.extension())?;
    let scratch = tempfile::tempdir()?;
    let mut state = State::File {
        path: input.to_path_buf(),
        fmt: from,
    };
    let title = doc::title_of(input);
    let total = steps.len() as f32;

    for (i, step) in steps.iter().enumerate() {
        ctx.check_cancel()?;
        let last = i + 1 == steps.len();
        let dst = if last {
            out_tmp.path().to_path_buf()
        } else {
            scratch.path().join(format!("step{i}"))
        };
        let base = i as f32 / total;
        let mut sub = |p: f32| (ctx.progress)(base + p / total);
        state = apply(
            step,
            state,
            &dst,
            scratch.path(),
            caps,
            ctx.cancel,
            &title,
            &mut sub,
        )?;
        (ctx.progress)((i + 1) as f32 / total);
    }
    ctx.check_cancel()?;

    // Encode pending pixels into the output file.
    match state {
        State::Pixels { .. } => finish_pixels(state, out_tmp.path(), caps)?,
        State::File { path, .. } if path == input => {
            return Err(Error::NothingToDo(format!(
                "{name} didn't need any changes"
            )));
        }
        State::File { path, .. } if path != out_tmp.path() => {
            std::fs::copy(&path, out_tmp.path())?;
        }
        State::File { .. } => {}
    }

    let bytes_in = std::fs::metadata(input)?.len();
    let bytes_out = std::fs::metadata(out_tmp.path())?.len();
    if bytes_out == 0 {
        return Err(Error::Invalid(
            "the conversion produced an empty file".into(),
        ));
    }
    let compressing = steps.iter().any(|s| matches!(s, Step::Compress { .. }));
    if compressing && final_fmt == from && bytes_out >= bytes_in {
        return Err(Error::NothingToDo(format!(
            "{name} is already as small as it gets"
        )));
    }

    let suffix = output_suffix(input, from, final_fmt, steps, label);
    naming::inherit_permissions(input, out_tmp.path());
    let desired = naming::desired_path(input, &dir, final_fmt.extension(), suffix.as_deref());
    let published = naming::publish(out_tmp.into_temp_path(), &desired)?;
    Ok(Produced::one(published))
}

fn output_suffix(
    input: &Path,
    from: Format,
    final_fmt: Format,
    steps: &[Step],
    label: Option<&str>,
) -> Option<String> {
    if final_fmt.extension() == extension_of(input).as_deref().unwrap_or("") || final_fmt == from {
        Some(label.map(slug).unwrap_or_else(|| {
            match steps.iter().rev().find(|s| s.suffix().is_some()) {
                // "notes (fr).md"
                Some(Step::Translate { to, .. }) => to.clone(),
                Some(s) => s.suffix().unwrap_or("converted").to_string(),
                None => "converted".to_string(),
            }
        }))
    } else {
        None
    }
}

fn finish_pixels(state: State, out: &Path, caps: &Capabilities) -> Result<()> {
    if let State::Pixels {
        pic,
        fmt,
        quality,
        smallest,
        goal,
    } = state
    {
        match goal {
            Some(Goal::Bytes(t)) => img::encode_to_target(&pic, fmt, t, out, caps)?,
            Some(Goal::LooksSame) => {
                img::encode_looking_the_same(&pic, fmt, out, caps)?;
            }
            None => img::encode(&pic, fmt, EncodeOptions { quality, smallest }, out, caps)?,
        }
    }
    Ok(())
}

/// Renders each page, runs the remaining (pixel) steps on it, and
/// publishes one image per page: next to the PDF for up to four pages,
/// otherwise in a folder.
fn pdf_to_images(
    input: &Path,
    to: Format,
    quality: Option<u8>,
    rest: &[Step],
    final_fmt: Format,
    label: Option<&str>,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    let caps = ctx.caps;
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let scratch = tempfile::tempdir()?;
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let cancel = ctx.cancel;
    let progress = &mut *ctx.progress;
    let mut each = |number: u32, total: u32, page: image::DynamicImage| -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut state = State::Pixels {
            pic: Picture {
                img: page,
                icc: None,
            },
            fmt: to,
            quality,
            smallest: false,
            goal: None,
        };
        for (i, step) in rest.iter().enumerate() {
            let dst = scratch.path().join(format!("p{number}s{i}"));
            state = apply(
                step,
                state,
                &dst,
                scratch.path(),
                caps,
                cancel,
                &stem,
                &mut |_| {},
            )?;
        }
        let raw = tmp
            .path()
            .join(format!("raw{number}.{}", final_fmt.extension()));
        match state {
            State::Pixels { .. } => finish_pixels(state, &raw, caps)?,
            State::File { path, .. } => {
                std::fs::copy(&path, &raw)?;
            }
        }
        naming::inherit_permissions(input, &raw);
        let digits = total.to_string().len();
        let name = if total == 1 {
            let suffix = label.map(slug);
            match suffix {
                Some(s) => format!("{stem} ({s}).{}", final_fmt.extension()),
                None => format!("{stem}.{}", final_fmt.extension()),
            }
        } else {
            format!("{stem} - page {number:0digits$}.{}", final_fmt.extension())
        };
        files.push((raw, name));
        progress(number as f32 / total as f32);
        Ok(())
    };
    pdfium::render(caps, input, PDF_RENDER_DPI, None, &mut each)?;
    let outputs = naming::publish_group(tmp, files, &dir, &format!("{stem} (pages)"))?;
    Ok(Produced::many(outputs))
}

/// Splits a PDF into parts: next to it for up to four parts, otherwise in
/// a folder.
pub fn split_pdf(input: &Path, mode: &SplitMode, ctx: &mut Context<'_>) -> Result<Produced> {
    let from = format::detect(input)?;
    if from != Format::Pdf {
        return Err(Error::UnsupportedInput(format!(
            "{} isn't a PDF",
            display_name(input)
        )));
    }
    let count = pdf::page_count(input)? as u32;
    let groups = pdf::split_groups(mode, count)?;
    ctx.check_cancel()?;
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let parts = pdf::split_into(input, &groups, tmp.path())?;
    (ctx.progress)(0.9);
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    let files: Vec<(PathBuf, String)> = parts
        .into_iter()
        .zip(&groups)
        .map(|(path, group)| {
            naming::inherit_permissions(input, &path);
            (path, format!("{stem} ({}).pdf", pdf::group_label(group)))
        })
        .collect();
    let outputs = naming::publish_group(tmp, files, &dir, &format!("{stem} (split)"))?;
    (ctx.progress)(1.0);
    Ok(Produced::many(outputs))
}

fn extension_of(p: &Path) -> Option<String> {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn step_name(step: &Step) -> &'static str {
    match step {
        Step::Convert { .. } => "Convert",
        Step::Resize { .. } => "Resize",
        Step::Rotate { .. } => "Rotate",
        Step::Crop { .. } => "Crop",
        Step::Redact { .. } => "Redact",
        Step::StripMetadata => "Remove metadata",
        Step::Compress { .. } => "Compress",
        Step::Pages { .. } => "Pages",
        Step::PageNumbers { .. } => "Page numbers",
        Step::Watermark { .. } => "Watermark",
        Step::Trim { .. } => "Trim",
        Step::NormalizeAudio => "Even out volume",
        Step::RemoveBackground { .. } => "Remove background",
        Step::MakeSearchable => "Make searchable",
        Step::Transcribe { .. } => "Make subtitles",
        Step::CleanText => "Clean up text",
        Step::BlurFaces { .. } => "Hide faces",
        Step::Translate { .. } => "Translate",
        Step::FormatJson { .. } => "Format JSON",
        Step::FormatXml { .. } => "Format XML",
        Step::OptimizeSvg => "Optimize SVG",
        Step::RedactText { .. } => "Black out text",
        Step::Trace { .. } => "Trace to SVG",
    }
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "loadout".into()
    } else {
        trimmed
    }
}

#[allow(clippy::too_many_arguments)]
fn apply(
    step: &Step,
    state: State,
    dst: &Path,
    scratch: &Path,
    caps: &Capabilities,
    cancel: &AtomicBool,
    title: &str,
    progress: &mut dyn FnMut(f32),
) -> Result<State> {
    let cur = state.format();
    let video = cur.category() == Category::Video;
    let run_media = |job: MediaJob, state: State, progress: &mut dyn FnMut(f32)| -> Result<State> {
        let (path, fmt) = materialize(state, scratch, caps)?;
        let to = media::run(caps, job, &path, fmt, dst, cancel, progress)?;
        Ok(State::File {
            path: dst.to_path_buf(),
            fmt: to,
        })
    };
    match step {
        Step::Convert { to, quality } => {
            let to = *to;
            if to == cur && quality.is_none() {
                return Ok(state);
            }
            // Keynote, Pages and Numbers export their own documents (and
            // PowerPoint's slides, through Keynote).
            if let State::File { path, fmt } = &state {
                if iwork::handles(caps, *fmt, to) {
                    iwork::convert(caps, path, *fmt, to, title, scratch, dst, cancel)?;
                    return Ok(State::File {
                        path: dst.to_path_buf(),
                        fmt: to,
                    });
                }
            }
            // A picture converted to SVG is traced into shapes.
            if to == Format::Svg && cur != Format::Svg && caps.can_decode_raster(cur) {
                return trace_state(state, true, TraceDetail::default(), dst, caps, cancel);
            }
            // Moving pictures keep every frame between GIF, WebP and PNG.
            if let State::File { path, fmt } = &state {
                if anim::animatable(*fmt) && anim::animatable(to) && anim::is_animated(path, *fmt) {
                    anim::convert(path, *fmt, to, *quality, dst)?;
                    return Ok(State::File {
                        path: dst.to_path_buf(),
                        fmt: to,
                    });
                }
            }
            if caps.can_decode_raster(cur) && caps.can_encode_raster(to) {
                let px = pixels(state, caps)?;
                return Ok(Px {
                    fmt: to,
                    quality: *quality,
                    goal: None,
                    ..px
                }
                .into_state());
            }
            if to == Format::Pdf {
                if let State::File { path, fmt } = &state {
                    let bytes = match fmt {
                        // Drawings stay vector.
                        Format::Svg => Some(svg::to_pdf(path)?),
                        f if f.is_document_text()
                            || f.is_workbook()
                            || matches!(f, Format::Csv | Format::Tsv) =>
                        {
                            let d = doc::read(path, *f)?;
                            Some(docpdf::render(&d, title, caps, cancel)?)
                        }
                        _ => None,
                    };
                    if let Some(bytes) = bytes {
                        std::fs::write(dst, bytes)?;
                        return Ok(State::File {
                            path: dst.to_path_buf(),
                            fmt: Format::Pdf,
                        });
                    }
                }
                let mut b = pdf::PdfBuilder::new();
                match state {
                    State::Pixels { pic, .. } => b.add_picture(&pic)?,
                    State::File { path, fmt } => b.add_file(&path, fmt, caps)?,
                }
                b.finish(dst)?;
                return Ok(State::File {
                    path: dst.to_path_buf(),
                    fmt: Format::Pdf,
                });
            }
            if to == Format::Txt && cur.category() == Category::Image {
                // Text recognition (the Vision helper reads the file itself).
                let input = vision_input(state, scratch, caps)?;
                std::fs::write(dst, vision::text(caps, &input, scratch, cancel, progress)?)?;
                return Ok(State::File {
                    path: dst.to_path_buf(),
                    fmt: Format::Txt,
                });
            }
            let (path, fmt) = materialize(state, scratch, caps)?;
            match (fmt, to) {
                (Format::Pdf, Format::Txt) => {
                    std::fs::write(dst, pdf_text(&path, scratch, caps, cancel, progress)?)?
                }
                // Word, Markdown and the rest from the PDF's text (recognised
                // first on a Mac, so scans work too), in paragraphs and lists.
                (Format::Pdf, t) if doc::writable(t) => {
                    let text = pdf_text(&path, scratch, caps, cancel, progress)?;
                    doc::write(&doc::from_text(&text), t, title, dst)?
                }
                // Markdown keeps its own renderer, with footnotes and the
                // rest of CommonMark.
                (Format::Md, Format::Html | Format::Txt)
                | (Format::Txt, Format::Srt | Format::Vtt) => {
                    text::convert(&path, fmt, to, dst)?;
                    if fmt == Format::Md && to == Format::Html {
                        retitle_html(dst, title)?;
                    }
                }
                (f, t) if f.is_font() && t.is_font() => font::convert(&path, fmt, to, dst)?,
                (f, _) if f.is_subtitles() => text::convert(&path, fmt, to, dst)?,
                (Format::Txt, t) if t.is_subtitles() => text::convert(&path, fmt, to, dst)?,
                (f, _)
                    if f.is_workbook()
                        || matches!(
                            f,
                            Format::Csv
                                | Format::Tsv
                                | Format::Json
                                | Format::Yaml
                                | Format::Toml
                                | Format::Xml
                        ) =>
                {
                    data::convert(&path, fmt, to, dst)?
                }
                (f, t) if f.is_document_text() && doc::writable(t) => {
                    let d = doc::read(&path, f)?;
                    doc::write(&d, t, title, dst)?
                }
                _ => {
                    media::run(
                        caps,
                        MediaJob::Convert(to),
                        &path,
                        fmt,
                        dst,
                        cancel,
                        progress,
                    )?;
                }
            }
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: to,
            })
        }
        Step::Resize {
            max,
            percent,
            width,
            height,
        } => {
            let spec = ResizeSpec {
                max: *max,
                percent: *percent,
                width: *width,
                height: *height,
            };
            // A drawing is drawn afresh at the new size.
            if let State::File {
                path,
                fmt: Format::Svg,
            } = &state
            {
                return Ok(State::Pixels {
                    pic: svg::decode_sized(path, spec)?,
                    fmt: Format::Png,
                    quality: None,
                    smallest: false,
                    goal: None,
                });
            }
            reject_animation(&state)?;
            let px = pixels(state, caps)?;
            let resized = img::resize(px.pic.img.clone(), spec)?;
            Ok(px.with_img(resized).into_state())
        }
        Step::Rotate { degrees } if cur == Format::Pdf => {
            let (path, _) = materialize(state, scratch, caps)?;
            pdfedit::rotate_all(&path, *degrees, dst)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        Step::Rotate { degrees } => {
            reject_animation(&state)?;
            let px = pixels(state, caps)?;
            let rotated = img::rotate(px.pic.img.clone(), *degrees)?;
            Ok(px.with_img(rotated).into_state())
        }
        Step::Crop { rect } => {
            if video {
                if !rect.is_valid() {
                    return Err(Error::Invalid(
                        "the crop area must lie inside the video".into(),
                    ));
                }
                let to = catalog::step_output(step, cur);
                return run_media(
                    MediaJob::Filter {
                        vf: media::crop_filter(rect),
                        to,
                    },
                    state,
                    progress,
                );
            }
            reject_animation(&state)?;
            let px = pixels(state, caps)?;
            let cropped = img::crop(px.pic.img.clone(), *rect)?;
            Ok(px.with_img(cropped).into_state())
        }
        Step::Redact { rects, style } => {
            if video {
                if rects.is_empty() || rects.iter().any(|r| !r.is_valid()) {
                    return Err(Error::Invalid(
                        "draw at least one area inside the video".into(),
                    ));
                }
                let to = catalog::step_output(step, cur);
                return run_media(
                    MediaJob::Filter {
                        vf: media::redact_filter(rects),
                        to,
                    },
                    state,
                    progress,
                );
            }
            reject_animation(&state)?;
            let px = pixels(state, caps)?;
            let redacted = img::redact(px.pic.img.clone(), rects, *style)?;
            Ok(px.with_img(redacted).into_state())
        }
        Step::StripMetadata => strip(state, dst, caps, cancel, progress),
        Step::Trace { color, detail } => trace_state(state, *color, *detail, dst, caps, cancel),
        Step::RedactText {
            words,
            emails,
            phones,
            cards,
        } => {
            let words: Vec<String> = words
                .iter()
                .map(|w| w.trim().to_string())
                .filter(|w| !w.is_empty())
                .collect();
            let kinds: Vec<&'static str> =
                [(*emails, "emails"), (*phones, "phones"), (*cards, "cards")]
                    .into_iter()
                    .filter_map(|(on, kind)| on.then_some(kind))
                    .collect();
            if words.is_empty() && kinds.is_empty() {
                return Err(Error::Invalid(
                    "say which words, or which kinds of details, to black out".into(),
                ));
            }
            let (path, _) = materialize(state, scratch, caps)?;
            let terms = vision::RedactTerms { words, kinds };
            vision::redact_pdf(caps, &path, &terms, scratch, dst, cancel, progress)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        Step::Compress {
            level,
            target_bytes,
        } if cur == Format::Pdf => {
            let (path, _) = materialize(state, scratch, caps)?;
            pdfedit::shrink(&path, *level, *target_bytes, dst)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        Step::Compress {
            level,
            target_bytes,
        } => {
            if video {
                let job = match target_bytes {
                    Some(t) => MediaJob::CompressVideoTarget(*t),
                    None => MediaJob::CompressVideo(level.video_crf()),
                };
                return run_media(job, state, progress);
            }
            let target = catalog::step_output(step, cur);
            let px = pixels(state, caps)?;
            let lossy = matches!(target, Format::Jpeg | Format::Webp | Format::Heic);
            let goal = match (target_bytes, level) {
                (Some(t), _) => Some(Goal::Bytes(*t)),
                // Measured for JPEG and WebP; HEIC goes through sips,
                // which can't be read back here.
                (None, CompressLevel::Smart) if matches!(target, Format::Jpeg | Format::Webp) => {
                    Some(Goal::LooksSame)
                }
                (None, _) => None,
            };
            Ok(Px {
                fmt: target,
                quality: lossy.then(|| level.image_quality()),
                smallest: true,
                goal,
                ..px
            }
            .into_state())
        }
        Step::Pages { pages, rotate } => {
            let (path, _) = materialize(state, scratch, caps)?;
            pdf::select_pages(&path, pages, rotate, dst)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        Step::PageNumbers {
            position,
            style,
            start,
            size,
            skip_first,
        } => {
            let (path, _) = materialize(state, scratch, caps)?;
            let opts = pdfedit::NumberOptions {
                position: *position,
                style: *style,
                start: *start,
                size: size.unwrap_or(10.0),
                skip_first: *skip_first,
            };
            pdfedit::page_numbers(&path, opts, dst)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        Step::Watermark {
            text,
            opacity,
            size,
            diagonal,
        } => {
            let (path, _) = materialize(state, scratch, caps)?;
            let opts = pdfedit::WatermarkOptions {
                text: text.clone(),
                opacity: *opacity,
                size: *size,
                diagonal: *diagonal,
            };
            pdfedit::watermark(&path, &opts, dst)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        // Keeping everything is no change at all.
        Step::Trim { start, end: None } if *start == 0.0 => Ok(state),
        Step::Trim { start, end } => {
            if !start.is_finite()
                || *start < 0.0
                || end.is_some_and(|e| !e.is_finite() || e <= *start)
            {
                return Err(Error::Invalid(
                    "the end of a trim must come after its start".into(),
                ));
            }
            run_media(
                MediaJob::Trim {
                    start: *start,
                    end: *end,
                },
                state,
                progress,
            )
        }
        Step::NormalizeAudio => run_media(MediaJob::Normalize, state, progress),
        Step::RemoveBackground { crop } => {
            reject_animation(&state)?;
            let to = catalog::step_output(step, cur);
            let px = pixels(state, caps)?;
            // Vision only supplies the mask; it's applied here, at full
            // resolution, so colour profiles stay as they are.
            let upright = scratch.join("cutout-input.png");
            img::save_png_fast(&px.pic.img, &upright)?;
            let mask = scratch.join("cutout-mask.png");
            vision::mask(caps, &upright, &mask, cancel)?;
            let mask = image::open(&mask)?.to_luma8();
            let cut = img::apply_mask(&px.pic.img, &mask, *crop)?;
            Ok(Px {
                fmt: to,
                quality: None,
                goal: None,
                ..px
            }
            .with_img(cut)
            .into_state())
        }
        Step::MakeSearchable => {
            let input = vision_input(state, scratch, caps)?;
            vision::searchable(caps, &input, dst, cancel, progress)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: Format::Pdf,
            })
        }
        Step::Transcribe { to, language } => {
            if let Some(l) = language {
                if !speech::valid_language(l) {
                    return Err(Error::Invalid(format!(
                        "\"{l}\" isn't a language code such as en-US"
                    )));
                }
            }
            let (path, _) = materialize(state, scratch, caps)?;
            let text = speech::transcribe(
                caps,
                &path,
                *to,
                language.as_deref(),
                scratch,
                cancel,
                progress,
            )?;
            std::fs::write(dst, text)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt: *to,
            })
        }
        Step::BlurFaces { style } => {
            reject_animation(&state)?;
            let px = pixels(state, caps)?;
            // Vision only finds them; they're covered here, at full size.
            let upright = scratch.join("faces-input.png");
            img::save_png_fast(&px.pic.img, &upright)?;
            let found = vision::faces(caps, &upright, scratch, cancel)?;
            // A little bigger than Vision's box, which hugs the features.
            let rects: Vec<crate::ops::Rect> = found
                .iter()
                .filter(|r| r.w > 0.0 && r.h > 0.0)
                .map(|r| {
                    let (gx, gy) = (r.w * 0.2, r.h * 0.25);
                    let x = (r.x - gx).clamp(0.0, 1.0);
                    let y = (r.y - gy).clamp(0.0, 1.0);
                    crate::ops::Rect {
                        x,
                        y,
                        w: (r.w + 2.0 * gx).min(1.0 - x),
                        h: (r.h + 2.0 * gy).min(1.0 - y),
                    }
                })
                .filter(|r| r.is_valid())
                .collect();
            if rects.is_empty() {
                return Err(Error::NothingToDo("no faces found".into()));
            }
            let hidden = img::redact(px.pic.img.clone(), &rects, *style)?;
            Ok(px.with_img(hidden).into_state())
        }
        Step::Translate { to, from } => {
            let (path, fmt) = materialize(state, scratch, caps)?;
            crate::engines::translate::translate_file(
                &path,
                fmt,
                to,
                from.as_deref(),
                title,
                caps,
                scratch,
                cancel,
                progress,
                dst,
            )?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
        Step::CleanText => {
            let (path, fmt) = materialize(state, scratch, caps)?;
            let raw = std::fs::read(&path)?;
            let cleaned = text::clean(&raw, fmt == Format::Md)?;
            if cleaned.as_bytes() == raw.as_slice() {
                return Err(Error::NothingToDo("it's already clean".into()));
            }
            std::fs::write(dst, cleaned)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
        Step::FormatJson { minify, sort_keys } => {
            let (path, fmt) = materialize(state, scratch, caps)?;
            let src = text::read_text(&path)?;
            let out = if *sort_keys {
                data::format_json(&devtools::sort_json_keys(&src)?, *minify)?
            } else {
                data::format_json(&src, *minify)?
            };
            if out == src {
                return Err(Error::NothingToDo(if *minify {
                    "it's already minified".into()
                } else {
                    "it's already formatted".into()
                }));
            }
            std::fs::write(dst, out)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
        Step::FormatXml { minify } => {
            let (path, fmt) = materialize(state, scratch, caps)?;
            let src = text::read_text(&path)?;
            let out = devtools::format_xml(&src, *minify)?;
            if out == src {
                return Err(Error::NothingToDo(if *minify {
                    "it's already minified".into()
                } else {
                    "it's already formatted".into()
                }));
            }
            std::fs::write(dst, out)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
        Step::OptimizeSvg => {
            let (path, fmt) = materialize(state, scratch, caps)?;
            let src = text::read_text(&path)?;
            let out = devtools::optimize_svg(&src)?;
            if out.len() >= src.len() {
                return Err(Error::NothingToDo(
                    "there's nothing to take out; it's already optimized".into(),
                ));
            }
            std::fs::write(dst, out)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
    }
}

/// Markdown pages are titled after the file, not the scratch name a
/// chain step had.
/// Traces the picture (decoded, or as earlier steps left it) into `dst`.
fn trace_state(
    state: State,
    color: bool,
    detail: TraceDetail,
    dst: &Path,
    caps: &Capabilities,
    cancel: &AtomicBool,
) -> Result<State> {
    let px = pixels(state, caps)?;
    let svg = trace::trace(&px.pic.img, color, detail, cancel)?;
    std::fs::write(dst, svg)?;
    Ok(State::File {
        path: dst.to_path_buf(),
        fmt: Format::Svg,
    })
}

fn retitle_html(path: &Path, title: &str) -> Result<()> {
    let html = std::fs::read_to_string(path)?;
    if let (Some(a), Some(b)) = (html.find("<title>"), html.find("</title>")) {
        if a < b {
            let fixed = format!(
                "{}<title>{}{}",
                &html[..a],
                doc::escape_html(title),
                &html[b..]
            );
            std::fs::write(path, fixed)?;
        }
    }
    Ok(())
}

/// A file the Vision helper (ImageIO and PDFKit) can read: the original
/// when it can, otherwise the pixels as PNG.
fn vision_input(state: State, scratch: &Path, caps: &Capabilities) -> Result<PathBuf> {
    match state {
        State::File { path, fmt } if !matches!(fmt, Format::Qoi | Format::Svg) => Ok(path),
        state => {
            let px = pixels(state, caps)?;
            let p = scratch.join("vision-input.png");
            img::save_png_fast(&px.pic.img, &p)?;
            Ok(p)
        }
    }
}

/// A PDF's text: with the Vision helper, PDFKit's text plus recognition
/// for scanned pages; otherwise the text layer alone.
fn pdf_text(
    path: &Path,
    scratch: &Path,
    caps: &Capabilities,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<String> {
    if caps.vision.is_none() {
        return pdf::to_text(path);
    }
    match vision::text(caps, path, scratch, cancel, progress) {
        Err(Error::Helper(msg)) => pdf::to_text(path).map_err(|_| Error::Helper(msg)),
        other => other,
    }
}

/// Lossless metadata removal where the container allows it; otherwise the
/// image is decoded and re-encoded, which drops metadata by construction.
fn strip(
    state: State,
    dst: &Path,
    caps: &Capabilities,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<State> {
    let (path, fmt) = match state {
        State::Pixels { .. } => return Ok(state),
        State::File { path, fmt } => (path, fmt),
    };
    let lossless: Option<Vec<u8>> = match fmt {
        Format::Jpeg => Some(meta::strip_jpeg(
            &std::fs::read(&path)?,
            img::jpeg_orientation(&path),
        )?),
        Format::Gif => Some(meta::strip_gif(&std::fs::read(&path)?)?),
        Format::Png | Format::Webp if !has_orientation(&path, fmt) => {
            let data = std::fs::read(&path)?;
            Some(if fmt == Format::Png {
                meta::strip_png(&data)?
            } else {
                meta::strip_webp(&data)?
            })
        }
        _ => None,
    };
    if let Some(bytes) = lossless {
        std::fs::write(dst, bytes)?;
        return Ok(State::File {
            path: dst.to_path_buf(),
            fmt,
        });
    }
    match fmt.category() {
        Category::Document => {
            pdf::strip_metadata(&path, dst)?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
        Category::Audio | Category::Video => {
            media::run(
                caps,
                MediaJob::StripMetadata,
                &path,
                fmt,
                dst,
                cancel,
                progress,
            )?;
            Ok(State::File {
                path: dst.to_path_buf(),
                fmt,
            })
        }
        _ => {
            let pic = img::decode(&path, fmt, caps)?;
            let quality = matches!(fmt, Format::Webp | Format::Heic).then_some(92);
            Ok(State::Pixels {
                pic,
                fmt,
                quality,
                smallest: false,
                goal: None,
            })
        }
    }
}

/// Pixel steps work on one frame; refuse rather than silently drop the
/// rest of an animation.
fn reject_animation(state: &State) -> Result<()> {
    if let State::File { path, fmt } = state {
        if anim::animatable(*fmt) && anim::is_animated(path, *fmt) {
            let hint = if *fmt == Format::Gif {
                ". Convert it to MP4 first"
            } else {
                ""
            };
            return Err(Error::UnsupportedInput(format!(
                "moving pictures can't be edited frame by frame yet{hint}"
            )));
        }
    }
    Ok(())
}

fn has_orientation(path: &Path, fmt: Format) -> bool {
    use image::ImageDecoder;
    let check = || -> Option<bool> {
        let mut r = image::ImageReader::open(path).ok()?;
        r.set_format(img::image_format(fmt)?);
        let mut d = r.into_decoder().ok()?;
        Some(d.orientation().ok()?.to_exif() != 1)
    };
    check().unwrap_or(false)
}

/// Turns pending pixels into a scratch file so a file-based engine can
/// read it.
fn materialize(state: State, scratch: &Path, caps: &Capabilities) -> Result<(PathBuf, Format)> {
    match state {
        State::File { path, fmt } => Ok((path, fmt)),
        State::Pixels { fmt, .. } => {
            let p = tempfile::Builder::new()
                .prefix("pixels-")
                .suffix(&format!(".{}", fmt.extension()))
                .tempfile_in(scratch)?
                .into_temp_path()
                .keep()
                .map_err(|e| Error::Io(e.error))?;
            finish_pixels(state, &p, caps)?;
            Ok((p, fmt))
        }
    }
}

/// Puts every input, in order, into one PDF beside the first input.
pub fn combine_pdf(inputs: &[PathBuf], ctx: &mut Context<'_>) -> Result<Produced> {
    let first = inputs
        .first()
        .ok_or_else(|| Error::Invalid("no files to combine".into()))?;
    let mut builder = pdf::PdfBuilder::new();
    for (i, p) in inputs.iter().enumerate() {
        ctx.check_cancel()?;
        let fmt = format::detect(p)?;
        builder.add_file(p, fmt, ctx.caps)?;
        (ctx.progress)((i + 1) as f32 / (inputs.len() + 1) as f32);
    }
    let dir = ctx.output.dir_for(first);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "pdf")?;
    builder.finish(tmp.path())?;
    naming::inherit_permissions(first, tmp.path());
    let desired = naming::desired_path(first, &dir, "pdf", Some("combined"));
    let out = naming::publish(tmp.into_temp_path(), &desired)?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Saves `count` evenly spaced stills from a video: next to it for up to
/// four, otherwise in a folder.
pub fn extract_frames(
    input: &Path,
    count: u32,
    format: Option<Format>,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    let from = format::detect(input)?;
    if from.category() != Category::Video {
        return Err(Error::UnsupportedInput(format!(
            "stills can only be taken from videos, not {from} files"
        )));
    }
    if !(1..=500).contains(&count) {
        return Err(Error::Invalid("take 1 to 500 stills".into()));
    }
    let to = format.unwrap_or(Format::Jpeg);
    if !matches!(to, Format::Jpeg | Format::Png) {
        return Err(Error::Invalid("stills are saved as JPEG or PNG".into()));
    }
    let probe = media::probe(ctx.caps, input);
    // The video's own length: the file can run on with audio only.
    let duration = probe
        .video_duration_s
        .or(probe.duration_s)
        .filter(|d| *d > 0.0)
        .ok_or_else(|| Error::Missing("taking stills needs FFprobe to read the length".into()))?;
    // Seeking past the last frame gives nothing, so stay just before it.
    let last = (duration - 0.1).max(0.0);
    let made = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.len() > 0);
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    let digits = count.to_string().len();
    let mut files = Vec::new();
    for i in 0..count {
        ctx.check_cancel()?;
        // The middle of each equal slice, so the first isn't a fade-in.
        let at = (duration * (i as f64 + 0.5) / count as f64).min(last);
        let raw = tmp.path().join(format!("frame{i}.{}", to.extension()));
        // A container can claim more video than there is; try a second
        // earlier before giving up. Nothing is saved unless every still is.
        for (attempt, when) in [at, (at - 1.0).max(0.0)].into_iter().enumerate() {
            let _ = std::fs::remove_file(&raw);
            media::run(
                ctx.caps,
                MediaJob::Frame { at: when, to },
                input,
                from,
                &raw,
                ctx.cancel,
                &mut |_| {},
            )?;
            if made(&raw) {
                break;
            }
            if attempt == 1 {
                return Err(Error::Ffmpeg(format!(
                    "couldn't take a still at {at:.1} s; the video may end early"
                )));
            }
        }
        naming::inherit_permissions(input, &raw);
        let n = i + 1;
        files.push((
            raw,
            format!("{stem} - still {n:0digits$}.{}", to.extension()),
        ));
        (ctx.progress)((i + 1) as f32 / count as f32);
    }
    let outputs = naming::publish_group(tmp, files, &dir, &format!("{stem} (stills)"))?;
    Ok(Produced::many(outputs))
}

/// Locks (`protect`) or unlocks a PDF.
pub fn pdf_password(
    input: &Path,
    password: &str,
    protect: bool,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    if format::detect(input)? != Format::Pdf {
        return Err(Error::UnsupportedInput(format!(
            "{} isn't a PDF",
            display_name(input)
        )));
    }
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "pdf")?;
    if protect {
        pdfedit::protect(input, password, tmp.path())?;
    } else {
        pdfedit::unlock(input, password, tmp.path())?;
    }
    naming::inherit_permissions(input, tmp.path());
    let suffix = if protect { "protected" } else { "unlocked" };
    let desired = naming::desired_path(input, &dir, "pdf", Some(suffix));
    let out = naming::publish(tmp.into_temp_path(), &desired)?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

fn value(v: String) -> Produced {
    Produced {
        output: None,
        outputs: vec![],
        value: Some(v),
    }
}

/// The text of a document, or the text recognised in an image or PDF,
/// for the clipboard. Markdown, plain text and data files are copied as
/// they are; Word, RTF and HTML as plain text.
pub fn copy_text(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let fmt = format::detect(input)?;
    let raw = fmt.is_subtitles()
        || matches!(
            fmt,
            Format::Txt
                | Format::Md
                | Format::Csv
                | Format::Tsv
                | Format::Json
                | Format::Yaml
                | Format::Toml
                | Format::Xml
        );
    if raw || fmt.is_document_text() {
        let text = if raw {
            text::read_text(input)?
        } else {
            doc::to_text(&doc::read(input, fmt)?, false)
        };
        let text = text.trim_end().to_string();
        if text.trim().is_empty() {
            return Err(Error::NothingToDo("there's no text in it".into()));
        }
        (ctx.progress)(1.0);
        return Ok(value(text));
    }
    if !(fmt == Format::Pdf || (ctx.caps.can_decode_raster(fmt) && fmt != Format::Svg)) {
        return Err(Error::UnsupportedInput(format!(
            "text can't be copied from {fmt} files"
        )));
    }
    let scratch = tempfile::tempdir()?;
    let state = State::File {
        path: input.to_path_buf(),
        fmt,
    };
    let text = if fmt == Format::Pdf {
        pdf_text(
            input,
            scratch.path(),
            ctx.caps,
            ctx.cancel,
            &mut *ctx.progress,
        )?
    } else {
        let source = vision_input(state, scratch.path(), ctx.caps)?;
        vision::text(
            ctx.caps,
            &source,
            scratch.path(),
            ctx.cancel,
            &mut *ctx.progress,
        )?
    };
    (ctx.progress)(1.0);
    Ok(value(text.trim_end().to_string()))
}

pub fn checksum(
    input: &Path,
    algorithm: crate::ops::HashKind,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    if input.is_dir() {
        return Err(Error::UnsupportedInput(
            "a folder has no checksum; zip it first".into(),
        ));
    }
    let value = devtools::digest(input, algorithm, ctx.cancel)?;
    (ctx.progress)(1.0);
    Ok(Produced {
        output: None,
        outputs: vec![],
        value: Some(value),
    })
}

/// "Valid JSON", or where the first problem is. Writes nothing.
pub fn check_syntax(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let fmt = format::detect(input)?;
    if !devtools::checks_syntax(fmt) {
        return Err(Error::UnsupportedInput(format!(
            "there's no syntax check for {fmt} files"
        )));
    }
    let value = devtools::check_syntax(&text::read_text(input)?, fmt)?;
    (ctx.progress)(1.0);
    Ok(Produced {
        output: None,
        outputs: vec![],
        value: Some(value),
    })
}

/// The file as a `data:` URI. Writes nothing.
pub fn data_uri(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let value = devtools::data_uri(input, format::detect(input)?)?;
    (ctx.progress)(1.0);
    Ok(Produced {
        output: None,
        outputs: vec![],
        value: Some(value),
    })
}

/// Encodes a file as Base64 text beside it ("photo.png.b64"), or decodes
/// a `.b64` file into the file it holds ("photo.png").
pub fn base64(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    if input.is_dir() {
        return Err(Error::UnsupportedInput(
            "a folder can't be Base64; zip it first".into(),
        ));
    }
    if std::fs::metadata(input)?.len() > devtools::BASE64_MAX {
        return Err(Error::UnsupportedInput(
            "it's over 256 MB, too big for Base64".into(),
        ));
    }
    let dir = ctx.output.dir_for(input);
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let (bytes, desired) = if devtools::is_base64_file(input) {
        let bytes = devtools::base64_decode(&text::read_text(input)?)?;
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "decoded".into());
        // "data.b64" says nothing of what it holds: look at the bytes.
        let named = if Path::new(&stem).extension().is_some() {
            stem
        } else {
            let ext = format::sniff(&bytes, None)
                .map(|f| f.extension())
                .unwrap_or("bin");
            format!("{stem}.{ext}")
        };
        (bytes, dir.join(named))
    } else {
        let bytes = std::fs::read(input)?;
        (
            devtools::base64_encode(&bytes).into_bytes(),
            dir.join(format!("{name}.b64")),
        )
    };
    let ext = desired
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bin".into());
    let tmp = naming::temp_in(&dir, ctx.temp_tag, &ext)?;
    std::fs::write(tmp.path(), &bytes)?;
    naming::inherit_permissions(input, tmp.path());
    let out = naming::publish(tmp.into_temp_path(), &desired)?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Words and reading time of a document: "1,234 words · 6 min read".
pub fn word_count(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let fmt = format::detect(input)?;
    let text = match fmt {
        Format::Pdf => {
            let scratch = tempfile::tempdir()?;
            pdf_text(
                input,
                scratch.path(),
                ctx.caps,
                ctx.cancel,
                &mut *ctx.progress,
            )?
        }
        f if f.is_document_text() => doc::read(input, f)?.words_text(),
        other => {
            return Err(Error::UnsupportedInput(format!(
                "words can only be counted in documents, not {other} files"
            )))
        }
    };
    (ctx.progress)(1.0);
    Ok(value(doc::word_count_label(&text)))
}

/// Joins documents, in order, into one in the first one's format. Each
/// part that doesn't start with a heading gets its file's name as one.
pub fn combine_text(inputs: &[PathBuf], ctx: &mut Context<'_>) -> Result<Produced> {
    let first = inputs
        .first()
        .ok_or_else(|| Error::Invalid("no files to combine".into()))?;
    if inputs.len() < 2 {
        return Err(Error::NothingToDo(
            "choose two or more documents to combine".into(),
        ));
    }
    let first_fmt = format::detect(first)?;
    let to = if doc::writable(first_fmt) {
        first_fmt
    } else {
        Format::Md
    };
    let mut combined = doc::Doc::default();
    for (i, p) in inputs.iter().enumerate() {
        ctx.check_cancel()?;
        let fmt = format::detect(p)?;
        let part = doc::read(p, fmt)?;
        if !matches!(part.blocks.first(), Some(doc::Block::Heading { .. })) {
            combined.blocks.push(doc::Block::Heading {
                level: 1,
                text: doc::text(&doc::title_of(p)),
            });
        }
        combined.blocks.extend(part.blocks);
        (ctx.progress)((i + 1) as f32 / (inputs.len() + 1) as f32);
    }
    let dir = ctx.output.dir_for(first);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, to.extension())?;
    doc::write(&combined, to, &doc::title_of(first), tmp.path())?;
    naming::inherit_permissions(first, tmp.path());
    let desired = naming::desired_path(first, &dir, to.extension(), Some("combined"));
    let out = naming::publish(tmp.into_temp_path(), &desired)?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Each sheet of a workbook as its own CSV, TSV or JSON file: beside the
/// workbook for up to four sheets, otherwise in a folder.
fn sheets_to_files(input: &Path, to: Format, ctx: &mut Context<'_>) -> Result<Produced> {
    let sheets = sheet::read_book(input, format::detect(input)?)?;
    if sheets.is_empty() {
        return Err(Error::NothingToDo("the workbook is empty".into()));
    }
    let render = |rows: &[Vec<String>]| -> Result<String> {
        match to {
            Format::Json => Ok(data::rows_to_json(rows)),
            Format::Tsv => data::write_rows(rows, b'\t'),
            _ => data::write_rows(rows, b','),
        }
    };
    let dir = ctx.output.dir_for(input);
    let stem = doc::title_of(input);
    if let [(_, rows)] = sheets.as_slice() {
        let tmp = naming::temp_in(&dir, ctx.temp_tag, to.extension())?;
        std::fs::write(tmp.path(), render(rows)?)?;
        naming::inherit_permissions(input, tmp.path());
        let desired = naming::desired_path(input, &dir, to.extension(), None);
        let out = naming::publish(tmp.into_temp_path(), &desired)?;
        (ctx.progress)(1.0);
        return Ok(Produced::one(out));
    }
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let mut files = Vec::new();
    for (i, (name, rows)) in sheets.iter().enumerate() {
        ctx.check_cancel()?;
        let raw = tmp.path().join(format!("sheet{i}.{}", to.extension()));
        std::fs::write(&raw, render(rows)?)?;
        naming::inherit_permissions(input, &raw);
        let safe = file_part(name, 80, &format!("Sheet {}", i + 1));
        files.push((raw, format!("{stem} - {safe}.{}", to.extension())));
    }
    let outputs = naming::publish_group(tmp, files, &dir, &format!("{stem} (sheets)"))?;
    (ctx.progress)(1.0);
    Ok(Produced::many(outputs))
}

/// Text made safe to go into a file name: no path separators or control
/// characters, no leading dot, at most `max` characters; `fallback` if
/// nothing is left.
fn file_part(text: &str, max: usize, fallback: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .take(max)
        .collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() {
        fallback.into()
    } else {
        clean
    }
}

/// Puts tables, in order, into one workbook: a sheet per CSV, TSV or JSON
/// file, and every sheet of a workbook.
pub fn combine_workbook(inputs: &[PathBuf], ctx: &mut Context<'_>) -> Result<Produced> {
    let first = inputs
        .first()
        .ok_or_else(|| Error::Invalid("no files to combine".into()))?;
    let mut sheets = Vec::new();
    for (i, p) in inputs.iter().enumerate() {
        ctx.check_cancel()?;
        let fmt = format::detect(p)?;
        sheets.extend(sheet::as_sheets(p, fmt)?);
        (ctx.progress)((i + 1) as f32 / (inputs.len() + 1) as f32);
    }
    let dir = ctx.output.dir_for(first);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "xlsx")?;
    std::fs::write(tmp.path(), sheet::write(&sheets)?)?;
    naming::inherit_permissions(first, tmp.path());
    let desired = naming::desired_path(first, &dir, "xlsx", Some("combined"));
    let out = naming::publish(tmp.into_temp_path(), &desired)?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Runs `steps` on every file of category `only` inside a folder and its
/// subfolders, into a new folder "<folder> (<label>)" with the same
/// structure. Files the steps can't take (or that are already in the
/// target format) are left out; a file that fails is listed in "Not
/// converted.txt" there rather than stopping the rest.
pub fn convert_inside(
    input: &Path,
    steps: &[Step],
    only: Category,
    label: Option<&str>,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    use std::collections::VecDeque;
    use std::sync::{mpsc, Mutex};

    require_folder(input)?;
    if steps.is_empty() {
        return Err(Error::Invalid("no steps to run".into()));
    }
    let caps = ctx.caps;
    let cancel = ctx.cancel;
    // Each file, and the folder inside the copy it goes into.
    let mut jobs: Vec<(PathBuf, PathBuf, String)> = Vec::new();
    // Files named like the kind being converted whose contents aren't:
    // reported rather than skipped silently.
    let mut failures: Vec<(String, String)> = Vec::new();
    for e in folder::walk(input, cancel)? {
        // Chosen by name first (cheap), then checked by contents.
        if e.dir || !Format::from_path(&e.path).is_some_and(|f| catalog::inside_kind(only, f, caps))
        {
            continue;
        }
        let fmt = match format::detect(&e.path) {
            Ok(f) if catalog::inside_kind(only, f, caps) => f,
            Ok(f) => {
                failures.push((
                    folder::slash_path(&e.rel),
                    format!("it's really a {f} file"),
                ));
                continue;
            }
            Err(err) => {
                failures.push((folder::slash_path(&e.rel), err.info().message));
                continue;
            }
        };
        if !catalog::chain_accepts(steps, fmt, caps) {
            continue;
        }
        if let [Step::Convert { to, .. }] = steps {
            if *to == fmt {
                continue;
            }
        }
        let parent = e.rel.parent().map(Path::to_path_buf).unwrap_or_default();
        jobs.push((e.path, parent, folder::slash_path(&e.rel)));
    }
    let label = label
        .map(str::to_string)
        .unwrap_or_else(|| match steps.last() {
            Some(Step::Convert { to, .. }) => to.label().to_string(),
            _ => "converted".into(),
        });
    if jobs.is_empty() {
        return Err(match failures.first() {
            None => Error::NothingToDo(format!("there's nothing in it to make into {label}")),
            Some((name, why)) => Error::Invalid(format!(
                "none of its files could be converted ({name}: {why})"
            )),
        });
    }
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let threads = if matches!(only, Category::Audio | Category::Video) {
        1
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 4)
    };
    let queue = Mutex::new(jobs.iter().enumerate().collect::<VecDeque<_>>());
    let total = jobs.len();
    let tag = ctx.temp_tag.to_string();
    let mut converted = 0usize;
    let progress = &mut *ctx.progress;
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, Result<Produced>)>();
        for _ in 0..threads {
            let tx = tx.clone();
            let (queue, tag, root) = (&queue, &tag, tmp.path());
            scope.spawn(move || loop {
                let next = queue.lock().map(|mut q| q.pop_front()).unwrap_or(None);
                let Some((i, (path, parent, _))) = next else {
                    break;
                };
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send((i, Err(Error::Cancelled)));
                    continue;
                }
                let out_dir = root.join(parent);
                let result = std::fs::create_dir_all(&out_dir)
                    .map_err(Error::from)
                    .and_then(|_| {
                        let output = OutputLocation::Directory(out_dir.clone());
                        let unit_tag = format!("{tag}i{i}");
                        let mut quiet = |_: f32| {};
                        let mut sub = Context {
                            caps,
                            cancel,
                            progress: &mut quiet,
                            output: &output,
                            temp_tag: &unit_tag,
                        };
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run_steps(path, steps, None, &mut sub)
                        }))
                        .unwrap_or_else(|_| Err(Error::Internal("unexpected failure".into())))
                    });
                let _ = tx.send((i, result));
            });
        }
        drop(tx);
        for (n, (i, result)) in rx.iter().enumerate() {
            match result {
                Ok(_) => converted += 1,
                Err(Error::NothingToDo(_)) | Err(Error::Cancelled) => {}
                Err(e) => failures.push((jobs[i].2.clone(), e.info().message)),
            }
            progress((n + 1) as f32 / total as f32);
        }
    });
    ctx.check_cancel()?;
    if converted == 0 {
        return Err(match failures.first() {
            None => Error::NothingToDo("nothing in it needed converting".into()),
            Some((name, why)) => Error::Invalid(format!(
                "none of its files could be converted ({name}: {why})"
            )),
        });
    }
    if !failures.is_empty() {
        failures.sort();
        let report: String = failures
            .iter()
            .map(|(name, why)| format!("{name}: {why}\n"))
            .collect();
        std::fs::write(tmp.path().join("Not converted.txt"), report)?;
    }
    let name = format!(
        "{} ({})",
        display_name(input),
        file_part(&label, 40, "converted")
    );
    let out = naming::publish_dir(tmp, &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// The contents of every QR code in a picture, for the clipboard.
pub fn read_codes(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let fmt = format::detect(input)?;
    if !ctx.caps.can_decode_raster(fmt) {
        return Err(Error::UnsupportedInput(format!(
            "QR codes are read from pictures, not {fmt} files"
        )));
    }
    // On a Mac, Vision reads barcodes too; elsewhere (or if it fails),
    // QR codes are read here.
    if ctx.caps.vision.is_some() && fmt != Format::Svg {
        let scratch = tempfile::tempdir()?;
        match vision::codes(ctx.caps, input, scratch.path(), ctx.cancel) {
            Ok(found) if !found.is_empty() => {
                (ctx.progress)(1.0);
                let values: Vec<String> = found.into_iter().map(|c| c.value).collect();
                return Ok(value(values.join("\n")));
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            _ => {}
        }
    }
    let pic = img::decode(input, fmt, ctx.caps)?;
    let found = crate::engines::codes::read(&pic.img);
    (ctx.progress)(1.0);
    if found.is_empty() {
        let what = if ctx.caps.vision.is_some() {
            "no QR code or barcode found"
        } else {
            "no QR code found"
        };
        return Err(Error::NothingToDo(what.into()));
    }
    Ok(value(found.join("\n")))
}

/// Runs the person's shortcut `name` on `input` and saves what it returns
/// beside it: "photo (Resize for Web).jpg". A shortcut that returns
/// nothing (it shared or sent the file, say) saves nothing.
pub fn run_shortcut(input: &Path, name: &str, ctx: &mut Context<'_>) -> Result<Produced> {
    use crate::engines::shortcuts;
    let scratch = tempfile::tempdir()?;
    let results = shortcuts::run(ctx.caps, name, input, scratch.path(), ctx.cancel)?;
    let dir = ctx.output.dir_for(input);
    let label = file_part(name, 60, "shortcut");
    let mut outs = Vec::with_capacity(results.len());
    for result in &results {
        ctx.check_cancel()?;
        let ext = shortcuts::extension_for(result);
        let tmp = naming::temp_in(&dir, ctx.temp_tag, &ext)?;
        std::fs::copy(result, tmp.path())?;
        naming::inherit_permissions(input, tmp.path());
        let desired = naming::desired_path(input, &dir, &ext, Some(&label));
        outs.push(naming::publish(tmp.into_temp_path(), &desired)?);
    }
    (ctx.progress)(1.0);
    Ok(Produced::many(outs))
}

/// A QR code (PNG) of a text file's contents, beside it.
pub fn make_qr(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let fmt = format::detect(input)?;
    if fmt != Format::Txt {
        return Err(Error::UnsupportedInput(format!(
            "QR codes are made from text files, not {fmt} files"
        )));
    }
    let code = crate::engines::codes::make(&text::read_text(input)?)?;
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "png")?;
    code.save_with_format(tmp.path(), image::ImageFormat::Png)?;
    naming::inherit_permissions(input, tmp.path());
    let desired = naming::desired_path(input, &dir, "png", Some("QR code"));
    let out = naming::publish(tmp.into_temp_path(), &desired)?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Icons from a picture: a favicon set, or an Xcode app icon set, in a
/// folder beside it.
pub fn icon_set(
    input: &Path,
    set: crate::ops::IconKind,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    use crate::engines::icons;
    let fmt = format::detect(input)?;
    if !ctx.caps.can_decode_raster(fmt) {
        return Err(Error::UnsupportedInput(format!(
            "icons are made from pictures, not {fmt} files"
        )));
    }
    let pic = img::decode(input, fmt, ctx.caps)?;
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let suffix = match set {
        crate::ops::IconKind::Favicon => {
            let svg = (fmt == Format::Svg).then_some(input);
            icons::favicons(&pic.img, svg, tmp.path())?;
            "favicons"
        }
        crate::ops::IconKind::Ios => {
            icons::ios(&pic.img, tmp.path())?;
            "iOS icon"
        }
    };
    let name = format!("{} ({suffix})", doc::title_of(input));
    let out = naming::publish_dir(tmp, &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

fn require_folder(input: &Path) -> Result<()> {
    if std::fs::symlink_metadata(input)?.is_dir() {
        Ok(())
    } else {
        Err(Error::UnsupportedInput(format!(
            "{} isn't a folder",
            display_name(input)
        )))
    }
}

/// Zips everything selected into one archive: "Name.zip" for one item,
/// "Archive.zip" for several, beside the first.
pub fn zip_all(
    inputs: &[PathBuf],
    password: Option<&str>,
    ctx: &mut Context<'_>,
) -> Result<Produced> {
    let first = inputs
        .first()
        .ok_or_else(|| Error::Invalid("nothing to zip".into()))?;
    let dir = ctx.output.dir_for(first);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "zip")?;
    archive::zip(inputs, tmp.path(), password, ctx.cancel, &mut *ctx.progress)?;
    if inputs.len() == 1 && !first.is_dir() {
        naming::inherit_permissions(first, tmp.path());
    }
    let name = if inputs.len() == 1 {
        format!("{}.zip", display_name(first))
    } else {
        "Archive.zip".to_string()
    };
    let out = naming::publish(tmp.into_temp_path(), &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Packs a folder as a .tar.gz beside it.
pub fn tar_folder(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "tar.gz")?;
    crate::engines::bsdtar::pack(ctx.caps, input, tmp.path(), ctx.cancel, &mut *ctx.progress)?;
    let name = format!("{}.tar.gz", display_name(input));
    let out = naming::publish(tmp.into_temp_path(), &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Extracts an archive beside it: its single top-level folder or file
/// as it is, or everything in a folder named after the archive. ZIPs are
/// opened here; 7-Zip, RAR, TAR and disc images through `bsdtar`.
pub fn unzip(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    unzip_with(input, None, ctx)
}

/// [`unzip`], decrypting a locked ZIP with `password`.
pub fn unzip_with(input: &Path, password: Option<&str>, ctx: &mut Context<'_>) -> Result<Produced> {
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    let fmt = format::detect(input)?;
    // Only ZIP passwords can be used; other archives open as usual.
    if fmt == Format::Zip {
        archive::extract(input, tmp.path(), password, ctx.cancel, &mut *ctx.progress)?;
    } else {
        crate::engines::bsdtar::extract(
            ctx.caps,
            input,
            tmp.path(),
            ctx.cancel,
            &mut *ctx.progress,
        )?;
    }
    let top: Vec<std::fs::DirEntry> = std::fs::read_dir(tmp.path())?.flatten().collect();
    let out = match top.as_slice() {
        [] => return Err(Error::NothingToDo("the archive is empty".into())),
        [only] if only.path().is_dir() => {
            let out = naming::publish_dir_contents(&only.path(), &dir.join(only.file_name()))?;
            drop(tmp);
            out
        }
        [only] => {
            let path = only.path();
            let name = only.file_name();
            naming::publish(tempfile::TempPath::try_from_path(path)?, &dir.join(name))?
        }
        _ => {
            let name = display_name(input);
            let lower = name.to_ascii_lowercase();
            // "Photos.tar.gz" unpacks into "Photos", not "Photos.tar".
            let stem = [".tar.gz", ".tar.bz2", ".tar.xz"]
                .iter()
                .find(|e| lower.ends_with(*e))
                .map(|e| name[..name.len() - e.len()].to_string())
                .or_else(|| input.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Archive".into());
            naming::publish_dir(tmp, &dir.join(stem))?
        }
    };
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Makes a new folder beside `input`, fills it with `fill`, and names it
/// "<folder> (<suffix>)".
fn new_folder(
    input: &Path,
    suffix: &str,
    ctx: &mut Context<'_>,
    fill: impl FnOnce(&Path, &AtomicBool, &mut dyn FnMut(f32)) -> Result<()>,
) -> Result<Produced> {
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_dir_in(&dir, ctx.temp_tag)?;
    fill(tmp.path(), ctx.cancel, &mut *ctx.progress)?;
    if std::fs::read_dir(tmp.path())?.next().is_none() {
        return Err(Error::NothingToDo("there was nothing to copy".into()));
    }
    let name = format!("{} ({suffix})", display_name(input));
    let out = naming::publish_dir(tmp, &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// Copies two or more folders into one new folder.
pub fn merge_folders(inputs: &[PathBuf], ctx: &mut Context<'_>) -> Result<Produced> {
    if inputs.len() < 2 {
        return Err(Error::NothingToDo(
            "choose two or more folders to merge".into(),
        ));
    }
    for i in inputs {
        require_folder(i)?;
    }
    new_folder(&inputs[0], "merged", ctx, |out, cancel, progress| {
        folder::merge(inputs, out, cancel, progress)
    })
}

/// A copy of a folder with every file at the top level.
pub fn flatten(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    require_folder(input)?;
    new_folder(input, "flattened", ctx, |out, cancel, progress| {
        folder::flatten(input, out, cancel, progress)
    })
}

/// A copy of a folder with its files grouped by kind.
pub fn sort_by_kind(input: &Path, ctx: &mut Context<'_>) -> Result<Produced> {
    require_folder(input)?;
    new_folder(input, "sorted", ctx, |out, cancel, progress| {
        folder::sort_by_kind(input, out, cancel, progress)
    })
}

/// A list of a folder's contents, saved beside it.
pub fn list_contents(input: &Path, fmt: Option<Format>, ctx: &mut Context<'_>) -> Result<Produced> {
    require_folder(input)?;
    let fmt = fmt.unwrap_or(Format::Txt);
    if !matches!(fmt, Format::Txt | Format::Csv) {
        return Err(Error::Invalid("lists are saved as TXT or CSV".into()));
    }
    let text = folder::list(input, fmt, ctx.cancel)?;
    let dir = ctx.output.dir_for(input);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, fmt.extension())?;
    std::fs::write(tmp.path(), text)?;
    naming::inherit_permissions(input, tmp.path());
    let name = format!("{} (contents).{}", display_name(input), fmt.extension());
    let out = naming::publish(tmp.into_temp_path(), &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

/// A report of identical files among the selection, saved beside it.
pub fn find_duplicates(inputs: &[PathBuf], ctx: &mut Context<'_>) -> Result<Produced> {
    let first = inputs
        .first()
        .ok_or_else(|| Error::Invalid("nothing to search".into()))?;
    let report = folder::duplicates(inputs, ctx.cancel, &mut *ctx.progress)?;
    let dir = ctx.output.dir_for(first);
    let tmp = naming::temp_in(&dir, ctx.temp_tag, "txt")?;
    std::fs::write(tmp.path(), report)?;
    naming::inherit_permissions(first, tmp.path());
    let name = if inputs.len() == 1 {
        format!("{} (duplicates).txt", display_name(first))
    } else {
        "Duplicates.txt".to_string()
    };
    let out = naming::publish(tmp.into_temp_path(), &dir.join(name))?;
    (ctx.progress)(1.0);
    Ok(Produced::one(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::CompressLevel;

    fn ctx_run(input: &Path, steps: &[Step], out: &OutputLocation) -> Result<Produced> {
        let caps = Capabilities::none();
        let cancel = AtomicBool::new(false);
        let mut noop = |_p: f32| {};
        let mut ctx = Context {
            caps: &caps,
            cancel: &cancel,
            progress: &mut noop,
            output: out,
            temp_tag: "test",
        };
        run_steps(input, steps, None, &mut ctx)
    }

    fn photo(dir: &Path) -> PathBuf {
        let p = dir.join("photo.png");
        image::RgbImage::from_fn(300, 200, |x, y| image::Rgb([x as u8, y as u8, 90]))
            .save(&p)
            .unwrap();
        p
    }

    #[test]
    fn convert_writes_beside_source_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let conv = [Step::Convert {
            to: Format::Jpeg,
            quality: None,
        }];
        let a = ctx_run(&p, &conv, &OutputLocation::BesideSource)
            .unwrap()
            .output
            .unwrap();
        let b = ctx_run(&p, &conv, &OutputLocation::BesideSource)
            .unwrap()
            .output
            .unwrap();
        assert_eq!(a, dir.path().join("photo.jpg"));
        assert_eq!(b, dir.path().join("photo 2.jpg"));
        assert_eq!(format::detect(&b).unwrap(), Format::Jpeg);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with(naming::TEMP_PREFIX)),
            "no temp files left: {names:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn outputs_get_source_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let out = ctx_run(
            &p,
            &[Step::Convert {
                to: Format::Jpeg,
                quality: None,
            }],
            &OutputLocation::BesideSource,
        )
        .unwrap()
        .output
        .unwrap();
        assert_eq!(
            std::fs::metadata(out).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn chain_encodes_once_and_names_by_step() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let out_dir = dir.path().join("out");
        let steps = [Step::resize_max(150), Step::Rotate { degrees: 90 }];
        let out = ctx_run(&p, &steps, &OutputLocation::Directory(out_dir.clone()))
            .unwrap()
            .output
            .unwrap();
        assert_eq!(out, out_dir.join("photo (rotated).png"));
        let i = image::open(&out).unwrap();
        assert_eq!((i.width(), i.height()), (100, 150));
    }

    #[test]
    fn same_format_convert_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let r = ctx_run(
            &p,
            &[Step::Convert {
                to: Format::Png,
                quality: None,
            }],
            &OutputLocation::BesideSource,
        );
        assert!(matches!(r, Err(Error::NothingToDo(_))));
    }

    #[test]
    fn unsupported_route_fails_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let r = ctx_run(
            &p,
            &[Step::Convert {
                to: Format::Mp3,
                quality: None,
            }],
            &OutputLocation::BesideSource,
        );
        assert!(matches!(r, Err(Error::UnsupportedRoute { .. })));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn strip_jpeg_metadata_losslessly() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("cam.jpg");
        image::RgbImage::from_pixel(16, 16, image::Rgb([9, 9, 9]))
            .save(&jpg)
            .unwrap();
        // Insert a fake EXIF segment after SOI.
        let mut data = std::fs::read(&jpg).unwrap();
        let mut exif = vec![0xFF, 0xE1, 0, 20];
        exif.extend_from_slice(b"Exif\0\0GPS-SECRET");
        exif.extend_from_slice(&[0, 0]);
        data.splice(2..2, exif);
        std::fs::write(&jpg, &data).unwrap();

        let out = ctx_run(&jpg, &[Step::StripMetadata], &OutputLocation::BesideSource)
            .unwrap()
            .output
            .unwrap();
        assert_eq!(out, dir.path().join("cam (clean).jpg"));
        let cleaned = std::fs::read(&out).unwrap();
        assert!(!String::from_utf8_lossy(&cleaned).contains("GPS-SECRET"));
        assert_eq!(
            cleaned.len(),
            data.len() - 22,
            "only the segment was removed"
        );
    }

    #[test]
    fn compress_never_grows_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("tiny.jpg");
        image::RgbImage::from_pixel(8, 8, image::Rgb([1, 1, 1]))
            .save_with_format(&jpg, image::ImageFormat::Jpeg)
            .unwrap();
        let r = ctx_run(
            &jpg,
            &[Step::compress(CompressLevel::Light)],
            &OutputLocation::BesideSource,
        );
        assert!(matches!(r, Err(Error::NothingToDo(_))), "{r:?}");
    }

    #[test]
    fn image_to_pdf_and_text_routes() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let pdf_out = ctx_run(
            &p,
            &[Step::Convert {
                to: Format::Pdf,
                quality: None,
            }],
            &OutputLocation::BesideSource,
        )
        .unwrap();
        assert_eq!(
            format::detect(pdf_out.output.as_ref().unwrap()).unwrap(),
            Format::Pdf
        );

        let srt = dir.path().join("subs.srt");
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
        let vtt = ctx_run(
            &srt,
            &[Step::Convert {
                to: Format::Vtt,
                quality: None,
            }],
            &OutputLocation::BesideSource,
        )
        .unwrap();
        assert!(std::fs::read_to_string(vtt.output.unwrap())
            .unwrap()
            .starts_with("WEBVTT"));
    }

    #[test]
    fn pictures_trace_into_svg_drawings() {
        let dir = tempfile::tempdir().unwrap();
        let caps = Capabilities::none();
        let logo = dir.path().join("logo.png");
        let mut img = image::RgbaImage::from_pixel(96, 64, image::Rgba([0, 0, 0, 0]));
        for y in 16..48 {
            for x in 16..80 {
                img.put_pixel(x, y, image::Rgba([30, 90, 200, 255]));
            }
        }
        img.save(&logo).unwrap();
        // Converting to SVG traces it.
        let svg = run_with(
            &caps,
            &logo,
            &[Step::Convert {
                to: Format::Svg,
                quality: None,
            }],
        )
        .unwrap()
        .output
        .unwrap();
        assert_eq!(svg.file_name().unwrap(), "logo.svg");
        let text = std::fs::read_to_string(&svg).unwrap();
        assert!(text.contains("width=\"96\" height=\"64\""), "{text}");
        assert_eq!(
            text.matches("<path").count(),
            1,
            "the blue bar only: {text}"
        );
        // The drawing reads back as an image.
        assert_eq!(format::detect(&svg).unwrap(), Format::Svg);
        // Earlier steps run on the pixels first: crop the left half, then
        // trace in black and white.
        let half = run_with(
            &caps,
            &logo,
            &[
                Step::Crop {
                    rect: crate::ops::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.5,
                        h: 1.0,
                    },
                },
                Step::Trace {
                    color: false,
                    detail: TraceDetail::Low,
                },
            ],
        )
        .unwrap()
        .output
        .unwrap();
        let text = std::fs::read_to_string(&half).unwrap();
        assert!(text.contains("width=\"48\" height=\"64\""), "{text}");
        assert!(text.contains("fill=\"#000000\""));
        // A drawing has nothing to trace.
        assert!(matches!(
            run_with(
                &caps,
                &svg,
                &[Step::Trace {
                    color: true,
                    detail: TraceDetail::Medium,
                }]
            ),
            Err(Error::UnsupportedInput(_))
        ));
    }

    fn run_with(caps: &Capabilities, input: &Path, steps: &[Step]) -> Result<Produced> {
        let cancel = AtomicBool::new(false);
        let mut noop = |_p: f32| {};
        let out = OutputLocation::BesideSource;
        let mut ctx = Context {
            caps,
            cancel: &cancel,
            progress: &mut noop,
            output: &out,
            temp_tag: "t2",
        };
        run_steps(input, steps, None, &mut ctx)
    }

    fn pdf_of(dir: &Path, pages: u32) -> PathBuf {
        let caps = Capabilities::none();
        let mut b = pdf::PdfBuilder::new();
        for i in 1..=pages {
            let p = dir.join(format!("pg{i}.png"));
            // Page i is solid red, 100 px wide and 50 * i px tall.
            image::RgbImage::from_pixel(100, 50 * i, image::Rgb([220, 20, 20]))
                .save(&p)
                .unwrap();
            b.add_file(&p, Format::Png, &caps).unwrap();
            std::fs::remove_file(&p).unwrap();
        }
        let out = dir.join("report.pdf");
        b.finish(&out).unwrap();
        out
    }

    fn pdfium_caps() -> Option<Capabilities> {
        let lib = std::env::var_os("RADLET_PDFIUM").map(PathBuf::from)?;
        Some(Capabilities {
            pdfium: Some(lib),
            ..Default::default()
        })
    }

    #[test]
    fn pdf_pages_become_images() {
        let Some(caps) = pdfium_caps() else {
            eprintln!("skipping: set RADLET_PDFIUM to libpdfium");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let src = pdf_of(dir.path(), 2);
        let out = run_with(
            &caps,
            &src,
            &[Step::Convert {
                to: Format::Png,
                quality: None,
            }],
        )
        .unwrap();
        assert_eq!(
            out.outputs,
            vec![
                dir.path().join("report - page 1.png"),
                dir.path().join("report - page 2.png")
            ]
        );
        let p1 = image::open(&out.outputs[0]).unwrap().to_rgb8();
        let p2 = image::open(&out.outputs[1]).unwrap().to_rgb8();
        assert!(p2.height() > p1.height(), "pages rendered in order");
        let c = p1.get_pixel(p1.width() / 2, p1.height() / 2).0;
        assert!(
            c[0] > 180 && c[1] < 80 && c[2] < 80,
            "page content rendered: {c:?}"
        );

        // A chain continues per page; many pages go into a folder.
        let big = pdf_of(dir.path(), 6);
        let out = run_with(
            &caps,
            &big,
            &[
                Step::Convert {
                    to: Format::Jpeg,
                    quality: Some(80),
                },
                Step::resize_max(40),
            ],
        )
        .unwrap();
        assert_eq!(out.outputs, vec![dir.path().join("report (pages)")]);
        let mut names: Vec<String> = std::fs::read_dir(&out.outputs[0])
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 6);
        assert_eq!(names[0], "report - page 1.jpg");
        let first = image::open(out.outputs[0].join(&names[0])).unwrap();
        assert!(first.width().max(first.height()) <= 40);
    }

    #[test]
    fn rotated_pdf_pages_render_upright() {
        let Some(caps) = pdfium_caps() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = pdf_of(dir.path(), 1);
        let plain = run_with(
            &caps,
            &src,
            &[Step::Convert {
                to: Format::Png,
                quality: None,
            }],
        )
        .unwrap();
        let plain = image::open(plain.output.unwrap()).unwrap();
        let mut doc = lopdf::Document::load(&src).unwrap();
        let page = *doc.get_pages().values().next().unwrap();
        doc.get_dictionary_mut(page).unwrap().set("Rotate", 90);
        let rotated = dir.path().join("rotated.pdf");
        doc.save(&rotated).unwrap();
        let out = run_with(
            &caps,
            &rotated,
            &[Step::Convert {
                to: Format::Png,
                quality: None,
            }],
        )
        .unwrap();
        let img = image::open(out.output.unwrap()).unwrap();
        assert_eq!(
            (img.width(), img.height()),
            (plain.height(), plain.width()),
            "/Rotate is applied"
        );
    }

    #[test]
    fn page_editing_and_split() {
        let caps = Capabilities::none();
        let dir = tempfile::tempdir().unwrap();
        let src = pdf_of(dir.path(), 5);
        let out = run_with(
            &caps,
            &src,
            &[Step::Pages {
                pages: vec![5, 1],
                rotate: vec![],
            }],
        )
        .unwrap();
        assert_eq!(
            out.output.as_deref(),
            Some(dir.path().join("report (edited).pdf").as_path())
        );
        assert_eq!(pdf::page_count(out.output.as_ref().unwrap()).unwrap(), 2);

        let cancel = AtomicBool::new(false);
        let mut noop = |_p: f32| {};
        let loc = OutputLocation::BesideSource;
        let mut ctx = Context {
            caps: &caps,
            cancel: &cancel,
            progress: &mut noop,
            output: &loc,
            temp_tag: "sp",
        };
        let parts = split_pdf(
            &src,
            &SplitMode::Ranges {
                ranges: "1-2, 3-".into(),
            },
            &mut ctx,
        )
        .unwrap();
        assert_eq!(
            parts.outputs,
            vec![
                dir.path().join("report (pages 1-2).pdf"),
                dir.path().join("report (pages 3-5).pdf")
            ]
        );
        let parts = split_pdf(&src, &SplitMode::EveryPage, &mut ctx).unwrap();
        assert_eq!(parts.outputs, vec![dir.path().join("report (split)")]);
        assert_eq!(std::fs::read_dir(&parts.outputs[0]).unwrap().count(), 5);
    }

    #[test]
    fn crop_redact_and_target_through_the_pipeline() {
        use crate::ops::{Rect, RedactStyle};
        let caps = Capabilities::none();
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let out = run_with(
            &caps,
            &p,
            &[Step::Crop {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.5,
                    h: 0.5,
                },
            }],
        )
        .unwrap();
        let c = image::open(out.output.unwrap()).unwrap();
        assert_eq!((c.width(), c.height()), (150, 100));

        let out = run_with(
            &caps,
            &p,
            &[Step::Redact {
                rects: vec![Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 0.5,
                }],
                style: RedactStyle::Black,
            }],
        )
        .unwrap();
        assert_eq!(
            out.output.as_deref(),
            Some(dir.path().join("photo (redacted).png").as_path())
        );
        assert_eq!(
            image::open(out.output.unwrap())
                .unwrap()
                .to_rgb8()
                .get_pixel(10, 10)
                .0,
            [0, 0, 0]
        );

        let jpg = dir.path().join("big.jpg");
        image::RgbImage::from_fn(1200, 900, |x, y| {
            let v = (x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) as u8;
            image::Rgb([v, v.wrapping_add(40), v.wrapping_mul(5)])
        })
        .save_with_format(&jpg, image::ImageFormat::Jpeg)
        .unwrap();
        let target = std::fs::metadata(&jpg).unwrap().len() / 4;
        let out = run_with(
            &caps,
            &jpg,
            &[Step::Compress {
                level: CompressLevel::Balanced,
                target_bytes: Some(target),
            }],
        )
        .unwrap();
        assert!(std::fs::metadata(out.output.unwrap()).unwrap().len() <= target);
    }

    #[test]
    fn video_crop_redact_and_target() {
        let caps = Capabilities::detect();
        if caps.ffmpeg.is_none() || caps.ffprobe.is_none() {
            eprintln!("skipping: needs ffmpeg and ffprobe");
            return;
        }
        use crate::ops::Rect;
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.mp4");
        let ok = std::process::Command::new(caps.ffmpeg.as_ref().unwrap())
            .args([
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=25",
                "-f",
                "lavfi",
                "-i",
                "sine",
                "-t",
                "6",
            ])
            .args([
                "-vf",
                "noise=alls=40:allf=t+u",
                "-c:v",
                "libx264",
                "-crf",
                "8",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
            ])
            .arg(&clip)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let probe_dims = |p: &Path| -> String {
            let o = std::process::Command::new(caps.ffprobe.as_ref().unwrap())
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "stream=width,height",
                    "-of",
                    "csv=p=0",
                ])
                .arg(p)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let out = run_with(
            &caps,
            &clip,
            &[Step::Crop {
                rect: Rect {
                    x: 0.25,
                    y: 0.25,
                    w: 0.5,
                    h: 0.5,
                },
            }],
        )
        .unwrap();
        assert_eq!(probe_dims(out.output.as_ref().unwrap()), "160,120");
        let out = run_with(
            &caps,
            &clip,
            &[Step::Redact {
                rects: vec![Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.5,
                    h: 0.5,
                }],
                style: Default::default(),
            }],
        )
        .unwrap();
        assert_eq!(probe_dims(out.output.as_ref().unwrap()), "320,240");
        let original = std::fs::metadata(&clip).unwrap().len();
        let target = 300_000;
        assert!(original > target * 2, "test clip is big enough: {original}");
        let out = run_with(
            &caps,
            &clip,
            &[Step::Compress {
                level: CompressLevel::Balanced,
                target_bytes: Some(target),
            }],
        )
        .unwrap();
        let size = std::fs::metadata(out.output.unwrap()).unwrap().len();
        assert!(
            size <= target * 13 / 10,
            "close to the 300 KB target: {size}"
        );
    }

    /// A stand-in for the Swift `radlet-vision` helper, honouring its
    /// contract: text, mask and searchable commands, exit 3 for "nothing
    /// found", progress lines on stdout. It logs what it was given.
    #[cfg(unix)]
    fn fake_vision(dir: &Path) -> (Capabilities, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        // Mask: left half white, at half the photo's size (the engine
        // scales it up).
        let mask = dir.join("fake-mask.png");
        image::GrayImage::from_fn(150, 100, |x, _| image::Luma([if x < 75 { 255 } else { 0 }]))
            .save(&mask)
            .unwrap();
        let fixture = dir.join("fixture");
        std::fs::create_dir_all(&fixture).unwrap();
        let pdf = pdf_of(&fixture, 1);
        let log = dir.join("vision.log");
        let script = dir.join("radlet-vision");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
cmd="$1"; in="$2"; out="$3"
name=$(basename "$in")
echo "$cmd $name" >> '{log}'
case "$name" in
  *fail*) echo "Vision exploded" >&2; exit 1;;
  *blank*) echo "no text found" >&2; exit 3;;
esac
case "$cmd" in
  transcribe)
    shift 3
    echo "transcribe $in $#" >> '{log}'
    printf '{{"locale":"en-US","chunks":[' > "$out"
    sep=""
    for c in "$@"; do
      printf '%s[{{"t":"Hello","s":0.5,"d":0.3}},{{"t":"world.","s":0.9,"d":0.4}}]' "$sep" >> "$out"
      sep=","
    done
    printf ']}}' >> "$out"
    echo "progress 1";;
  text) echo "progress 0.5"; printf 'Hello from %s\n' "$name" > "$out";;
  mask) cp '{mask}' "$out";;
  searchable) cp '{pdf}' "$out";;
  *) echo "unknown command" >&2; exit 2;;
esac
"#,
                log = log.display(),
                mask = mask.display(),
                pdf = pdf.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let caps = Capabilities {
            vision: Some(script),
            ..Default::default()
        };
        (caps, log)
    }

    #[cfg(unix)]
    #[test]
    fn subtitles_from_speech() {
        let detected = Capabilities::detect();
        if detected.ffmpeg.is_none() || detected.ffprobe.is_none() {
            eprintln!("skipping: needs ffmpeg and ffprobe");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (fake, log) = fake_vision(dir.path());
        let caps = Capabilities {
            vision: fake.vision,
            ..detected
        };
        // 70 seconds: two chunks for the recogniser.
        let clip = dir.path().join("talk.mp4");
        make_clip(&caps, &clip, 70, "volume=0.5");
        let srt = run_with(
            &caps,
            &clip,
            &[Step::Transcribe {
                to: Format::Srt,
                language: Some("en-GB".into()),
            }],
        )
        .unwrap()
        .output
        .unwrap();
        assert_eq!(srt.file_name().unwrap(), "talk.srt");
        let text = std::fs::read_to_string(&srt).unwrap();
        assert!(text.starts_with("1\n00:00:00,500 --> 00:00:01,"), "{text}");
        assert_eq!(text.matches("Hello world.").count(), 2, "{text}");
        let second = text.split("\n\n").nth(1).unwrap();
        assert!(
            second.starts_with("2\n00:00:4") || second.starts_with("2\n00:00:5"),
            "{second}"
        );
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("transcribe en-GB 2"), "{logged}");

        let vtt = run_with(
            &caps,
            &clip,
            &[Step::Transcribe {
                to: Format::Vtt,
                language: None,
            }],
        )
        .unwrap()
        .output
        .unwrap();
        assert!(std::fs::read_to_string(&vtt)
            .unwrap()
            .starts_with("WEBVTT\n\n00:00:00.500"));
        assert!(std::fs::read_to_string(&log)
            .unwrap()
            .contains("transcribe auto 2"));

        let bad = run_with(
            &caps,
            &clip,
            &[Step::Transcribe {
                to: Format::Srt,
                language: Some("en US; rm".into()),
            }],
        );
        assert!(matches!(bad, Err(Error::Invalid(_))), "{bad:?}");

        // A video without sound has nothing to transcribe.
        let silent = dir.path().join("silent.mp4");
        let ok = std::process::Command::new(caps.ffmpeg.as_ref().unwrap())
            .args([
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=5",
                "-t",
                "1",
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&silent)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let none = run_with(
            &caps,
            &silent,
            &[Step::Transcribe {
                to: Format::Srt,
                language: None,
            }],
        );
        assert!(matches!(none, Err(Error::NothingToDo(_))), "{none:?}");
    }

    #[cfg(unix)]
    #[test]
    fn vision_tools_through_the_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let (caps, log) = fake_vision(dir.path());
        let p = photo(dir.path());

        // Remove background: the mask becomes alpha, the hidden part is
        // cleared, and a JPEG becomes a PNG.
        let cut = run_with(&caps, &p, &[Step::RemoveBackground { crop: false }]).unwrap();
        let out = cut.output.unwrap();
        assert_eq!(out.file_name().unwrap(), "photo (cutout).png");
        let rgba = image::open(&out).unwrap().to_rgba8();
        assert_eq!(rgba.dimensions(), (300, 200));
        assert_eq!(rgba.get_pixel(10, 10).0[3], 255);
        assert_eq!(rgba.get_pixel(290, 10).0, [0, 0, 0, 0]);
        let cropped = run_with(&caps, &p, &[Step::RemoveBackground { crop: true }]).unwrap();
        let w = image::open(cropped.output.unwrap()).unwrap().width();
        assert!((148..=152).contains(&w), "trimmed to the subject: {w}");
        let jpg = dir.path().join("shot.jpg");
        image::open(&p).unwrap().to_rgb8().save(&jpg).unwrap();
        let from_jpeg = run_with(&caps, &jpg, &[Step::RemoveBackground { crop: false }]).unwrap();
        assert_eq!(from_jpeg.output.unwrap().file_name().unwrap(), "shot.png");

        // Image to text: the helper reads the original file...
        let txt = run_with(
            &caps,
            &p,
            &[Step::Convert {
                to: Format::Txt,
                quality: None,
            }],
        )
        .unwrap();
        let txt = txt.output.unwrap();
        assert_eq!(txt.file_name().unwrap(), "photo.txt");
        assert_eq!(
            std::fs::read_to_string(&txt).unwrap(),
            "Hello from photo.png\n"
        );
        // ...or the edited pixels, after earlier steps.
        let after_crop = run_with(
            &caps,
            &p,
            &[
                Step::Crop {
                    rect: crate::ops::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.5,
                        h: 0.5,
                    },
                },
                Step::Convert {
                    to: Format::Txt,
                    quality: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(after_crop.output.unwrap()).unwrap(),
            "Hello from vision-input.png\n"
        );

        // Searchable PDFs from an image and from a PDF.
        let from_image = run_with(&caps, &p, &[Step::MakeSearchable]).unwrap();
        assert_eq!(from_image.output.unwrap().file_name().unwrap(), "photo.pdf");
        let scan = pdf_of(dir.path(), 2);
        let from_pdf = run_with(&caps, &scan, &[Step::MakeSearchable]).unwrap();
        assert_eq!(
            from_pdf.output.unwrap().file_name().unwrap(),
            "report (searchable).pdf"
        );

        // A PDF's text goes through the helper too.
        let pdf_txt = run_with(
            &caps,
            &scan,
            &[Step::Convert {
                to: Format::Txt,
                quality: None,
            }],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(pdf_txt.output.unwrap()).unwrap(),
            "Hello from report.pdf\n"
        );

        // Nothing found, and a helper failure, are reported as such.
        let blank = dir.path().join("blank.png");
        std::fs::copy(&p, &blank).unwrap();
        let none = run_with(
            &caps,
            &blank,
            &[Step::Convert {
                to: Format::Txt,
                quality: None,
            }],
        );
        assert!(
            matches!(none, Err(Error::NothingToDo(ref m)) if m == "no text found"),
            "{none:?}"
        );
        let fail = dir.path().join("fail.png");
        std::fs::copy(&p, &fail).unwrap();
        let err = run_with(&caps, &fail, &[Step::MakeSearchable]);
        assert!(
            matches!(err, Err(Error::Helper(ref m)) if m == "Vision exploded"),
            "{err:?}"
        );
        // A scanned PDF the helper can't read, with no text layer either.
        let fail_pdf = dir.path().join("fail.pdf");
        std::fs::copy(&scan, &fail_pdf).unwrap();
        let err = run_with(
            &caps,
            &fail_pdf,
            &[Step::Convert {
                to: Format::Txt,
                quality: None,
            }],
        );
        assert!(matches!(err, Err(Error::Helper(_))), "{err:?}");

        // Copy text: a value, no file.
        let cancel = AtomicBool::new(false);
        let mut noop = |_p: f32| {};
        let out = OutputLocation::BesideSource;
        let mut ctx = Context {
            caps: &caps,
            cancel: &cancel,
            progress: &mut noop,
            output: &out,
            temp_tag: "t3",
        };
        let copied = copy_text(&p, &mut ctx).unwrap();
        assert_eq!(copied.value.as_deref(), Some("Hello from photo.png"));
        assert!(copied.outputs.is_empty());

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("mask cutout-input.png"), "{calls}");
        assert!(calls.contains("searchable report.pdf"), "{calls}");
        // No temporary files are left next to the originals.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".radlet-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn vision_tools_need_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path());
        let err = ctx_run(
            &p,
            &[Step::RemoveBackground { crop: false }],
            &OutputLocation::BesideSource,
        );
        assert!(matches!(err, Err(Error::UnsupportedInput(_))), "{err:?}");
    }

    fn make_clip(caps: &Capabilities, out: &Path, seconds: u32, audio_filter: &str) {
        let ok = std::process::Command::new(caps.ffmpeg.as_ref().unwrap())
            .args([
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=25",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440,{audio_filter}"),
            ])
            .args([
                "-t",
                &seconds.to_string(),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .args(["-c:a", "aac", "-b:a", "128k"])
            .arg(out)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    fn duration(caps: &Capabilities, p: &Path) -> f64 {
        media::probe(caps, p).duration_s.unwrap()
    }

    #[test]
    fn trim_gif_stills_and_loudness() {
        let caps = Capabilities::detect();
        if caps.ffmpeg.is_none() || caps.ffprobe.is_none() {
            eprintln!("skipping: needs ffmpeg and ffprobe");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.mp4");
        make_clip(&caps, &clip, 6, "volume=0.05");

        // Exact trim: 1.5 s to 4 s.
        let cut = run_with(
            &caps,
            &clip,
            &[Step::Trim {
                start: 1.5,
                end: Some(4.0),
            }],
        )
        .unwrap();
        let cut = cut.output.unwrap();
        assert_eq!(cut.file_name().unwrap(), "clip (trimmed).mp4");
        let d = duration(&caps, &cut);
        assert!((d - 2.5).abs() < 0.2, "trimmed to {d}");
        let tail = run_with(
            &caps,
            &clip,
            &[Step::Trim {
                start: 5.0,
                end: None,
            }],
        )
        .unwrap();
        assert!((duration(&caps, &tail.output.unwrap()) - 1.0).abs() < 0.2);
        assert!(run_with(
            &caps,
            &clip,
            &[Step::Trim {
                start: 3.0,
                end: Some(2.0)
            }]
        )
        .is_err());
        assert!(run_with(
            &caps,
            &clip,
            &[Step::Trim {
                start: 60.0,
                end: None
            }]
        )
        .is_err());

        // A GIF of part of the clip.
        let gif = run_with(
            &caps,
            &clip,
            &[
                Step::Trim {
                    start: 1.0,
                    end: Some(2.0),
                },
                Step::Convert {
                    to: Format::Gif,
                    quality: None,
                },
            ],
        )
        .unwrap();
        let gif = gif.output.unwrap();
        assert_eq!(gif.extension().unwrap(), "gif");
        assert!(meta::gif_frames(&std::fs::read(&gif).unwrap()).unwrap() >= 10);

        // Stills: three beside the clip, twelve in a folder.
        let cancel = AtomicBool::new(false);
        let mut noop = |_p: f32| {};
        let out = OutputLocation::BesideSource;
        let mut ctx = Context {
            caps: &caps,
            cancel: &cancel,
            progress: &mut noop,
            output: &out,
            temp_tag: "t4",
        };
        let three = extract_frames(&clip, 3, None, &mut ctx).unwrap();
        let names: Vec<String> = three
            .outputs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "clip - still 1.jpg",
                "clip - still 2.jpg",
                "clip - still 3.jpg"
            ]
        );
        assert_eq!(image::open(&three.outputs[0]).unwrap().width(), 160);
        let twelve = extract_frames(&clip, 12, Some(Format::Png), &mut ctx).unwrap();
        assert_eq!(twelve.outputs.len(), 1);
        assert!(twelve.outputs[0].is_dir());
        assert_eq!(std::fs::read_dir(&twelve.outputs[0]).unwrap().count(), 12);
        assert!(extract_frames(&clip, 0, None, &mut ctx).is_err());

        // Loudness: a very quiet clip comes out much louder, video copied.
        let loud = run_with(&caps, &clip, &[Step::NormalizeAudio]).unwrap();
        let loud = loud.output.unwrap();
        assert_eq!(loud.file_name().unwrap(), "clip (normalized).mp4");
        let mean_volume = |p: &Path| -> f64 {
            let o = std::process::Command::new(caps.ffmpeg.as_ref().unwrap())
                .args(["-hide_banner", "-nostats", "-i"])
                .arg(p)
                .args(["-af", "volumedetect", "-f", "null", "-"])
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&o.stderr);
            let line = err
                .lines()
                .find(|l| l.contains("mean_volume"))
                .unwrap()
                .to_string();
            line.split("mean_volume:")
                .nth(1)
                .unwrap()
                .trim()
                .trim_end_matches(" dB")
                .parse()
                .unwrap()
        };
        let (before, after) = (mean_volume(&clip), mean_volume(&loud));
        assert!(after > before + 10.0, "{before} dB → {after} dB");

        // Keeping the whole clip changes nothing.
        assert!(matches!(
            run_with(
                &caps,
                &clip,
                &[Step::Trim {
                    start: 0.0,
                    end: None
                }]
            ),
            Err(Error::NothingToDo(_))
        ));

        // Video that stops before the audio: stills come from the video.
        let short = dir.path().join("short video.mp4");
        let ok = std::process::Command::new(caps.ffmpeg.as_ref().unwrap())
            .args(["-loglevel", "error", "-f", "lavfi", "-t", "2", "-i"])
            .arg("testsrc=size=160x120:rate=25")
            .args(["-f", "lavfi", "-t", "6", "-i", "sine=frequency=440"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&short)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let stills = extract_frames(&short, 3, None, &mut ctx).unwrap();
        assert_eq!(stills.outputs.len(), 3);
        assert!(stills
            .outputs
            .iter()
            .all(|p| p.metadata().unwrap().len() > 0));
        let many = extract_frames(&short, 40, None, &mut ctx).unwrap();
        assert_eq!(std::fs::read_dir(&many.outputs[0]).unwrap().count(), 40);

        // Audio-only files too.
        let wav = dir.path().join("talk.wav");
        let ok = std::process::Command::new(caps.ffmpeg.as_ref().unwrap())
            .args([
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=300,volume=0.05",
                "-t",
                "3",
            ])
            .arg(&wav)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let wav_loud = run_with(&caps, &wav, &[Step::NormalizeAudio]).unwrap();
        assert_eq!(
            wav_loud.output.unwrap().file_name().unwrap(),
            "talk (normalized).wav"
        );
        let wav_cut = run_with(
            &caps,
            &wav,
            &[Step::Trim {
                start: 1.0,
                end: None,
            }],
        )
        .unwrap();
        assert!((duration(&caps, &wav_cut.output.unwrap()) - 2.0).abs() < 0.1);
    }

    /// Page numbers and watermarks land where a reader expects them, even
    /// on a rotated page (checked by rendering with PDFium).
    #[test]
    fn stamps_render_upright() {
        let Some(caps) = pdfium_caps() else {
            eprintln!("skipping: RADLET_PDFIUM not set");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // A white page, so dark text is easy to find.
        let white = dir.path().join("white.png");
        image::RgbImage::from_pixel(600, 800, image::Rgb([255, 255, 255]))
            .save(&white)
            .unwrap();
        let mut b = pdf::PdfBuilder::new();
        b.add_file(&white, Format::Png, &caps).unwrap();
        let page = dir.path().join("page.pdf");
        b.finish(&page).unwrap();
        let rotated = run_with(&caps, &page, &[Step::Rotate { degrees: 90 }])
            .unwrap()
            .output
            .unwrap();
        let numbered = run_with(
            &caps,
            &rotated,
            &[Step::PageNumbers {
                position: crate::ops::NumberPosition::BottomRight,
                style: crate::ops::NumberStyle::Number,
                start: 7,
                size: Some(24.0),
                skip_first: false,
            }],
        )
        .unwrap()
        .output
        .unwrap();
        let mut dark: Vec<(u32, u32)> = Vec::new();
        let mut size = (0, 0);
        pdfium::render(&caps, &numbered, 72, None, &mut |_, _, img| {
            let rgb = img.to_rgb8();
            size = rgb.dimensions();
            for (x, y, p) in rgb.enumerate_pixels() {
                if p.0[0] < 100 && p.0[1] < 100 && p.0[2] < 100 {
                    dark.push((x, y));
                }
            }
            Ok(())
        })
        .unwrap();
        // The rotated page is shown landscape, and the number sits at its
        // bottom right as seen.
        assert!(size.0 > size.1, "landscape: {size:?}");
        assert!(!dark.is_empty());
        let (w, h) = (size.0 as f32, size.1 as f32);
        assert!(
            dark.iter()
                .all(|&(x, y)| x as f32 > w * 0.8 && y as f32 > h * 0.85),
            "{:?}",
            &dark[..dark.len().min(5)]
        );
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Web-ready image!"), "web-ready-image");
        assert_eq!(slug("  "), "loadout");
    }
}
