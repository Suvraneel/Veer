//! End-to-end tests of the `radlet` binary: CLI commands, isolated workers
//! and the `serve` protocol the Mac app uses.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const RADLET: &str = env!("CARGO_BIN_EXE_radlet");

fn radlet(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let out = Command::new(RADLET)
        .args(args)
        .current_dir(cwd)
        .env("RADLET_LOADOUTS_DIR", cwd.join("no-recipes"))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    let p = dir.join(name);
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 77])
    })
    .save(&p)
    .unwrap();
    p
}

/// A JPEG carrying EXIF orientation 6 (rotate 90° clockwise) and a fake
/// GPS string, like a phone photo taken in portrait.
fn phone_jpeg(dir: &Path) -> PathBuf {
    let p = dir.join("IMG_0001.jpg");
    image::RgbImage::from_pixel(60, 40, image::Rgb([10, 120, 200]))
        .save(&p)
        .unwrap();
    let mut tiff: Vec<u8> = b"MM\0\x2A\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&[0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]);
    tiff.extend_from_slice(b"GPS 51.5007N 0.1246W");
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    let mut data = std::fs::read(&p).unwrap();
    data.splice(2..2, seg);
    std::fs::write(&p, data).unwrap();
    p
}

fn has_ffmpeg() -> bool {
    let out = Command::new(RADLET)
        .args(["--json", "formats"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    !v["capabilities"]["ffmpeg"].is_null()
}

#[test]
fn convert_batch_reports_each_file_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    png(dir.path(), "a.png", 20, 20);
    png(dir.path(), "b.png", 20, 20);
    std::fs::write(dir.path().join("bad.png"), b"\x89PNG\r\n\x1a\ngarbage").unwrap();
    let (code, out, err) = radlet(
        &["convert", "a.png", "b.png", "bad.png", "--to", "webp"],
        dir.path(),
    );
    assert_eq!(code, 1, "one failure → exit 1\n{out}\n{err}");
    assert!(out.contains("✓ a.png → a.webp"));
    assert!(out.contains("✓ b.png → b.webp"));
    assert!(out.contains("✗ bad.png"));
    assert!(err.contains("2 done, 0 skipped, 1 failed"));
    assert!(dir.path().join("a.webp").exists() && dir.path().join("b.webp").exists());
}

#[test]
fn phone_photo_is_upright_and_strip_removes_gps_only() {
    let dir = tempfile::tempdir().unwrap();
    let jpg = phone_jpeg(dir.path());

    // Converting applies the orientation to the pixels.
    let (code, _, err) = radlet(&["convert", "IMG_0001.jpg", "--to", "png"], dir.path());
    assert_eq!(code, 0, "{err}");
    let png = image::open(dir.path().join("IMG_0001.png")).unwrap();
    assert_eq!(
        (png.width(), png.height()),
        (40, 60),
        "portrait after applying EXIF orientation"
    );

    // Stripping is lossless, drops GPS, and keeps the orientation tag.
    let (code, out, err) = radlet(&["strip", "IMG_0001.jpg"], dir.path());
    assert_eq!(code, 0, "{out}{err}");
    let clean = dir.path().join("IMG_0001 (clean).jpg");
    let bytes = std::fs::read(&clean).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("GPS 51.5007N"));
    let original = std::fs::read(&jpg).unwrap();
    assert!(bytes.len() < original.len());
    use image::ImageDecoder;
    let mut reader = image::ImageReader::open(&clean).unwrap();
    reader.set_format(image::ImageFormat::Jpeg);
    let orientation = reader.into_decoder().unwrap().orientation().unwrap();
    assert_eq!(orientation.to_exif(), 6);
}

#[test]
fn combine_images_and_pdf_into_one() {
    let dir = tempfile::tempdir().unwrap();
    png(dir.path(), "p1.png", 30, 40);
    png(dir.path(), "p2.png", 40, 30);
    let (code, _, err) = radlet(&["combine", "p1.png", "p2.png"], dir.path());
    assert_eq!(code, 0, "{err}");
    let first = dir.path().join("p1 (combined).pdf");
    let (code, _, err) = radlet(
        &[
            "combine",
            "p1 (combined).pdf",
            "p2.png",
            "p1 (combined).pdf",
        ],
        dir.path(),
    );
    assert_eq!(code, 0, "{err}");
    let doc = lopdf::Document::load(dir.path().join("p1 (combined) (combined).pdf")).unwrap();
    assert_eq!(doc.get_pages().len(), 5);
    assert!(first.exists());
}

#[test]
fn loadout_from_file_and_builtins() {
    let dir = tempfile::tempdir().unwrap();
    png(dir.path(), "big.png", 400, 200);
    std::fs::write(
        dir.path().join("thumb.toml"),
        "name = \"Thumbnail\"\n[[steps]]\nop = \"resize\"\nmax = 100\n[[steps]]\nop = \"convert\"\nto = \"jpeg\"\nquality = 70\n",
    )
    .unwrap();
    let (code, _, err) = radlet(&["loadout", "thumb.toml", "big.png"], dir.path());
    assert_eq!(code, 0, "{err}");
    let t = image::open(dir.path().join("big.jpg")).unwrap();
    assert_eq!((t.width(), t.height()), (100, 50));

    // "recipe" and "recipes" still work, from before loadouts were renamed.
    let (code, _, err) = radlet(&["recipe", "web-ready-image", "big.png"], dir.path());
    assert_eq!(code, 0, "{err}");
    assert!(dir.path().join("big.webp").exists());
    let (_, out, _) = radlet(&["loadouts"], dir.path());
    assert!(out.contains("Web-ready image") && out.contains("Email-friendly photo"));
    assert!(out.contains("Your loadouts folder"), "{out}");
    let (_, old, _) = radlet(&["recipes"], dir.path());
    assert_eq!(old, out);
}

