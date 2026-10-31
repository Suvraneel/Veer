//! Documents, drawings, data files and folders, end to end: planned and
//! run the way the app and the CLI run them.

#![allow(clippy::cloned_ref_to_slice_refs)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use radlet_core::format::{detect, Format};
use radlet_core::job::{self, Outcome, Status};
use radlet_core::ops::{Action, Step};
use radlet_core::{Capabilities, OutputLocation};

fn run(inputs: &[PathBuf], action: Action) -> Vec<Outcome> {
    let caps = Capabilities::none();
    let units = job::plan(inputs, &action, &OutputLocation::BesideSource, "t");
    let cancel = AtomicBool::new(false);
    units
        .iter()
        .map(|u| job::execute(u, &caps, &cancel, &mut |_| {}))
        .collect()
}

fn one(inputs: &[PathBuf], action: Action) -> Outcome {
    let mut all = run(inputs, action);
    assert_eq!(all.len(), 1);
    all.remove(0)
}

fn done(o: &Outcome) -> PathBuf {
    assert_eq!(o.status, Status::Done, "{:?}", o.error);
    o.output.clone().expect("an output")
}

fn convert(input: &Path, to: Format) -> PathBuf {
    done(&one(&[input.to_path_buf()], Action::convert(to)))
}

fn name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

const NOTES: &str = "# Trip notes\n\nPack **light** and read [the guide](https://example.com/guide).\n\n- Passport\n- Tickets\n  - Train\n\n1. Book\n2. Go\n\n| Day | City |\n| --- | --- |\n| 1 | Lyon |\n\n```\nlet x = 1;\n```\n";

#[test]
fn documents_convert_every_way() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("notes.md");
    std::fs::write(&md, NOTES).unwrap();

    let docx = convert(&md, Format::Docx);
    assert_eq!(name(&docx), "notes.docx");
    assert_eq!(detect(&docx).unwrap(), Format::Docx);
    let rtf = convert(&docx, Format::Rtf);
    assert_eq!(detect(&rtf).unwrap(), Format::Rtf);
    let html = convert(&rtf, Format::Html);
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("<h1>Trip notes</h1>"), "{page}");
    assert!(page.contains("<strong>light</strong>"));
    assert!(page.contains("<a href=\"https://example.com/guide\">the guide</a>"));
    assert!(page.contains("<title>notes</title>"));
    let back = convert(&html, Format::Md);
    let text = std::fs::read_to_string(&back).unwrap();
    assert!(
        text.starts_with(
            "# Trip notes\n\nPack **light** and read [the guide](https://example.com/guide)."
        ),
        "{text}"
    );
    assert!(
        text.contains("- Passport\n- Tickets\n    - Train\n\n1. Book\n2. Go"),
        "{text}"
    );
    assert!(text.contains("| 1 | Lyon |"), "{text}");
    assert!(text.contains("```\nlet x = 1;\n```"), "{text}");
    // Markdown already exists, so the copy gets a new name.
    assert_eq!(name(&back), "notes 2.md");

    let txt = convert(&docx, Format::Txt);
    let plain = std::fs::read_to_string(&txt).unwrap();
    assert!(plain.starts_with("Trip notes\n==========\n"), "{plain}");

    let pdf = convert(&md, Format::Pdf);
    let doc = lopdf::Document::load(&pdf).unwrap();
    let words = doc.extract_text(&[1]).unwrap();
    assert!(
        words.contains("Trip notes") && words.contains("Passport"),
        "{words}"
    );

    // PDFs come back as Markdown too.
    let from_pdf = convert(&pdf, Format::Md);
    assert!(std::fs::read_to_string(from_pdf)
        .unwrap()
        .contains("Trip notes"));
}

#[test]
fn counting_copying_and_combining() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "# One\n\nHello **big** world.\n").unwrap();
    std::fs::write(&b, "Second note\nwith two lines\n").unwrap();

    let count = one(&[a.clone()], Action::WordCount);
    assert_eq!(count.value.as_deref(), Some("4 words · 1 min read"));
    let docx = convert(&a, Format::Docx);
    let copied = one(&[docx], Action::CopyText);
    assert_eq!(copied.value.as_deref(), Some("One\n\nHello big world."));
    let raw = one(&[a.clone()], Action::CopyText);
    assert_eq!(raw.value.as_deref(), Some("# One\n\nHello **big** world."));

    let combined = done(&one(&[a.clone(), b.clone()], Action::CombineText));
    assert_eq!(name(&combined), "a (combined).md");
    let text = std::fs::read_to_string(&combined).unwrap();
    assert_eq!(
        text,
        "# One\n\nHello **big** world.\n\n# b\n\nSecond note\\\nwith two lines\n"
    );

    // Pictures, drawings and documents in one PDF.
    let png = dir.path().join("dot.png");
    image::RgbImage::from_pixel(20, 10, image::Rgb([200, 30, 30]))
        .save(&png)
        .unwrap();
    let svg = dir.path().join("box.svg");
    std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="blue"/></svg>"#).unwrap();
    let pdf = done(&one(&[a, png, svg], Action::CombinePdf));
    assert_eq!(lopdf::Document::load(&pdf).unwrap().get_pages().len(), 3);
}

