//! Batches: splitting a request into units, running them in parallel, and
//! (optionally) isolating each unit in its own worker process so a crash,
//! hang or runaway decoder only fails that one file.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::assemble;
use crate::catalog::Capabilities;
use crate::error::{Error, ErrorInfo};
use crate::format::{self, Category};
use crate::naming::{self, OutputLocation};
use crate::ops::{Action, Step};
use crate::pipeline::{self, Context};

/// Options that apply to a whole request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOptions {
    #[serde(default)]
    pub output: OutputLocation,
    /// Units run at once. Defaults to the CPU count (2 for media).
    #[serde(default)]
    pub parallelism: Option<usize>,
    /// Per-unit time limit. Defaults to 10 minutes, or 4 hours for media.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// The smallest piece of work: one file through a chain, or one combine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub index: usize,
    pub inputs: Vec<PathBuf>,
    pub action: Action,
    pub output: OutputLocation,
    pub temp_tag: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    /// Nothing needed doing (already in that format, already small).
    Skipped,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub index: usize,
    pub inputs: Vec<PathBuf>,
    pub status: Status,
    /// The main output (first of `outputs`), kept for simple clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    /// Every top-level item created: files, or one folder of many files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    pub bytes_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_out: Option<u64>,
    pub millis: u64,
}

impl Outcome {
    fn from_error(unit: &Unit, e: &Error, started: Instant) -> Outcome {
        let status = match e {
            Error::NothingToDo(_) => Status::Skipped,
            Error::Cancelled => Status::Cancelled,
            _ => Status::Failed,
        };
        Outcome {
            index: unit.index,
            inputs: unit.inputs.clone(),
            status,
            output: None,
            outputs: Vec::new(),
            value: None,
            error: Some(e.info()),
            bytes_in: input_bytes(unit),
            bytes_out: None,
            millis: started.elapsed().as_millis() as u64,
        }
    }
}

fn input_bytes(unit: &Unit) -> u64 {
    unit.inputs.iter().map(|p| naming::size_of(p)).sum()
}

/// A short, unique-per-process tag for temporary file names.
pub fn new_job_tag() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}{n:x}", std::process::id(), t)
}

/// Splits a request into units: one per file, except a combine, which is
/// one unit over every file.
pub fn plan(
    inputs: &[PathBuf],
    action: &Action,
    output: &OutputLocation,
    job_tag: &str,
) -> Vec<Unit> {
    let unit = |index: usize, inputs: Vec<PathBuf>| Unit {
        index,
        inputs,
        action: action.clone(),
        output: output.clone(),
        temp_tag: format!("{job_tag}u{index}"),
    };
    match action {
        a if a.is_whole_selection() => vec![unit(0, inputs.to_vec())],
        _ => inputs
            .iter()
            .enumerate()
            .map(|(i, p)| unit(i, vec![p.clone()]))
            .collect(),
    }
}

/// Runs one unit in this process. A panic in an engine is caught and
/// reported as that unit's failure; temporary files are removed as the
/// stack unwinds.
pub fn execute(
    unit: &Unit,
    caps: &Capabilities,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Outcome {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute_inner(unit, caps, cancel, progress, started)
    }));
    result.unwrap_or_else(|panic| {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unexpected failure".into());
        Outcome::from_error(unit, &Error::Internal(msg), started)
    })
}