fn three_page_pdf(dir: &Path) -> PathBuf {
    for i in 1..=3 {
        png(dir, &format!("pg{i}.png"), 40 * i, 30);
    }
    let (code, _, err) = radlet(&["combine", "pg1.png", "pg2.png", "pg3.png"], dir);
    assert_eq!(code, 0, "{err}");
    let out = dir.join("doc.pdf");
    std::fs::rename(dir.join("pg1 (combined).pdf"), &out).unwrap();
    out
}

#[test]
fn pdf_split_and_page_editing() {
    let dir = tempfile::tempdir().unwrap();
    three_page_pdf(dir.path());
    let (code, out, err) = radlet(&["split", "doc.pdf", "--ranges", "1-2, 3"], dir.path());
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        dir.path().join("doc (pages 1-2).pdf").exists()
            && dir.path().join("doc (page 3).pdf").exists()
    );
    assert!(out.contains("and 1 more"), "{out}");

    let (code, _, err) = radlet(
        &["pages", "doc.pdf", "--order", "3", "--order", "1"],
        dir.path(),
    );
    assert_eq!(code, 0, "{err}");
    let doc = lopdf::Document::load(dir.path().join("doc (edited).pdf")).unwrap();
    assert_eq!(doc.get_pages().len(), 2);

    let (code, out, _) = radlet(&["pages", "doc.pdf", "--order", "9"], dir.path());
    assert_eq!(code, 1);
    assert!(out.contains("page 9 doesn't exist"), "{out}");
}

#[test]
fn pdf_to_images_with_pdfium() {
    if std::env::var_os("RADLET_PDFIUM").is_none() {
        eprintln!("skipping: set RADLET_PDFIUM");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    three_page_pdf(dir.path());
    let (code, out, err) = radlet(&["convert", "doc.pdf", "--to", "png"], dir.path());
    assert_eq!(code, 0, "{out}{err}");
    for i in 1..=3 {
        assert!(
            dir.path().join(format!("doc - page {i}.png")).exists(),
            "{out}"
        );
    }
}

#[test]
fn crop_redact_resize_and_target() {
    let dir = tempfile::tempdir().unwrap();
    png(dir.path(), "shot.png", 400, 200);
    let (code, _, err) = radlet(&["crop", "shot.png", "--rect", "0.5,0,0.5,1"], dir.path());
    assert_eq!(code, 0, "{err}");
    let c = image::open(dir.path().join("shot (cropped).png")).unwrap();
    assert_eq!((c.width(), c.height()), (200, 200));

    let (code, _, err) = radlet(
        &[
            "redact",
            "shot.png",
            "--rect",
            "0,0,0.25,0.25",
            "--rect",
            "0.75,0.75,0.25,0.25",
            "--style",
            "pixelate",
        ],
        dir.path(),
    );
    assert_eq!(code, 0, "{err}");
    assert!(dir.path().join("shot (redacted).png").exists());

    let (code, _, err) = radlet(&["resize", "shot.png", "--width", "100"], dir.path());
    assert_eq!(code, 0, "{err}");
    let r = image::open(dir.path().join("shot (resized).png")).unwrap();
    assert_eq!((r.width(), r.height()), (100, 50));

    assert_eq!(
        radlet(&["crop", "shot.png", "--rect", "0.5,0,0.9,1"], dir.path()).0,
        2,
        "rect outside the image"
    );
    assert_eq!(
        radlet(&["resize", "shot.png"], dir.path()).0,
        2,
        "a size is required"
    );

    let noisy = dir.path().join("noisy.jpg");
    image::RgbImage::from_fn(1000, 800, |x, y| {
        let v = (x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) as u8;
        image::Rgb([v, v.wrapping_add(90), v.wrapping_mul(7)])
    })
    .save(&noisy)
    .unwrap();
    let (code, _, err) = radlet(&["compress", "noisy.jpg", "--target", "80KB"], dir.path());
    assert_eq!(code, 0, "{err}");
    assert!(
        std::fs::metadata(dir.path().join("noisy (compressed).jpg"))
            .unwrap()
            .len()
            <= 80_000
    );
}

#[test]
fn checksum_prints_sha256sum_format() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("abc.txt"), b"abc").unwrap();
    let (code, out, _) = radlet(&["checksum", "abc.txt"], dir.path());
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  abc.txt"
    );
}

#[test]
fn usage_errors_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    png(dir.path(), "a.png", 2, 2);
    assert_eq!(
        radlet(&["convert", "a.png", "--to", "ppt"], dir.path()).0,
        2
    );
    assert_eq!(
        radlet(&["convert", "missing.png", "--to", "jpg"], dir.path()).0,
        2
    );
    assert_eq!(
        radlet(&["rotate", "a.png", "--degrees", "45"], dir.path()).0,
        2
    );
}

#[test]
fn media_conversion_when_ffmpeg_present() {
    if !has_ffmpeg() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "1",
            "tone.wav",
        ])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    let (code, out, err) = radlet(&["convert", "tone.wav", "--to", "m4a"], dir.path());
    assert_eq!(code, 0, "{out}{err}");
    let head = std::fs::read(dir.path().join("tone.m4a")).unwrap();
    assert_eq!(&head[4..8], b"ftyp");
}

struct Helper {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<Value>,
}

impl Helper {
    fn start() -> Helper {
        Helper::start_with(&["serve"])
    }

    fn start_with(args: &[&str]) -> Helper {
        Helper::start_env(args, &[])
    }