#[test]
fn drawings_render_at_any_size() {
    let dir = tempfile::tempdir().unwrap();
    let svg = dir.path().join("icon.svg");
    std::fs::write(&svg, r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="24" height="12" viewBox="0 0 24 12"><circle cx="6" cy="6" r="6" fill="green"/></svg>"#).unwrap();
    assert_eq!(detect(&svg).unwrap(), Format::Svg);

    let png = convert(&svg, Format::Png);
    assert_eq!(image::image_dimensions(&png).unwrap(), (1024, 512));
    let twice = done(&one(
        &[svg.clone()],
        Action::Steps {
            steps: vec![
                Step::resize_percent(200),
                Step::Convert {
                    to: Format::Png,
                    quality: None,
                },
            ],
            label: None,
        },
    ));
    assert_eq!(image::image_dimensions(&twice).unwrap(), (48, 24));
    let resized = done(&one(&[svg.clone()], Action::single(Step::resize_max(300))));
    assert_eq!(image::image_dimensions(&resized).unwrap(), (300, 150));
    let jpeg = convert(&svg, Format::Jpeg);
    assert_eq!(detect(&jpeg).unwrap(), Format::Jpeg);
    let ico = convert(&svg, Format::Ico);
    assert_eq!(image::image_dimensions(&ico).unwrap(), (256, 128));

    let pdf = convert(&svg, Format::Pdf);
    let doc = lopdf::Document::load(&pdf).unwrap();
    let page = doc.get_pages()[&1];
    let media = doc
        .get_dictionary(page)
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let width = media[2].as_float().unwrap();
    assert!(
        (width - 18.0).abs() < 0.01,
        "24 px at 96 dpi is 18 pt, got {width}"
    );
}

#[test]
fn data_files_convert_and_tidy() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("orders.csv");
    std::fs::write(&csv, "item;qty\r\nTea;2\r\nCoffee;10\r\n").unwrap();
    let json = convert(&csv, Format::Json);
    assert_eq!(
        std::fs::read_to_string(&json).unwrap(),
        "[\n  {\n    \"item\": \"Tea\",\n    \"qty\": 2\n  },\n  {\n    \"item\": \"Coffee\",\n    \"qty\": 10\n  }\n]\n"
    );
    let tsv = convert(&json, Format::Tsv);
    assert_eq!(
        std::fs::read_to_string(&tsv).unwrap(),
        "item\tqty\nTea\t2\nCoffee\t10\n"
    );
    let md = convert(&csv, Format::Md);
    assert!(std::fs::read_to_string(&md)
        .unwrap()
        .contains("| Tea | 2 |"));

    let mini = done(&one(
        &[json.clone()],
        Action::single(Step::FormatJson {
            minify: true,
            sort_keys: false,
        }),
    ));
    assert_eq!(name(&mini), "orders (minified).json");
    assert_eq!(
        std::fs::read_to_string(&mini).unwrap(),
        r#"[{"item":"Tea","qty":2},{"item":"Coffee","qty":10}]"#
    );
    let again = one(
        &[json],
        Action::single(Step::FormatJson {
            minify: false,
            sort_keys: false,
        }),
    );
    assert_eq!(again.status, Status::Skipped, "already formatted");

    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "one  \r\ntwo\t\r\n\r\n\r\n").unwrap();
    let cleaned = done(&one(&[notes], Action::single(Step::CleanText)));
    assert_eq!(name(&cleaned), "notes (cleaned).txt");
    assert_eq!(std::fs::read_to_string(&cleaned).unwrap(), "one\ntwo\n");
    let twice = one(&[cleaned], Action::single(Step::CleanText));
    assert_eq!(twice.status, Status::Skipped);
    // Tables are left alone: a trailing tab is an empty cell.
    let refused = one(&[csv.clone()], Action::single(Step::CleanText));
    assert_eq!(refused.status, Status::Failed);

    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{\"a\": }").unwrap();
    let o = one(
        &[bad],
        Action::single(Step::FormatJson {
            minify: false,
            sort_keys: false,
        }),
    );
    assert_eq!(o.status, Status::Failed);
    assert!(o.error.unwrap().message.contains("isn't valid JSON"));
}