fn execute_inner(
    unit: &Unit,
    caps: &Capabilities,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
    started: Instant,
) -> Outcome {
    let mut ctx = Context {
        caps,
        cancel,
        progress,
        output: &unit.output,
        temp_tag: &unit.temp_tag,
    };
    let result = match (&unit.action, unit.inputs.as_slice()) {
        (Action::CombinePdf, inputs) => pipeline::combine_pdf(inputs, &mut ctx),
        (Action::CombineText, inputs) => pipeline::combine_text(inputs, &mut ctx),
        (Action::CombineWorkbook, inputs) => pipeline::combine_workbook(inputs, &mut ctx),
        (Action::ZipAll, inputs) => pipeline::zip_all(inputs, None, &mut ctx),
        (Action::ZipLocked { password }, inputs) => {
            pipeline::zip_all(inputs, Some(password.expose()), &mut ctx)
        }
        (Action::UnzipLocked { password }, [input]) => {
            pipeline::unzip_with(input, Some(password.expose()), &mut ctx)
        }
        (Action::MergeFolders, inputs) => pipeline::merge_folders(inputs, &mut ctx),
        (Action::FindDuplicates, inputs) => pipeline::find_duplicates(inputs, &mut ctx),
        (Action::JoinMedia, inputs) => assemble::join_media(inputs, &mut ctx),
        (Action::BurnSubtitles, inputs) => assemble::burn_subtitles(inputs, &mut ctx),
        (Action::ScanDocuments, inputs) => assemble::scan_documents(inputs, &mut ctx),
        (Action::WordCount, [input]) => pipeline::word_count(input, &mut ctx),
        (Action::ReadCodes, [input]) => pipeline::read_codes(input, &mut ctx),
        (Action::MakeQr, [input]) => pipeline::make_qr(input, &mut ctx),
        (Action::IconSet { set }, [input]) => pipeline::icon_set(input, *set, &mut ctx),
        (Action::Flatten, [input]) => pipeline::flatten(input, &mut ctx),
        (Action::SortByKind, [input]) => pipeline::sort_by_kind(input, &mut ctx),
        (Action::ListContents { format }, [input]) => {
            pipeline::list_contents(input, *format, &mut ctx)
        }
        (Action::ConvertInside { steps, only, label }, [input]) => {
            pipeline::convert_inside(input, steps, *only, label.as_deref(), &mut ctx)
        }
        (Action::Steps { steps, label }, [input]) => {
            pipeline::run_steps(input, steps, label.as_deref(), &mut ctx)
        }
        (Action::Checksum { algorithm }, [input]) => {
            pipeline::checksum(input, *algorithm, &mut ctx)
        }
        (Action::CheckSyntax, [input]) => pipeline::check_syntax(input, &mut ctx),
        (Action::DataUri, [input]) => pipeline::data_uri(input, &mut ctx),
        (Action::Base64, [input]) => pipeline::base64(input, &mut ctx),
        (Action::CopyText, [input]) => pipeline::copy_text(input, &mut ctx),
        (Action::ExtractFrames { count, format }, [input]) => {
            pipeline::extract_frames(input, *count, *format, &mut ctx)
        }
        (Action::ProtectPdf { password }, [input]) => {
            pipeline::pdf_password(input, password.expose(), true, &mut ctx)
        }
        (Action::UnlockPdf { password }, [input]) => {
            pipeline::pdf_password(input, password.expose(), false, &mut ctx)
        }
        (Action::SplitPdf { mode }, [input]) => pipeline::split_pdf(input, mode, &mut ctx),
        (Action::RunShortcut { name }, [input]) => pipeline::run_shortcut(input, name, &mut ctx),
        (Action::Describe, [input]) => crate::content::describe(input, &mut ctx),
        (Action::Summarize, [input]) => crate::content::summarize(input, &mut ctx),
        _ => Err(Error::Invalid(
            "this action takes exactly one file per unit".into(),
        )),
    };
    match result {
        Ok(p) => Outcome {
            index: unit.index,
            inputs: unit.inputs.clone(),
            status: Status::Done,
            bytes_out: (!p.outputs.is_empty())
                .then(|| p.outputs.iter().map(|o| naming::size_of(o)).sum()),
            output: p.output,
            outputs: p.outputs,
            value: p.value,
            error: None,
            bytes_in: input_bytes(unit),
            millis: started.elapsed().as_millis() as u64,
        },
        Err(e) => Outcome::from_error(unit, &e, started),
    }
}