    fn start_env(args: &[&str], env: &[(&str, &str)]) -> Helper {
        let mut child = Command::new(RADLET)
            .args(args)
            .envs(env.iter().copied())
            .env(
                "RADLET_LOADOUTS_DIR",
                std::env::temp_dir().join("radlet-no-recipes"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str::<Value>(&line).expect("helper writes JSON"));
            }
        });
        Helper {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn send_raw(&mut self, s: &str) {
        writeln!(self.stdin, "{s}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn next(&self) -> Value {
        self.lines
            .recv_timeout(Duration::from_secs(30))
            .expect("helper replied")
    }

    /// Reads until a message matching `pred`, returning everything seen.
    fn until(&self, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let v = self.next();
            let done = pred(&v);
            seen.push(v);
            if done {
                return seen;
            }
        }
    }
}

#[test]
fn serve_protocol_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let a = png(dir.path(), "one.png", 50, 50);
    let b = png(dir.path(), "two.png", 50, 50);
    let mut h = Helper::start();

    h.send(json!({"id": 1, "method": "hello"}));
    let hello = h.next();
    assert_eq!(hello["id"], 1);
    assert_eq!(hello["result"]["protocol"], 1);

    h.send(json!({"id": 2, "method": "inspect", "params": {"paths": [a, b]}}));
    let inspect = h.next();
    let wheel = &inspect["result"]["wheel"];
    assert_eq!(inspect["result"]["files"][0]["format"], "png");
    let jpeg_slot = wheel["convert"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "to_jpeg")
        .unwrap()
        .clone();
    assert!(wheel["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == "combine_pdf"));
    let crop = wheel["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "crop")
        .unwrap();
    assert_eq!(crop["window"], "crop");
    assert!(wheel["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["label"] == "Web-ready image"));

    // The app sends the slot's action back verbatim.
    h.send(json!({"id": 3, "method": "run", "params": {"paths": [a, b], "action": jpeg_slot["action"]}}));
    let started = h.next();
    assert_eq!(started["id"], 3);
    let job = started["result"]["job"].as_u64().unwrap();
    assert_eq!(started["result"]["units"], 2);
    let events = h.until(|v| v["event"] == "job_finished");
    let finished: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "item_finished")
        .collect();
    assert_eq!(finished.len(), 2);
    assert!(finished
        .iter()
        .all(|e| e["job"] == job && e["outcome"]["status"] == "done"));
    assert_eq!(events.last().unwrap()["summary"]["done"], 2);
    assert!(dir.path().join("one.jpg").exists() && dir.path().join("two.jpg").exists());

    // Recipes saved from the app: never overwritten, listed with their file.
    let recipes = dir.path().join("recipes");
    h.send(json!({"id": 10, "method": "save_recipe", "params": {"recipes_dir": recipes, "recipe": {"name": "Half size", "steps": [{"op": "resize", "percent": 50}]}}}));
    let saved = h.next();
    let path = saved["result"]["path"].as_str().unwrap().to_string();
    assert!(path.ends_with("half-size.toml"), "{saved}");
    h.send(json!({"id": 11, "method": "recipes", "params": {"recipes_dir": recipes}}));
    let listed = h.next();
    let list = listed["result"]["recipes"].as_array().unwrap();
    assert!(list.iter().any(|r| r["builtin"] == true));
    let mine = list.iter().find(|r| r["name"] == "Half size").unwrap();
    assert_eq!(mine["path"], path);
    assert_eq!(mine["steps"][0]["percent"], 50);
    h.send(json!({"id": 12, "method": "save_recipe", "params": {"recipes_dir": recipes, "recipe": {"name": "Bad", "steps": []}}}));
    assert_eq!(h.next()["error"]["code"], "invalid");

    // Sharing: a loadout becomes text, and shared text is checked before
    // anything is saved.
    h.send(json!({"id": 13, "method": "recipe_toml", "params": {"recipe": {"name": "Half size", "steps": [{"op": "resize", "percent": 50}]}}}));
    let text = h.next()["result"]["text"].as_str().unwrap().to_string();
    assert!(text.contains("name = \"Half size\""), "{text}");
    h.send(json!({"id": 14, "method": "parse_recipe", "params": {"text": text}}));
    let parsed = h.next();
    assert_eq!(
        parsed["result"]["recipe"]["steps"][0]["percent"], 50,
        "{parsed}"
    );
    h.send(json!({"id": 15, "method": "parse_recipe", "params": {"text": "name = \"x\"\n[[steps]]\nop = \"melt\"\n"}}));
    assert_eq!(h.next()["error"]["code"], "invalid");
    h.send(json!({"id": 16, "method": "recipe_toml", "params": {"recipe": {"name": " ", "steps": [{"op": "strip_metadata"}]}}}));
    assert_eq!(h.next()["error"]["code"], "invalid");

    // Errors are answered, not fatal.
    h.send_raw("{not json");
    assert_eq!(h.next()["error"]["code"], "bad_request");
    h.send(json!({"id": 4, "method": "teleport"}));
    assert_eq!(h.next()["error"]["code"], "unknown_method");
    h.send(json!({"id": 5, "method": "run", "params": {"paths": [a], "action": {"kind": "steps", "steps": [{"op": "melt"}]}}}));
    assert_eq!(h.next()["error"]["code"], "bad_params");
    h.send(json!({"id": 6, "method": "cancel", "params": {"job": 999}}));
    assert_eq!(h.next()["result"]["cancelled"], false);

    h.send(json!({"id": 7, "method": "shutdown"}));
    assert_eq!(h.next()["id"], 7);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = h.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "helper exits after shutdown");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn serve_offers_what_needs_installing() {
    let dir = tempfile::tempdir().unwrap();
    let mkv = dir.path().join("film.mkv");
    std::fs::write(&mkv, [0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]).unwrap();
    let mut h = Helper::start_env(
        &["serve"],
        &[("RADLET_NO_FFMPEG", "1"), ("RADLET_NO_VISION", "1")],
    );
    let convert = |v: &Value| v["result"]["wheel"]["convert"].as_array().unwrap().clone();

    // Only when asked: the wheel then lists what FFmpeg would add.
    h.send(json!({"id": 1, "method": "inspect", "params": {"paths": [mkv]}}));
    assert!(convert(&h.next()).is_empty());
    h.send(json!({"id": 2, "method": "inspect", "params": {"paths": [mkv], "installs": true}}));
    let slots = convert(&h.next());
    let mp4 = slots
        .iter()
        .find(|s| s["id"] == "to_mp4")
        .expect("MP4 offered");
    assert_eq!(mp4["needs"], "ffmpeg");

    // Running it anyway fails saying what it needs.
    h.send(json!({"id": 3, "method": "run", "params": {"paths": [mkv], "action": mp4["action"]}}));
    let events = h.until(|v| v["event"] == "job_finished");
    let failed = events
        .iter()
        .find(|e| e["event"] == "item_finished")
        .unwrap();
    assert_eq!(failed["outcome"]["status"], "failed");
    assert_eq!(failed["outcome"]["error"]["need"], "ffmpeg");

    // After an install, the helper looks again.
    h.send(json!({"id": 4, "method": "refresh"}));
    let refreshed = h.until(|v| v["id"] == 4);
    let found = &refreshed.last().unwrap()["result"];
    assert_eq!(found["ffmpeg"], false);
    for app in ["keynote", "pages", "numbers"] {
        assert!(found[app].is_boolean(), "{app}");
    }
    h.send(json!({"id": 5, "method": "shutdown"}));
    let _ = h.until(|v| v["id"] == 5);
}

#[test]
fn serve_exits_when_app_goes_away() {
    let mut h = Helper::start();
    h.send(json!({"id": 1, "method": "hello"}));
    h.next();
    drop(h.stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "helper exits on stdin EOF");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn radlet_env(args: &[&str], cwd: &Path, env: &[(&str, &Path)]) -> (i32, String, String) {
    let mut cmd = Command::new(RADLET);
    cmd.args(args)
        .current_dir(cwd)
        .env("RADLET_LOADOUTS_DIR", cwd.join("no-recipes"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `radlet text`, `radlet cutout` and the helper discovery, with a shell
/// script standing in for the Swift Vision helper.
#[cfg(unix)]
#[test]
fn vision_commands_with_a_stand_in_helper() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    png(d, "a.png", 40, 20);
    png(d, "b.png", 40, 20);
    let mask = d.join("mask.png");
    image::GrayImage::from_fn(40, 20, |x, _| image::Luma([if x < 10 { 255 } else { 0 }]))
        .save(&mask)
        .unwrap();
    let helper = d.join("radlet-vision");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\nname=$(basename \"$2\")\ncase \"$1\" in\n  text) printf 'Hello from %s\\n' \"$name\" > \"$3\";;\n  mask) cp '{}' \"$3\";;\n  *) exit 2;;\nesac\n",
            mask.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = [("RADLET_VISION", helper.as_path())];

    let (code, out, _) = radlet_env(&["text", "a.png"], d, &env);
    assert_eq!(code, 0);
    assert_eq!(out, "Hello from a.png\n");
    let (_, out, _) = radlet_env(&["text", "a.png", "b.png"], d, &env);
    assert!(out.contains("==> a.png <==\nHello from a.png"), "{out}");
    assert!(out.contains("==> b.png <==\nHello from b.png"), "{out}");

    let (code, _, err) = radlet_env(&["cutout", "a.png", "--crop"], d, &env);
    assert_eq!(code, 0, "{err}");
    let cut = image::open(d.join("a (cutout).png")).unwrap();
    assert_eq!(cut.width(), 10);

    // Without the helper the tools explain what they need.
    let (code, out, err) = radlet_env(
        &["text", "a.png"],
        d,
        &[("RADLET_VISION", d.join("missing").as_path())],
    );
    assert_eq!(code, 1);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("need the Radlet app"), "{err}");
}

#[test]
fn pdf_extras_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    three_page_pdf(d);
    let ok = |args: &[&str]| {
        let (code, out, err) = radlet(args, d);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    };
    ok(&[
        "numbers",
        "doc.pdf",
        "--style",
        "page-of-total",
        "--position",
        "top-right",
    ]);
    let text = lopdf::Document::load(d.join("doc (numbered).pdf"))
        .unwrap()
        .extract_text(&[3])
        .unwrap();
    assert!(text.contains("Page 3 of 3"), "{text}");
    ok(&[
        "watermark",
        "doc.pdf",
        "--text",
        "CONFIDENTIAL",
        "--opacity",
        "0.3",
    ]);
    assert!(d.join("doc (watermarked).pdf").exists());
    ok(&["rotate", "doc.pdf", "--degrees", "180"]);
    assert!(d.join("doc (rotated).pdf").exists());
    ok(&[
        "pages", "doc.pdf", "--order", "2", "--order", "1", "--rotate", "2:90",
    ]);
    let edited = lopdf::Document::load(d.join("doc (edited).pdf")).unwrap();
    let first = edited.get_pages()[&1];
    assert_eq!(
        edited
            .get_dictionary(first)
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );

    ok(&["protect", "doc.pdf", "--password", "tiger"]);
    let locked = "doc (protected).pdf";
    let (code, out, _) = radlet(&["unlock", locked, "--password", "lion"], d);
    assert_eq!(code, 1);
    assert!(out.contains("isn't right"), "{out}");
    ok(&["unlock", locked, "--password", "tiger"]);
    let open = lopdf::Document::load(d.join("doc (protected) (unlocked).pdf")).unwrap();
    assert_eq!(open.get_pages().len(), 3);

    let (code, out, _) = radlet(&["watermark", "doc.pdf", "--text", "草稿"], d);
    assert_eq!(code, 1);
    assert!(out.contains("Latin letters"), "{out}");
}

#[test]
fn media_extras_from_the_command_line() {
    if !has_ffmpeg() {
        eprintln!("skipping: needs ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let status = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=25",
        ])
        .args([
            "-f", "lavfi", "-i", "sine", "-t", "4", "-c:v", "libx264", "-pix_fmt", "yuv420p",
        ])
        .arg(d.join("clip.mp4"))
        .status();
    if !status.is_ok_and(|s| s.success()) {
        eprintln!("skipping: ffmpeg can't make a test clip");
        return;
    }
    let ok = |args: &[&str]| {
        let (code, out, err) = radlet(args, d);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
    };
    ok(&["trim", "clip.mp4", "--start", "0:01", "--end", "0:02.5"]);
    assert!(d.join("clip (trimmed).mp4").exists());
    ok(&["gif", "clip.mp4", "--start", "1", "--end", "2"]);
    assert!(d.join("clip.gif").exists());
    ok(&["normalize", "clip.mp4"]);
    assert!(d.join("clip (normalized).mp4").exists());
    ok(&["frames", "clip.mp4", "--count", "2", "--format", "png"]);
    assert!(d.join("clip - still 1.png").exists() && d.join("clip - still 2.png").exists());
    let (code, _, err) = radlet(&["trim", "clip.mp4", "--start", "soon"], d);
    assert_eq!(code, 2, "{err}");
}

/// Reads MCP messages until the response to `id`, skipping notifications.
fn mcp_reply(h: &Helper, id: u64) -> (Value, Vec<Value>) {
    let mut notes = Vec::new();
    loop {
        let v = h.next();
        if v["id"] == id {
            return (v, notes);
        }
        notes.push(v);
    }
}

fn mcp_call(h: &mut Helper, id: u64, tool: &str, args: Value) -> Value {
    h.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                  "params": {"name": tool, "arguments": args, "_meta": {"progressToken": format!("p{id}")}}}));
    mcp_reply(h, id).0
}

#[test]
fn mcp_server_for_agents() {
    let dir = tempfile::tempdir().unwrap();
    let chart = png(dir.path(), "chart.png", 80, 60);
    let mut h = Helper::start_with(&["mcp"]);

    h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    let init = h.next();
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "radlet");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    h.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = h.next();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in [
        "inspect_files",
        "convert",
        "compress",
        "run_steps",
        "privacy_check",
        "list_loadouts",
        "run_loadout",
    ] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    assert!(!names
        .iter()
        .any(|n| n.contains("password") || n.contains("protect")));
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
    }

    let inspect = mcp_call(&mut h, 3, "inspect_files", json!({"paths": [chart]}));
    assert_eq!(inspect["result"]["isError"], false);
    let s = &inspect["result"]["structuredContent"];
    assert!(
        s["convert_to"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "webp"),
        "{s}"
    );
    assert!(
        s["steps"].as_array().unwrap().iter().any(|f| f == "resize"),
        "{s}"
    );

    let converted = mcp_call(
        &mut h,
        4,
        "convert",
        json!({"paths": [chart], "to": "webp"}),
    );
    assert_eq!(converted["result"]["isError"], false, "{converted}");
    assert!(converted["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("chart.webp"));
    assert!(dir.path().join("chart.webp").exists());

    let chained = mcp_call(
        &mut h,
        5,
        "run_steps",
        json!({"paths": [chart], "steps": [{"op": "resize", "max": 40}, {"op": "convert", "to": "jpeg"}]}),
    );
    assert_eq!(
        chained["result"]["structuredContent"]["summary"]["done"], 1,
        "{chained}"
    );
    assert_eq!(
        image::image_dimensions(dir.path().join("chart.jpg")).unwrap(),
        (40, 30)
    );

    // Mistakes come back as tool errors the agent can read and fix.
    let bad = mcp_call(
        &mut h,
        6,
        "run_steps",
        json!({"paths": [chart], "steps": [{"op": "melt"}]}),
    );
    assert_eq!(bad["result"]["isError"], true);
    assert!(bad["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("steps aren't right"));
    let missing = mcp_call(
        &mut h,
        7,
        "convert",
        json!({"paths": ["/nope/x.png"], "to": "jpeg"}),
    );
    assert_eq!(missing["result"]["isError"], true);

    let privacy = mcp_call(
        &mut h,
        8,
        "privacy_check",
        json!({"paths": [dir.path().join("chart.jpg")]}),
    );
    assert_eq!(privacy["result"]["isError"], false);
    assert_eq!(
        privacy["result"]["structuredContent"]["reports"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Loadouts were called recipes: the old tool name answers in the old shape.
    let loadouts = mcp_call(&mut h, 14, "list_loadouts", json!({}));
    let built_in = loadouts["result"]["structuredContent"]["loadouts"]
        .as_array()
        .unwrap()
        .len();
    assert!(built_in > 0, "{loadouts}");
    let recipes = mcp_call(&mut h, 15, "list_recipes", json!({}));
    assert_eq!(
        recipes["result"]["structuredContent"]["recipes"]
            .as_array()
            .unwrap()
            .len(),
        built_in
    );

    let unknown = mcp_call(&mut h, 9, "teleport", json!({}));
    assert_eq!(unknown["error"]["code"], -32602);
    h.send(json!({"jsonrpc": "2.0", "id": 10, "method": "resources/list"}));
    assert_eq!(h.next()["error"]["code"], -32601);
    h.send_raw(r#"[{"jsonrpc": "2.0", "id": 13, "method": "ping"}]"#);
    assert_eq!(h.next()["error"]["code"], -32600);
    h.send(json!({"jsonrpc": "2.0", "id": 11, "method": "ping"}));
    assert_eq!(h.next()["result"], json!({}));
    h.send(json!({"jsonrpc": "2.0", "id": 12, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}));
    assert_eq!(h.next()["result"]["protocolVersion"], "2025-11-25");

    // Closing stdin ends the server.
    drop(h.stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "mcp exits when the client goes away"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn privacy_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let photo = phone_jpeg(dir.path());
    let (code, out, err) = radlet(&["privacy", photo.to_str().unwrap()], dir.path());
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with(photo.to_str().unwrap()), "{out}");
    let (code, out, _) = radlet(
        &["--json", "privacy", photo.to_str().unwrap(), "missing.jpg"],
        dir.path(),
    );
    assert_eq!(code, 1);
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0]["findings"].is_array());
    assert!(lines[1]["error"]["message"].is_string());
}

#[test]
fn documents_and_folders_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.md"), "# Hi\n\nThree more words.\n").unwrap();
    let (code, out, _) = radlet(&["words", "notes.md"], dir.path());
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "4 words · 1 min read");
    assert_eq!(
        radlet(&["convert", "notes.md", "--to", "docx"], dir.path()).0,
        0
    );
    assert!(dir.path().join("notes.docx").is_file());

    std::fs::create_dir(dir.path().join("Box")).unwrap();
    std::fs::write(dir.path().join("Box/a.txt"), "a").unwrap();
    assert_eq!(radlet(&["zip", "Box", "notes.md"], dir.path()).0, 0);
    assert!(dir.path().join("Archive.zip").is_file());
    assert_eq!(radlet(&["unzip", "Archive.zip"], dir.path()).0, 0);
    assert!(dir.path().join("Archive/Box/a.txt").is_file());
    let (code, _, _) = radlet(&["list", "Box", "--csv"], dir.path());
    assert_eq!(code, 0);
    assert!(dir.path().join("Box (contents).csv").is_file());
}

#[test]
fn tables_codes_icons_and_folder_contents_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("a.csv"), "x,y\n1,2\n").unwrap();
    std::fs::write(d.join("b.tsv"), "z\n3\n").unwrap();
    assert_eq!(radlet(&["workbook", "a.csv", "b.tsv"], d).0, 0);
    assert!(d.join("a (combined).xlsx").is_file());
    assert_eq!(radlet(&["convert", "a.csv", "--to", "xlsx"], d).0, 0);
    let (code, out, _) = radlet(&["--json", "inspect", "a.xlsx"], d);
    assert_eq!(code, 0);
    assert!(
        out.contains("\"to_csv\"") && !out.contains("to_xlsx"),
        "{out}"
    );

    std::fs::write(d.join("link.txt"), "https://example.com/\n").unwrap();
    assert_eq!(radlet(&["qr", "link.txt"], d).0, 0);
    let (code, out, _) = radlet(&["codes", "link (QR code).png"], d);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "https://example.com/");

    png(d, "logo.png", 64, 32);
    assert_eq!(radlet(&["icons", "logo.png", "--set", "mac"], d).0, 0);
    assert!(d.join("logo.icns").is_file());
    assert_eq!(radlet(&["icons", "logo.png", "--set", "favicon"], d).0, 0);
    assert!(d.join("logo (favicons)/favicon.ico").is_file());

    std::fs::create_dir_all(d.join("Shots/old")).unwrap();
    png(&d.join("Shots/old"), "one.png", 8, 8);
    png(&d.join("Shots"), "two.png", 8, 8);
    let (code, _, err) = radlet(&["convert", "Shots", "--to", "webp"], d);
    assert_eq!(code, 0, "{err}");
    assert!(d.join("Shots (WebP)/old/one.webp").is_file());
    assert!(d.join("Shots (WebP)/two.webp").is_file());
    let (_, out, _) = radlet(&["--json", "inspect", "Shots"], d);
    assert!(out.contains("inside_jpeg"), "{out}");

    // Faces and translation need the Mac helper.
    assert_ne!(radlet(&["faces", "logo.png"], d).0, 0);
    assert_ne!(radlet(&["translate", "link.txt", "--to", "fr"], d).0, 0);
}