fn folder_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
    std::fs::write(root.join("a.txt"), "alpha").unwrap();
    std::fs::write(root.join("pic.png"), "not really a png").unwrap();
    std::fs::write(root.join("sub/a.txt"), "alpha").unwrap();
    std::fs::write(root.join("sub/deeper/b.txt"), "beta").unwrap();
}

#[test]
fn folders_zip_and_unzip() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("Project");
    folder_fixture(&f);
    assert_eq!(detect(&f).unwrap(), Format::Folder);

    let zip = convert(&f, Format::Zip);
    assert_eq!(name(&zip), "Project.zip");
    let out = convert(&zip, Format::Folder);
    // One folder at the top of the archive comes out as itself.
    assert_eq!(name(&out), "Project 2");
    assert_eq!(
        std::fs::read_to_string(out.join("sub/deeper/b.txt")).unwrap(),
        "beta"
    );

    let other = dir.path().join("loose.txt");
    std::fs::write(&other, "loose").unwrap();
    let all = done(&one(&[f.clone(), other.clone()], Action::ZipAll));
    assert_eq!(name(&all), "Archive.zip");
    let unpacked = convert(&all, Format::Folder);
    assert_eq!(name(&unpacked), "Archive");
    assert!(unpacked.join("Project/a.txt").is_file() && unpacked.join("loose.txt").is_file());

    // A single file in an archive comes out beside it.
    let single = done(&one(&[other], Action::ZipAll));
    assert_eq!(name(&single), "loose.txt.zip");
    let file = convert(&single, Format::Folder);
    assert_eq!(name(&file), "loose 2.txt");
}

#[test]
fn folders_tidy_into_copies() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("Stuff");
    folder_fixture(&f);
    let g = dir.path().join("More");
    std::fs::create_dir_all(g.join("sub")).unwrap();
    std::fs::write(g.join("sub/c.txt"), "gamma").unwrap();
    std::fs::write(g.join("a.txt"), "another alpha").unwrap();

    let merged = done(&one(&[f.clone(), g.clone()], Action::MergeFolders));
    assert_eq!(name(&merged), "Stuff (merged)");
    assert!(merged.join("sub/c.txt").is_file() && merged.join("sub/deeper/b.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(merged.join("a 2.txt")).unwrap(),
        "another alpha"
    );

    let flat = done(&one(&[f.clone()], Action::Flatten));
    assert_eq!(name(&flat), "Stuff (flattened)");
    let mut names: Vec<String> = std::fs::read_dir(&flat)
        .unwrap()
        .map(|e| name(&e.unwrap().path()))
        .collect();
    names.sort();
    assert_eq!(names, ["a.txt", "b.txt", "pic.png"]);

    let sorted = done(&one(&[f.clone()], Action::SortByKind));
    assert!(sorted.join("Text/a.txt").is_file());
    assert!(sorted.join("Images/pic.png").is_file());
    assert!(sorted.join("Folders/sub/deeper/b.txt").is_file());

    let list = done(&one(&[f.clone()], Action::ListContents { format: None }));
    assert_eq!(name(&list), "Stuff (contents).txt");
    assert!(std::fs::read_to_string(&list)
        .unwrap()
        .contains("│   ├── deeper/\n│   │   └── b.txt"));
    let csv = done(&one(
        &[f.clone()],
        Action::ListContents {
            format: Some(Format::Csv),
        },
    ));
    assert_eq!(name(&csv), "Stuff (contents).csv");

    let dups = done(&one(&[f.clone()], Action::FindDuplicates));
    assert_eq!(name(&dups), "Stuff (duplicates).txt");
    assert!(std::fs::read_to_string(&dups)
        .unwrap()
        .contains("  a.txt\n  sub/a.txt\n"));
    let none = one(&[g.clone()], Action::FindDuplicates);
    assert_eq!(none.status, Status::Skipped);

    // The originals are untouched.
    assert_eq!(
        std::fs::read_to_string(f.join("sub/a.txt")).unwrap(),
        "alpha"
    );
    let sum = one(&[f], Action::checksum());
    assert_eq!(sum.status, Status::Failed);
}

#[test]
fn the_wheel_sees_folders_and_documents() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("F");
    folder_fixture(&f);
    let caps = Capabilities::none();
    let w = radlet_core::catalog::wheel(&[detect(&f).ok()], &caps, &[]);
    assert_eq!(w.convert[0].label, "ZIP");
    assert!(w.tools.iter().any(|s| s.id == "flatten"));
    let md = dir.path().join("x.md");
    std::fs::write(&md, "hi").unwrap();
    let w = radlet_core::catalog::wheel(&[detect(&md).ok()], &caps, &[]);
    assert!(w.convert.iter().any(|s| s.label == "Word"));
}