/// How units are run.
#[derive(Debug, Clone)]
pub enum Executor {
    /// In this process. Fastest; a crash takes the whole batch down.
    Inline,
    /// One child process per unit: `program args...`, speaking the worker
    /// protocol on stdin/stdout (see [`worker_main`]).
    Subprocess { program: PathBuf, args: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    ItemStarted { index: usize, inputs: Vec<PathBuf> },
    ItemProgress { index: usize, fraction: f32 },
    ItemFinished { outcome: Outcome },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: usize,
}

/// Text recognition can take a while on long scanned PDFs.
fn involves_vision(unit: &Unit) -> bool {
    match &unit.action {
        Action::CopyText
        | Action::WordCount
        | Action::ScanDocuments
        | Action::Describe
        | Action::Summarize => true,
        Action::Steps { steps, .. } => steps.iter().any(|s| {
            matches!(
                s,
                Step::MakeSearchable
                    | Step::Transcribe { .. }
                    | Step::RemoveBackground { .. }
                    | Step::BlurFaces { .. }
                    | Step::Translate { .. }
                    | Step::RedactText { .. }
                    | Step::Convert {
                        to: crate::format::Format::Txt | crate::format::Format::Md,
                        ..
                    }
            )
        }),
        _ => false,
    }
}

/// Folders and archives can be any size.
fn involves_folders(unit: &Unit) -> bool {
    match &unit.action {
        Action::ZipAll
        | Action::ZipLocked { .. }
        | Action::UnzipLocked { .. }
        | Action::MergeFolders
        | Action::Flatten
        | Action::SortByKind
        | Action::ListContents { .. }
        | Action::FindDuplicates
        | Action::ConvertInside { .. } => true,
        Action::Steps { steps, .. } => steps.iter().any(|s| {
            matches!(
                s,
                Step::Convert {
                    to: crate::format::Format::Zip | crate::format::Format::Folder,
                    ..
                }
            )
        }),
        _ => false,
    }
}

/// Keynote, Pages and Numbers export one document at a time.
fn involves_apps(unit: &Unit) -> bool {
    unit.inputs
        .iter()
        .any(|p| format::detect(p).is_ok_and(|f| f.is_iwork() || f == crate::format::Format::Pptx))
}

fn involves_media(unit: &Unit) -> bool {
    let media_step = |s: &Step| match s {
        Step::Convert { to, .. } => matches!(to.category(), Category::Audio | Category::Video),
        _ => false,
    };
    if let Action::Steps { steps, .. } = &unit.action {
        if steps.iter().any(media_step) {
            return true;
        }
    }
    unit.inputs
        .iter()
        .filter_map(|p| format::detect(p).ok())
        .any(|f| matches!(f.category(), Category::Audio | Category::Video))
}

/// Runs units, at most `options.parallelism` at once, reporting events.
pub fn run_batch(
    units: Vec<Unit>,
    executor: &Executor,
    options: &RunOptions,
    caps: &Capabilities,
    cancel: &AtomicBool,
    on_event: &(dyn Fn(Event) + Sync),
) -> Summary {
    // FFmpeg and Vision are heavy and multi-threaded themselves, and a
    // folder's contents are converted several files at a time already.
    let any_heavy = units.iter().any(|u| {
        involves_media(u)
            || involves_vision(u)
            || involves_apps(u)
            || matches!(u.action, Action::ConvertInside { .. })
    });
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    let default_parallel = if any_heavy { 2 } else { cpus.min(8) };
    let parallel = options
        .parallelism
        .unwrap_or(default_parallel)
        .clamp(1, 32)
        .min(units.len().max(1));
    let queue = Mutex::new(units.into_iter().collect::<VecDeque<_>>());
    let summary = Mutex::new(Summary::default());

    std::thread::scope(|scope| {
        for _ in 0..parallel {
            scope.spawn(|| loop {
                let Some(unit) = queue.lock().expect("queue lock").pop_front() else {
                    break;
                };
                let outcome = if cancel.load(Ordering::Relaxed) {
                    Outcome::from_error(&unit, &Error::Cancelled, Instant::now())
                } else {
                    on_event(Event::ItemStarted {
                        index: unit.index,
                        inputs: unit.inputs.clone(),
                    });
                    let index = unit.index;
                    let mut progress =
                        |fraction: f32| on_event(Event::ItemProgress { index, fraction });
                    let timeout = Duration::from_secs(options.timeout_secs.unwrap_or(
                        if involves_media(&unit) || involves_folders(&unit) {
                            4 * 3600
                        } else if involves_vision(&unit)
                            || involves_apps(&unit)
                            || matches!(unit.action, Action::RunShortcut { .. })
                        {
                            3600
                        } else {
                            600
                        },
                    ));
                    match executor {
                        Executor::Inline => execute(&unit, caps, cancel, &mut progress),
                        Executor::Subprocess { program, args } => execute_isolated(
                            &unit,
                            program,
                            args,
                            timeout,
                            caps,
                            cancel,
                            &mut progress,
                        ),
                    }
                };
                {
                    let mut s = summary.lock().expect("summary lock");
                    match outcome.status {
                        Status::Done => s.done += 1,
                        Status::Skipped => s.skipped += 1,
                        Status::Failed => s.failed += 1,
                        Status::Cancelled => s.cancelled += 1,
                    }
                }
                on_event(Event::ItemFinished { outcome });
            });
        }
    });
    summary.into_inner().expect("summary lock")
}

/// Messages a worker writes to stdout, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    Progress { fraction: f32 },
    Outcome { outcome: Outcome },
}