/// A stand-in for macOS's `shortcuts`: "Upper" returns the input in
/// capitals, "Quiet" returns nothing.
#[cfg(unix)]
fn fake_shortcuts(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("shortcuts");
    std::fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = list ]; then printf 'Upper\\nQuiet\\n'; exit 0; fi\n\
         case \"$2\" in Upper) tr a-z A-Z < \"$4\" > \"$6\" ;; Quiet) ;; *) echo \"Couldn't find shortcut\" >&2; exit 1 ;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn serve_runs_the_persons_shortcuts() {
    let dir = tempfile::tempdir().unwrap();
    let tool = fake_shortcuts(dir.path());
    let note = dir.path().join("note.txt");
    std::fs::write(&note, "hello").unwrap();
    let mut h = Helper::start_env(&["serve"], &[("RADLET_SHORTCUTS", tool.to_str().unwrap())]);

    h.send(json!({"id": 1, "method": "shortcuts"}));
    assert_eq!(h.next()["result"]["shortcuts"], json!(["Upper", "Quiet"]));

    // Only the ones chosen go on the Control wheel, after the loadouts.
    h.send(
        json!({"id": 2, "method": "inspect", "params": {"paths": [note], "shortcuts": ["Upper"]}}),
    );
    let wheel = h.next()["result"]["wheel"]["recipes"]
        .as_array()
        .unwrap()
        .clone();
    let slot = wheel
        .iter()
        .find(|s| s["id"] == "shortcut:Upper")
        .expect("the shortcut's slot");
    assert_eq!(slot["label"], "Upper");
    assert!(!wheel.iter().any(|s| s["id"] == "shortcut:Quiet"));
    h.send(json!({"id": 3, "method": "inspect", "params": {"paths": [dir.path()], "shortcuts": ["Upper"]}}));
    let folder = h.next()["result"]["wheel"]["recipes"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        !folder.iter().any(|s| s["id"] == "shortcut:Upper"),
        "shortcuts take files"
    );

    // Running it saves what it returns beside the file.
    h.send(
        json!({"id": 4, "method": "run", "params": {"paths": [note], "action": slot["action"]}}),
    );
    let events = h.until(|v| v["event"] == "job_finished");
    let done = events
        .iter()
        .find(|e| e["event"] == "item_finished")
        .unwrap();
    assert_eq!(done["outcome"]["status"], "done", "{done}");
    let out = dir.path().join("note (Upper).txt");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "HELLO");
    assert_eq!(
        std::fs::read_to_string(&note).unwrap(),
        "hello",
        "the original is untouched"
    );

    // One that returns nothing is done, with nothing saved; a missing one
    // says so.
    h.send(json!({"id": 5, "method": "run", "params": {"paths": [note], "action": {"kind": "run_shortcut", "name": "Quiet"}}}));
    let events = h.until(|v| v["event"] == "job_finished");
    let quiet = events
        .iter()
        .find(|e| e["event"] == "item_finished")
        .unwrap();
    assert_eq!(quiet["outcome"]["status"], "done", "{quiet}");
    h.send(json!({"id": 6, "method": "run", "params": {"paths": [note], "action": {"kind": "run_shortcut", "name": "Gone"}}}));
    let events = h.until(|v| v["event"] == "job_finished");
    let gone = events
        .iter()
        .find(|e| e["event"] == "item_finished")
        .unwrap();
    assert_eq!(gone["outcome"]["status"], "failed");
    assert!(
        gone["outcome"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no shortcut called"),
        "{gone}"
    );
    h.send(json!({"id": 7, "method": "shutdown"}));
    let _ = h.until(|v| v["id"] == 7);
}