#[allow(clippy::too_many_arguments)]
fn execute_isolated(
    unit: &Unit,
    program: &PathBuf,
    args: &[String],
    timeout: Duration,
    caps: &Capabilities,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Outcome {
    // A unit that can't be described as JSON (a file name that isn't valid
    // UTF-8, possible on Linux) runs in this process instead.
    let Ok(payload) = serde_json::to_vec(unit) else {
        return execute(unit, caps, cancel, progress);
    };
    let started = Instant::now();
    let fail = |e: Error| Outcome::from_error(unit, &e, started);
    let cleanup = || {
        let mut dirs: Vec<PathBuf> = unit.inputs.iter().map(|p| unit.output.dir_for(p)).collect();
        dirs.dedup();
        for d in dirs {
            naming::remove_leftovers(&d, &unit.temp_tag);
        }
    };

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so killing the worker also kills anything it
    // started (FFmpeg), and nothing keeps our pipes open afterwards.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(Error::WorkerCrashed(format!("couldn't start worker: {e}"))),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&payload);
    }
    let (tx, rx) = mpsc::channel::<WorkerMessage>();
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let Some(out) = stdout else { return };
        for line in BufReader::new(out)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Ok(msg) = serde_json::from_str::<WorkerMessage>(&line) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        }
    });
    let stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let mut buf = Vec::new();
            let _ = e.read_to_end(&mut buf);
            let tail = &buf[buf.len().saturating_sub(4096)..];
            s = String::from_utf8_lossy(tail).into_owned();
        }
        s
    });

    let deadline = started + timeout;
    let mut outcome: Option<Outcome> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(WorkerMessage::Progress { fraction }) => progress(fraction),
            Ok(WorkerMessage::Outcome { outcome: o }) => outcome = Some(o),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let stop = if cancel.load(Ordering::Relaxed) {
            Some(Error::Cancelled)
        } else if Instant::now() >= deadline {
            Some(Error::Timeout(timeout.as_secs()))
        } else {
            None
        };
        if let Some(e) = stop {
            kill_tree(&mut child);
            // The reader thread ends by itself once the pipe closes.
            drop(reader);
            // A result that arrived just in time is real: its file exists.
            if let Some(mut o) = outcome.take() {
                o.millis = started.elapsed().as_millis() as u64;
                return o;
            }
            cleanup();
            return fail(e);
        }
    }
    if outcome.is_none() {
        // The worker died without reporting. Stop anything it started
        // (FFmpeg) before reaping it, while its process group still exists.
        kill_group(&child);
    }
    let status = child.wait();
    let _ = reader.join();
    let stderr_tail = err_reader.join().unwrap_or_default();
    match outcome {
        Some(mut o) => {
            o.millis = started.elapsed().as_millis() as u64;
            o
        }
        None => {
            cleanup();
            let how = match status {
                Ok(s) => describe_exit(s),
                Err(e) => e.to_string(),
            };
            let detail = stderr_tail
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            let msg = if detail.is_empty() {
                how
            } else {
                format!("{how}: {detail}")
            };
            fail(Error::WorkerCrashed(msg))
        }
    }
}

/// Signals every process in a worker's group. Only call this before the
/// worker has been reaped, so the group id can't have been reused.
fn kill_group(child: &std::process::Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: plain syscall; a negative pid addresses the worker's
            // own process group, created with `process_group(0)`.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

/// Kills a worker and every process in its group, then reaps it.
fn kill_tree(child: &mut std::process::Child) {
    kill_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// In a worker: if the supervisor dies, delete this unit's partial output
/// and exit, taking FFmpeg with us, so a crashed app or a killed CLI never
/// leaves conversions running or half-written files behind.
fn watch_parent(unit: &Unit) {
    #[cfg(unix)]
    {
        let mut dirs: Vec<PathBuf> = unit.inputs.iter().map(|p| unit.output.dir_for(p)).collect();
        dirs.dedup();
        let tag = unit.temp_tag.clone();
        // SAFETY: getppid and getpgrp have no preconditions.
        let parent = unsafe { libc::getppid() };
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            if unsafe { libc::getppid() } != parent {
                for d in &dirs {
                    naming::remove_leftovers(d, &tag);
                }
                // SAFETY: we only signal our own process group.
                unsafe {
                    libc::kill(-libc::getpgrp(), libc::SIGKILL);
                }
                std::process::exit(3);
            }
        });
    }
    #[cfg(not(unix))]
    let _ = unit;
}

fn describe_exit(s: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return format!("stopped by signal {sig}");
        }
    }
    match s.code() {
        Some(c) => format!("exited with code {c}"),
        None => "exited unexpectedly".into(),
    }
}