#[test]
fn serve_names_files_by_their_content() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("untitled 3.md");
    std::fs::write(&doc, "# Trip to Lisbon\n\nDay one: the castle.\n").unwrap();
    let odd = dir.path().join("data.bin");
    std::fs::write(&odd, [0u8, 1, 2, 3]).unwrap();
    let mut h = Helper::start_env(&["serve"], &[("RADLET_NO_VISION", "1")]);
    h.send(json!({"id": 1, "method": "content_names", "params": {"paths": [doc, odd]}}));
    let names = h.next();
    assert_eq!(
        names["result"]["names"],
        json!(["Trip to Lisbon", null]),
        "{names}"
    );
    h.send(json!({"id": 2, "method": "shutdown"}));
    let _ = h.until(|v| v["id"] == 2);
}

#[test]
fn mcp_offers_the_wheels_tools() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut h = Helper::start_with(&["mcp"]);
    h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    h.next();
    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = h.next();
    let listed = tools["result"]["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = listed.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in [
        "combine_documents",
        "combine_into_workbook",
        "read_codes",
        "make_qr",
        "make_icons",
        "convert_folder",
        "merge_folders",
        "flatten_folders",
        "sort_by_kind",
        "list_folder_contents",
        "find_duplicates",
        "suggest_names",
    ] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    assert!(!names.iter().any(|n| n.contains("shortcut")));
    let run_steps = listed.iter().find(|t| t["name"] == "run_steps").unwrap();
    let help = run_steps["description"].as_str().unwrap();
    for op in [
        "blur_faces",
        "translate",
        "clean_text",
        "format_json",
        "trace",
    ] {
        assert!(help.contains(op), "{op} documented");
    }

    // A QR code from text, read back.
    let qr = mcp_call(
        &mut h,
        3,
        "make_qr",
        json!({"text": "https://radlet.vercel.app", "output_dir": d}),
    );
    assert_eq!(qr["result"]["isError"], false, "{qr}");
    let code = d.join("QR code.png");
    assert!(code.exists());
    let read = mcp_call(&mut h, 4, "read_codes", json!({"paths": [code]}));
    assert!(
        read["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("https://radlet.vercel.app"),
        "{read}"
    );
    let no_dir = mcp_call(&mut h, 5, "make_qr", json!({"text": "x"}));
    assert_eq!(no_dir["result"]["isError"], true);

    // Documents and tables, combined.
    std::fs::write(d.join("a.md"), "# Plan\n\nFirst part.\n").unwrap();
    std::fs::write(d.join("b.md"), "Second part.\n").unwrap();
    let joined = mcp_call(
        &mut h,
        6,
        "combine_documents",
        json!({"paths": [d.join("a.md"), d.join("b.md")]}),
    );
    assert_eq!(
        joined["result"]["structuredContent"]["summary"]["done"], 1,
        "{joined}"
    );
    std::fs::write(d.join("one.csv"), "a,b\n1,2\n").unwrap();
    std::fs::write(d.join("two.csv"), "c\n3\n").unwrap();
    let book = mcp_call(
        &mut h,
        7,
        "combine_into_workbook",
        json!({"paths": [d.join("one.csv"), d.join("two.csv")]}),
    );
    assert_eq!(
        book["result"]["structuredContent"]["summary"]["done"], 1,
        "{book}"
    );

    // A document's own title, without renaming anything.
    let names = mcp_call(
        &mut h,
        8,
        "suggest_names",
        json!({"paths": [d.join("a.md")]}),
    );
    assert_eq!(
        names["result"]["structuredContent"]["names"][0]["name"], "Plan",
        "{names}"
    );
    assert!(d.join("a.md").exists());

    // Folders.
    let album = d.join("Album");
    std::fs::create_dir_all(album.join("Day 2")).unwrap();
    png(&album, "one.png", 20, 10);
    png(&album.join("Day 2"), "two.png", 20, 10);
    std::fs::copy(album.join("one.png"), album.join("Day 2/copy.png")).unwrap();
    let converted = mcp_call(
        &mut h,
        9,
        "convert_folder",
        json!({"paths": [album], "kind": "image", "to": "jpeg"}),
    );
    assert_eq!(converted["result"]["isError"], false, "{converted}");
    assert!(d.join("Album (JPEG)/Day 2/two.jpg").exists(), "{converted}");
    let bad_kind = mcp_call(
        &mut h,
        10,
        "convert_folder",
        json!({"paths": [album], "kind": "pictures", "to": "jpeg"}),
    );
    assert_eq!(bad_kind["result"]["isError"], true);
    for (id, tool) in [
        (11, "flatten_folders"),
        (12, "sort_by_kind"),
        (13, "list_folder_contents"),
        (14, "find_duplicates"),
    ] {
        let r = mcp_call(&mut h, id, tool, json!({"paths": [album]}));
        assert_eq!(
            r["result"]["structuredContent"]["summary"]["done"], 1,
            "{tool}: {r}"
        );
    }
    let icons = mcp_call(
        &mut h,
        15,
        "make_icons",
        json!({"paths": [album.join("one.png")], "set": "favicon"}),
    );
    assert_eq!(icons["result"]["isError"], false, "{icons}");

    // Checking, data URIs, Base64 and other checksums.
    std::fs::write(d.join("data.json"), "{\"a\": 1}").unwrap();
    std::fs::write(d.join("conf.toml"), "a = \n").unwrap();
    let check = mcp_call(
        &mut h,
        17,
        "check_syntax",
        json!({"paths": [d.join("data.json"), d.join("conf.toml")]}),
    );
    let text = check["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("data.json:\nValid JSON") && text.contains("conf.toml: line 1, column 5"),
        "{check}"
    );
    let uri = mcp_call(&mut h, 18, "data_uri", json!({"paths": [code]}));
    assert!(
        uri["result"]["structuredContent"]["results"][0]["value"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"),
        "{uri}"
    );
    let b64 = mcp_call(
        &mut h,
        19,
        "base64",
        json!({"paths": [d.join("data.json")]}),
    );
    assert_eq!(b64["result"]["isError"], false, "{b64}");
    assert!(d.join("data.json.b64").exists());
    let sum = mcp_call(
        &mut h,
        20,
        "checksum",
        json!({"paths": [d.join("data.json")], "algorithm": "sha1"}),
    );
    assert_eq!(
        sum["result"]["structuredContent"]["results"][0]["value"]
            .as_str()
            .unwrap()
            .len(),
        40,
        "{sum}"
    );

    // The newly documented steps are suggested where they fit.
    std::fs::write(d.join("data.json"), "{\"a\": 1}").unwrap();
    let inspect = mcp_call(
        &mut h,
        16,
        "inspect_files",
        json!({"paths": [d.join("data.json")]}),
    );
    let steps = &inspect["result"]["structuredContent"]["steps"];
    assert!(
        steps.as_array().unwrap().iter().any(|s| s == "format_json"),
        "{steps}"
    );
}

#[test]
fn developer_tools_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("b.json"), r#"{"z":1,"a":[2,1]}"#).unwrap();
    std::fs::write(d.join("bad.json"), "{\"a\": 1,\n}").unwrap();
    std::fs::write(
        d.join("feed.xml"),
        "<rss><item><t>Hi <b>there</b></t></item></rss>",
    )
    .unwrap();
    std::fs::write(d.join("logo.svg"), "<?xml version=\"1.0\"?>\n<!-- made by hand -->\n<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 4 4\">\n  <rect width=\"4\" height=\"4\"/>\n</svg>\n").unwrap();

    assert_eq!(radlet(&["json", "b.json", "--sort-keys"], d).0, 0);
    assert_eq!(
        std::fs::read_to_string(d.join("b (sorted).json")).unwrap(),
        "{\n  \"a\": [\n    2,\n    1\n  ],\n  \"z\": 1\n}\n"
    );
    assert_eq!(radlet(&["xml", "feed.xml"], d).0, 0);
    assert_eq!(
        std::fs::read_to_string(d.join("feed (formatted).xml")).unwrap(),
        "<rss>\n  <item>\n    <t>Hi <b>there</b></t>\n  </item>\n</rss>\n"
    );
    assert_eq!(radlet(&["optimize-svg", "logo.svg"], d).0, 0);
    assert_eq!(
        std::fs::read_to_string(d.join("logo (optimized).svg")).unwrap(),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 4 4\"><rect width=\"4\" height=\"4\"/></svg>"
    );

    // A syntax check prints the verdict and fails on a problem, for scripts.
    let (code, out, err) = radlet(&["check", "b.json", "bad.json"], d);
    assert_eq!(code, 1);
    assert!(out.contains("b.json: Valid JSON"), "{out}");
    assert!(
        err.contains("bad.json: line 2, column 1: key must be a string"),
        "{err}"
    );
    assert_eq!(
        radlet(&["check", "logo.svg"], d),
        (0, "Well-formed SVG\n".into(), String::new())
    );

    let (code, uri, _) = radlet(&["data-uri", "logo.svg"], d);
    assert_eq!(code, 0);
    assert!(
        uri.starts_with("data:image/svg+xml;base64,PD94bWwg"),
        "{uri}"
    );
    assert_eq!(radlet(&["base64", "logo.svg"], d).0, 0);
    std::fs::rename(d.join("logo.svg.b64"), d.join("copy.svg.b64")).unwrap();
    assert_eq!(radlet(&["base64", "copy.svg.b64"], d).0, 0);
    assert_eq!(
        std::fs::read(d.join("copy.svg")).unwrap(),
        std::fs::read(d.join("logo.svg")).unwrap()
    );
    let (code, sum, _) = radlet(&["checksum", "--algorithm", "md5", "b.json"], d);
    assert_eq!(code, 0);
    assert!(sum.starts_with(&format!(
        "{:x}  ",
        md5_of(&std::fs::read(d.join("b.json")).unwrap())
    )));
}

/// MD5 by the book, so the test doesn't trust the code it checks.
fn md5_of(data: &[u8]) -> u128 {
    let s: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(s[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    u128::from_be_bytes(out)
}