/// Entry point for a worker process: reads one [`Unit`] as JSON from
/// `input`, runs it, and writes progress and the outcome to `output`.
/// Returns the process exit code.
pub fn worker_main(input: &mut dyn Read, output: &mut dyn Write, caps: &Capabilities) -> i32 {
    let mut buf = Vec::new();
    if input.read_to_end(&mut buf).is_err() {
        return 2;
    }
    let unit: Unit = match serde_json::from_slice(&buf) {
        Ok(u) => u,
        Err(e) => {
            let _ = writeln!(
                output,
                "{{\"type\":\"error\",\"message\":{:?}}}",
                e.to_string()
            );
            return 2;
        }
    };
    watch_parent(&unit);
    let cancel = AtomicBool::new(false);
    let out = Mutex::new(output);
    let mut last = -1i32;
    let mut progress = |f: f32| {
        // Throttle to whole percentages.
        let pct = (f * 100.0) as i32;
        if pct != last {
            last = pct;
            let msg = WorkerMessage::Progress { fraction: f };
            if let Ok(mut o) = out.lock() {
                if let Ok(line) = serde_json::to_string(&msg) {
                    let _ = writeln!(o, "{line}");
                    let _ = o.flush();
                }
            }
        }
    };
    let outcome = execute(&unit, caps, &cancel, &mut progress);
    let msg = WorkerMessage::Outcome { outcome };
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    match serde_json::to_string(&msg) {
        Ok(line) => {
            let _ = writeln!(o, "{line}");
            let _ = o.flush();
            0
        }
        Err(_) => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Format;

    #[test]
    fn plan_splits_per_file_except_combine() {
        let files = vec![PathBuf::from("/a.png"), PathBuf::from("/b.png")];
        let units = plan(
            &files,
            &Action::convert(Format::Jpeg),
            &OutputLocation::BesideSource,
            "t",
        );
        assert_eq!(units.len(), 2);
        assert_eq!(units[1].temp_tag, "tu1");
        let units = plan(
            &files,
            &Action::CombinePdf,
            &OutputLocation::BesideSource,
            "t",
        );
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].inputs.len(), 2);
    }

    #[test]
    fn inline_batch_reports_each_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..5 {
            let p = dir.path().join(format!("img{i}.png"));
            image::RgbImage::from_pixel(10, 10, image::Rgb([i * 40, 0, 0]))
                .save(&p)
                .unwrap();
            files.push(p);
        }
        let broken = dir.path().join("broken.png");
        std::fs::write(&broken, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        files.push(broken);
        let already = dir.path().join("already.jpg");
        image::RgbImage::new(4, 4).save(&already).unwrap();
        files.push(already);

        let units = plan(
            &files,
            &Action::convert(Format::Jpeg),
            &OutputLocation::BesideSource,
            &new_job_tag(),
        );
        let events = Mutex::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let summary = run_batch(
            units,
            &Executor::Inline,
            &RunOptions::default(),
            &Capabilities::none(),
            &cancel,
            &|e| events.lock().unwrap().push(e),
        );
        assert_eq!(
            summary,
            Summary {
                done: 5,
                skipped: 1,
                failed: 1,
                cancelled: 0
            }
        );
        let finished = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::ItemFinished { .. }))
            .count();
        assert_eq!(finished, 7);
    }

    #[test]
    fn panics_become_failures() {
        let unit = plan(
            &[PathBuf::from("/x.png")],
            &Action::checksum(),
            &OutputLocation::BesideSource,
            "p",
        )
        .remove(0);
        let cancel = AtomicBool::new(false);
        let mut boom = |_f: f32| panic!("engine bug");
        // Checksum of a missing file fails before progress; use a real one.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "x").unwrap();
        let unit = Unit {
            inputs: vec![f],
            ..unit
        };
        let o = execute(&unit, &Capabilities::none(), &cancel, &mut boom);
        assert_eq!(o.status, Status::Failed);
        assert_eq!(o.error.unwrap().code, "internal");
    }

    #[test]
    fn cancelled_batch_runs_nothing() {
        let units = plan(
            &[PathBuf::from("/nope.png")],
            &Action::checksum(),
            &OutputLocation::BesideSource,
            "c",
        );
        let cancel = AtomicBool::new(true);
        let s = run_batch(
            units,
            &Executor::Inline,
            &RunOptions::default(),
            &Capabilities::none(),
            &cancel,
            &|_| {},
        );
        assert_eq!(s.cancelled, 1);
    }

    #[cfg(unix)]
    fn sh_worker(script: &str) -> Executor {
        Executor::Subprocess {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        }
    }

    #[cfg(unix)]
    #[test]
    fn crashing_worker_fails_only_its_file_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image::RgbImage::new(2, 2).save(&p).unwrap();
        let units = plan(
            &[p],
            &Action::convert(Format::Jpeg),
            &OutputLocation::BesideSource,
            "crash",
        );
        // Simulate a worker that starts writing, then dies from a segfault.
        let leftover = dir.path().join(".radlet-crashu0-partial.jpg");
        let script = format!(
            "cat >/dev/null; touch '{}'; kill -SEGV $$",
            leftover.display()
        );
        let events = Mutex::new(Vec::new());
        let s = run_batch(
            units,
            &sh_worker(&script),
            &RunOptions::default(),
            &Capabilities::none(),
            &AtomicBool::new(false),
            &|e| events.lock().unwrap().push(e),
        );
        assert_eq!(s.failed, 1);
        let msgs: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::ItemFinished { outcome } => outcome
                    .error
                    .as_ref()
                    .map(|e| e.code.clone() + ": " + &e.message),
                _ => None,
            })
            .collect();
        assert!(
            msgs.iter()
                .all(|m| m.starts_with("worker_crashed") && m.contains("signal 11")),
            "{msgs:?}"
        );
        assert!(!leftover.exists(), "partial output removed");
    }

    #[cfg(unix)]
    #[test]
    fn hung_worker_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "x").unwrap();
        let units = plan(
            &[p],
            &Action::checksum(),
            &OutputLocation::BesideSource,
            "hang",
        );
        let opts = RunOptions {
            timeout_secs: Some(1),
            ..Default::default()
        };
        let started = Instant::now();
        // `sleep` runs as a grandchild holding our pipes open; the whole
        // process group must be killed for the batch to return.
        let s = run_batch(
            units,
            &sh_worker("cat >/dev/null; sleep 30; true"),
            &opts,
            &Capabilities::none(),
            &AtomicBool::new(false),
            &|_| {},
        );
        assert_eq!(s.failed, 1);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn worker_protocol_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, b"abc").unwrap();
        let unit = plan(
            &[p],
            &Action::checksum(),
            &OutputLocation::BesideSource,
            "w",
        )
        .remove(0);
        let input = serde_json::to_vec(&unit).unwrap();
        let mut out = Vec::new();
        let code = worker_main(&mut input.as_slice(), &mut out, &Capabilities::none());
        assert_eq!(code, 0);
        let last = String::from_utf8(out)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        let msg: WorkerMessage = serde_json::from_str(&last).unwrap();
        match msg {
            WorkerMessage::Outcome { outcome } => {
                assert_eq!(outcome.status, Status::Done);
                assert_eq!(
                    outcome.value.as_deref(),
                    Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
