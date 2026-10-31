//! Tools for developers' files: formatting and minifying XML and SVG,
//! optimizing SVG, sorting JSON keys, checking syntax, `data:` URIs,
//! Base64 and checksums other than SHA-256.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use quick_xml::events::{BytesStart, Event};

use crate::error::{Error, Result};
use crate::format::Format;
use crate::ops::HashKind;

/// The line and column (both from 1) of byte `offset` in `src`. Columns
/// count characters, as editors show them.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let mut at = offset;
    while !src.is_char_boundary(at) {
        at -= 1;
    }
    let before = &src[..at];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[start..].chars().count() + 1)
}

fn problem(src: &str, offset: usize, message: impl std::fmt::Display) -> Error {
    let (line, column) = line_col(src, offset);
    Error::Invalid(format!("line {line}, column {column}: {message}"))
}

// ---------------------------------------------------------------------------
// JSON

/// A JSON value with every number, string and literal kept as written.
enum Raw<'a> {
    Scalar(&'a str),
    Array(Vec<Raw<'a>>),
    Object(Vec<(&'a str, Raw<'a>)>),
}

struct RawParser<'a> {
    src: &'a str,
    at: usize,
}

impl<'a> RawParser<'a> {
    fn skip_ws(&mut self) {
        let b = self.src.as_bytes();
        while self.at < b.len() && matches!(b[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }

    fn string(&mut self) -> &'a str {
        let b = self.src.as_bytes();
        let start = self.at;
        self.at += 1;
        while self.at < b.len() {
            match b[self.at] {
                b'\\' => self.at += 2,
                b'"' => {
                    self.at += 1;
                    break;
                }
                _ => self.at += 1,
            }
        }
        &self.src[start..self.at]
    }

    /// The value at the current position. The text was already checked
    /// by serde_json, so it is valid JSON (and at most 128 levels deep).
    fn value(&mut self) -> Raw<'a> {
        self.skip_ws();
        let b = self.src.as_bytes();
        match b[self.at] {
            b'{' => {
                self.at += 1;
                let mut entries = Vec::new();
                loop {
                    self.skip_ws();
                    if b[self.at] == b'}' {
                        self.at += 1;
                        break;
                    }
                    let key = self.string();
                    self.skip_ws();
                    self.at += 1; // the colon
                    let value = self.value();
                    entries.push((key, value));
                    self.skip_ws();
                    if b[self.at] == b',' {
                        self.at += 1;
                    }
                }
                Raw::Object(entries)
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if b[self.at] == b']' {
                        self.at += 1;
                        break;
                    }
                    items.push(self.value());
                    self.skip_ws();
                    if b[self.at] == b',' {
                        self.at += 1;
                    }
                }
                Raw::Array(items)
            }
            b'"' => Raw::Scalar(self.string()),
            _ => {
                let start = self.at;
                while self.at < b.len()
                    && !matches!(
                        b[self.at],
                        b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r'
                    )
                {
                    self.at += 1;
                }
                Raw::Scalar(&self.src[start..self.at])
            }
        }
    }
}

impl Raw<'_> {
    fn sort(&mut self) {
        match self {
            Raw::Scalar(_) => {}
            Raw::Array(items) => items.iter_mut().for_each(Raw::sort),
            Raw::Object(entries) => {
                for (_, v) in entries.iter_mut() {
                    v.sort();
                }
                // By the key's text, escapes decoded; equal keys keep their order.
                entries.sort_by_cached_key(|(k, _)| {
                    serde_json::from_str::<String>(k).unwrap_or_else(|_| (*k).to_string())
                });
            }
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Raw::Scalar(s) => out.push_str(s),
            Raw::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Raw::Object(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// The JSON with every object's keys in alphabetical order, compact.
/// Numbers and strings are copied exactly as written.
pub fn sort_json_keys(src: &str) -> Result<String> {
    let src = src.trim_start_matches('\u{feff}');
    serde_json::from_str::<serde::de::IgnoredAny>(src)
        .map_err(|e| Error::Invalid(format!("this isn't valid JSON ({e})")))?;
    let mut raw = RawParser { src, at: 0 }.value();
    raw.sort();
    let mut out = String::with_capacity(src.len());
    raw.write(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// XML and SVG

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Decl,
    DocType,
    Comment,
    Pi,
    Start,
    Empty,
    End,
    Text,
    CData,
    Ref,
}

/// One piece of an XML document, as written (still escaped).
#[derive(Clone, Debug)]
struct Piece {
    kind: Kind,
    raw: String,
}

impl Piece {
    fn whitespace(&self) -> bool {
        self.kind == Kind::Text
            && self
                .raw
                .bytes()
                .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
    }

    /// The element's name, for Start, Empty and End.
    fn name(&self) -> &str {
        self.raw
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .next()
            .unwrap_or("")
    }

    fn write(&self, out: &mut String) {
        let (open, close) = match self.kind {
            Kind::Decl | Kind::Pi => ("<?", "?>"),
            Kind::DocType => ("<!DOCTYPE ", ">"),
            Kind::Comment => ("<!--", "-->"),
            Kind::Start => ("<", ">"),
            Kind::Empty => ("<", "/>"),
            Kind::End => ("</", ">"),
            Kind::Text => ("", ""),
            Kind::CData => ("<![CDATA[", "]]>"),
            Kind::Ref => ("&", ";"),
        };
        out.push_str(open);
        out.push_str(&self.raw);
        out.push_str(close);
    }
}

const PREDEFINED: [&str; 5] = ["amp", "lt", "gt", "quot", "apos"];

/// Reads a whole XML document into pieces, checking that it's well formed:
/// matching tags, one root element, quoted and unique attributes, and only
/// entities that are predefined or declared in its DOCTYPE.
fn parse_xml(src: &str) -> Result<Vec<Piece>> {
    let src = src.trim_start_matches('\u{feff}');
    let mut reader = quick_xml::Reader::from_str(src);
    let mut pieces = Vec::new();
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut roots = 0usize;
    let mut declared: Vec<String> = Vec::new();
    loop {
        let at = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|e| problem(src, reader.error_position() as usize, e))?;
        let (kind, raw): (Kind, &str) = match &event {
            Event::Eof => break,
            Event::Decl(b) => (Kind::Decl, b.as_ref()),
            Event::DocType(b) => (Kind::DocType, b.as_ref()),
            Event::Comment(b) => (Kind::Comment, b.as_ref()),
            Event::PI(b) => (Kind::Pi, b.as_ref()),
            Event::Start(b) => (Kind::Start, b.as_ref()),
            Event::Empty(b) => (Kind::Empty, b.as_ref()),
            Event::End(b) => (Kind::End, b.as_ref()),
            Event::Text(b) => (Kind::Text, b.as_ref()),
            Event::CData(b) => (Kind::CData, b.as_ref()),
            Event::GeneralRef(b) => (Kind::Ref, b.as_ref()),
        };
        match &event {
            Event::Start(b) | Event::Empty(b) => {
                if open.is_empty() {
                    roots += 1;
                    if roots > 1 {
                        return Err(problem(src, at, "a second root element; XML has one"));
                    }
                }
                check_attributes(b, src, at)?;
                if matches!(event, Event::Start(_)) {
                    open.push((
                        Piece {
                            kind,
                            raw: raw.into(),
                        }
                        .name()
                        .to_string(),
                        at,
                    ));
                }
            }
            Event::End(_) => {
                open.pop();
            }
            Event::DocType(b) => {
                let text: &str = b.as_ref();
                for part in text.split("<!ENTITY").skip(1) {
                    let name = part
                        .trim_start()
                        .trim_start_matches('%')
                        .trim_start()
                        .split(|c: char| c.is_whitespace())
                        .next()
                        .unwrap_or("");
                    declared.push(name.to_string());
                }
            }
            Event::GeneralRef(b) => {
                let name: &str = b.as_ref();
                let ok = if let Some(code) = name.strip_prefix('#') {
                    let n = match code.strip_prefix('x') {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => code.parse::<u32>().ok(),
                    };
                    n.and_then(char::from_u32).is_some()
                } else {
                    PREDEFINED.contains(&name) || declared.iter().any(|d| d == name)
                };
                if !ok {
                    return Err(problem(src, at, format!("unknown entity &{name};")));
                }
            }
            Event::Text(_) | Event::CData(_) if open.is_empty() => {
                let p = Piece {
                    kind,
                    raw: raw.into(),
                };
                if kind == Kind::CData || !p.whitespace() {
                    return Err(problem(src, at, "text outside the root element"));
                }
            }
            _ => {}
        }
        pieces.push(Piece {
            kind,
            raw: raw.to_string(),
        });
    }
    if let Some((name, at)) = open.last() {
        return Err(problem(src, *at, format!("<{name}> is never closed")));
    }
    if roots == 0 {
        return Err(problem(src, src.len(), "there's no root element"));
    }
    Ok(pieces)
}

fn check_attributes(start: &BytesStart<'_>, src: &str, at: usize) -> Result<()> {
    for attribute in start.attributes() {
        attribute.map_err(|e| problem(src, at, e))?;
    }
    Ok(())
}

/// Lays pieces out again: indented two spaces, or with nothing between
/// tags (`minify`, which also drops comments). Inside an element that
/// holds text (or says `xml:space="preserve"`) everything is copied
/// exactly, since spaces there are part of the content.
fn layout(pieces: &[Piece], minify: bool) -> String {
    // Which elements hold text, by the index of their start tag.
    let mut holds_text = vec![false; pieces.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        match p.kind {
            Kind::Start => {
                if p.raw.contains("xml:space=\"preserve\"")
                    || p.raw.contains("xml:space='preserve'")
                {
                    holds_text[i] = true;
                }
                stack.push(i);
            }
            Kind::End => {
                stack.pop();
            }
            Kind::Text if p.whitespace() => {}
            Kind::Text | Kind::CData | Kind::Ref => {
                if let Some(&top) = stack.last() {
                    holds_text[top] = true;
                }
            }
            _ => {}
        }
    }

    let mut out = String::new();
    // Open elements: copied exactly, and whether anything was put inside.
    let mut open: Vec<(bool, bool)> = Vec::new();
    let newline = |out: &mut String, depth: usize| {
        if !minify && !out.is_empty() {
            out.push('\n');
            for _ in 0..depth {
                out.push_str("  ");
            }
        }
    };
    for (i, p) in pieces.iter().enumerate() {
        if open.last().is_some_and(|o| o.0) {
            match p.kind {
                Kind::Start => open.push((true, true)),
                Kind::End => {
                    open.pop();
                }
                Kind::Comment if minify => continue,
                _ => {}
            }
            p.write(&mut out);
            continue;
        }
        match p.kind {
            Kind::Text if p.whitespace() => {}
            Kind::Comment if minify => {}
            Kind::End => {
                let had_children = open.pop().is_some_and(|o| o.1);
                if had_children {
                    newline(&mut out, open.len());
                }
                p.write(&mut out);
            }
            _ => {
                newline(&mut out, open.len());
                p.write(&mut out);
                if let Some(parent) = open.last_mut() {
                    parent.1 = true;
                }
                if p.kind == Kind::Start {
                    open.push((holds_text[i], false));
                }
            }
        }
    }
    if !minify {
        out.push('\n');
    }
    out
}

/// Re-indents an XML document, or minifies it.
pub fn format_xml(src: &str, minify: bool) -> Result<String> {
    Ok(layout(&parse_xml(src)?, minify))
}

/// Namespaces only drawing apps read.
const EDITOR_NAMESPACES: &[&str] = &[
    "http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd",
    "http://www.inkscape.org/namespaces/inkscape",
    "http://www.bohemiancoding.com/sketch/ns",
    "http://www.serif.com/",
    "http://ns.adobe.com/AdobeIllustrator/10.0/",
    "http://ns.adobe.com/AdobeSVGViewerExtensions/3.0/",
    "http://ns.adobe.com/Extensibility/1.0/",
    "http://ns.adobe.com/Flows/1.0/",
    "http://ns.adobe.com/GenericCustomNamespace/1.0/",
    "http://ns.adobe.com/Graphs/1.0/",
    "http://ns.adobe.com/ImageReplacement/1.0/",
    "http://ns.adobe.com/SaveForWeb/1.0/",
    "http://ns.adobe.com/Variables/1.0/",
    "http://ns.adobe.com/XPath/1.0/",
];

/// Namespaces of metadata, dropped with it when nothing else uses them.
const METADATA_NAMESPACES: &[&str] = &[
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://purl.org/dc/elements/1.1/",
    "http://creativecommons.org/ns#",
    "http://web.resource.org/cc/",
];

/// An element's or attribute's prefix: "inkscape" for "inkscape:label".
fn prefix(name: &str) -> Option<&str> {
    name.split_once(':').map(|(p, _)| p)
}

/// The attributes of a start tag, as (name, raw value).
fn attributes(raw: &str) -> Vec<(String, String)> {
    let name_len = raw
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(raw.len());
    let start = BytesStart::from_content(raw, name_len);
    start
        .attributes()
        .flatten()
        .map(|a| {
            let key: &str = a.key.as_ref();
            (key.to_string(), a.value.into_owned())
        })
        .collect()
}

/// A start tag's text again, with only the attributes `keep` allows.
fn rebuild(raw: &str, keep: impl Fn(&str, &str) -> bool) -> Option<String> {
    let all = attributes(raw);
    let kept: Vec<&(String, String)> = all.iter().filter(|(k, v)| keep(k, v)).collect();
    if kept.len() == all.len() {
        return None;
    }
    let name = raw
        .split(|c: char| c.is_ascii_whitespace())
        .next()
        .unwrap_or("");
    let mut out = name.to_string();
    for (k, v) in kept {
        let quote = if v.contains('"') { '\'' } else { '"' };
        out.push(' ');
        out.push_str(k);
        out.push('=');
        out.push(quote);
        out.push_str(v);
        out.push(quote);
    }
    Some(out)
}

/// Makes an SVG smaller without changing how it looks (see
/// [`crate::ops::Step::OptimizeSvg`]).
pub fn optimize_svg(src: &str) -> Result<String> {
    let pieces = parse_xml(src)?;
    // Prefixes bound to namespaces only editors read.
    let mut editor: Vec<String> = Vec::new();
    let mut metadata: Vec<String> = Vec::new();
    for p in &pieces {
        if matches!(p.kind, Kind::Start | Kind::Empty) {
            for (k, v) in attributes(&p.raw) {
                if let Some(px) = k.strip_prefix("xmlns:") {
                    if EDITOR_NAMESPACES.contains(&v.as_str()) {
                        editor.push(px.to_string());
                    } else if METADATA_NAMESPACES.contains(&v.as_str()) {
                        metadata.push(px.to_string());
                    }
                }
            }
        }
    }
    let is_editor = |name: &str| prefix(name).is_some_and(|p| editor.iter().any(|e| e == p));

    // Drop editor elements, metadata and comments; strip editor attributes.
    let mut kept: Vec<Piece> = Vec::new();
    let mut skipping = 0usize;
    for p in pieces {
        if skipping > 0 {
            match p.kind {
                Kind::Start => skipping += 1,
                Kind::End => skipping -= 1,
                _ => {}
            }
            continue;
        }
        match p.kind {
            Kind::Decl | Kind::Comment => continue,
            // An internal subset can declare entities the drawing uses.
            Kind::DocType if !p.raw.contains('[') => continue,
            Kind::Start | Kind::Empty => {
                let name = p.name().to_string();
                if name == "metadata" || is_editor(&name) {
                    if p.kind == Kind::Start {
                        skipping = 1;
                    }
                    continue;
                }
                let raw = rebuild(&p.raw, |k, _| {
                    let editor_ns = k
                        .strip_prefix("xmlns:")
                        .is_some_and(|px| editor.iter().any(|e| e == px));
                    !editor_ns && !is_editor(k)
                })
                .unwrap_or(p.raw);
                kept.push(Piece { kind: p.kind, raw });
            }
            _ => kept.push(p),
        }
    }

    // Metadata namespaces nothing uses any more.
    let used = |px: &str| {
        kept.iter()
            .filter(|p| matches!(p.kind, Kind::Start | Kind::Empty))
            .any(|p| {
                prefix(p.name()) == Some(px)
                    || attributes(&p.raw)
                        .iter()
                        .any(|(k, _)| !k.starts_with("xmlns") && prefix(k) == Some(px))
            })
    };
    let unused: Vec<String> = metadata.into_iter().filter(|px| !used(px)).collect();
    if !unused.is_empty() {
        for p in kept.iter_mut() {
            if matches!(p.kind, Kind::Start | Kind::Empty) {
                if let Some(raw) = rebuild(&p.raw, |k, _| {
                    !k.strip_prefix("xmlns:")
                        .is_some_and(|px| unused.iter().any(|u| u == px))
                }) {
                    p.raw = raw;
                }
            }
        }
    }

    // Empty groups and definitions draw nothing.
    let bare = |p: &Piece| matches!(p.raw.as_str(), "g" | "defs");
    loop {
        let before = kept.len();
        kept.retain(|p| !(p.kind == Kind::Empty && bare(p)));
        let mut i = 0;
        while i < kept.len() {
            if kept[i].kind == Kind::Start && bare(&kept[i]) {
                let mut j = i + 1;
                while j < kept.len() && kept[j].whitespace() {
                    j += 1;
                }
                if j < kept.len() && kept[j].kind == Kind::End {
                    kept.drain(i..=j);
                    continue;
                }
            }
            i += 1;
        }
        if kept.len() == before {
            break;
        }
    }
    // A group with no attributes changes nothing about what it holds.
    let mut i = 0;
    while i < kept.len() {
        if kept[i].kind == Kind::Start && kept[i].raw == "g" {
            let mut depth = 0usize;
            let end = (i + 1..kept.len()).find(|&j| match kept[j].kind {
                Kind::Start => {
                    depth += 1;
                    false
                }
                Kind::End if depth == 0 => true,
                Kind::End => {
                    depth -= 1;
                    false
                }
                _ => false,
            });
            if let Some(j) = end {
                kept.remove(j);
                kept.remove(i);
                continue;
            }
        }
        i += 1;
    }
    Ok(layout(&kept, true))
}

// ---------------------------------------------------------------------------
// Checking syntax

/// "Valid JSON", or why the file isn't (as an `Error::Invalid` naming the
/// line and column of the first problem).
pub fn check_syntax(src: &str, fmt: Format) -> Result<String> {
    let src = src.trim_start_matches('\u{feff}');
    match fmt {
        Format::Json => {
            serde_json::from_str::<serde::de::IgnoredAny>(src).map_err(|e| {
                let message = e.to_string();
                let message = message
                    .rsplit_once(" at line ")
                    .map_or(message.as_str(), |(m, _)| m)
                    .to_string();
                Error::Invalid(format!(
                    "line {}, column {}: {message}",
                    e.line().max(1),
                    e.column().max(1)
                ))
            })?;
            Ok("Valid JSON".into())
        }
        Format::Yaml => {
            let docs = yaml_rust2::YamlLoader::load_from_str(src).map_err(|e| {
                let m = e.marker();
                let info = e.info().to_string();
                Error::Invalid(format!(
                    "line {}, column {}: {info}",
                    m.line().max(1),
                    m.col() + 1
                ))
            })?;
            Ok(match docs.len() {
                0 | 1 => "Valid YAML".into(),
                n => format!("Valid YAML · {n} documents"),
            })
        }
        Format::Toml => {
            src.parse::<toml::Table>().map_err(|e| {
                let at = e.span().map_or(0, |s| s.start);
                problem(src, at, e.message().trim())
            })?;
            Ok("Valid TOML".into())
        }
        Format::Xml | Format::Svg => {
            parse_xml(src)?;
            Ok(if fmt == Format::Svg {
                "Well-formed SVG".into()
            } else {
                "Well-formed XML".into()
            })
        }
        Format::Csv | Format::Tsv => {
            let delimiter = if fmt == Format::Tsv { b'\t' } else { b',' };
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .delimiter(delimiter)
                .from_reader(src.as_bytes());
            let mut rows = 0usize;
            let mut columns = 0usize;
            for record in reader.records() {
                match record {
                    Ok(r) => {
                        rows += 1;
                        columns = columns.max(r.len());
                    }
                    Err(e) => {
                        return Err(match e.kind() {
                            csv::ErrorKind::UnequalLengths {
                                pos: Some(pos),
                                expected_len,
                                len,
                            } => Error::Invalid(format!(
                                "line {}: {len} fields, but the first row has {expected_len}",
                                pos.line()
                            )),
                            _ => Error::Invalid(e.to_string()),
                        });
                    }
                }
            }
            let what = if fmt == Format::Tsv { "TSV" } else { "CSV" };
            let rows_text = if rows == 1 {
                "1 row".to_string()
            } else {
                format!("{rows} rows")
            };
            Ok(format!("Valid {what} · {rows_text} × {columns} columns"))
        }
        other => Err(Error::UnsupportedInput(format!(
            "there's no syntax check for {other} files"
        ))),
    }
}

/// Formats whose syntax can be checked.
pub fn checks_syntax(f: Format) -> bool {
    matches!(
        f,
        Format::Json
            | Format::Yaml
            | Format::Toml
            | Format::Xml
            | Format::Svg
            | Format::Csv
            | Format::Tsv
    )
}

// ---------------------------------------------------------------------------
// Data URIs, Base64 and checksums

/// The largest file a data URI is made of: past this, linking to the file
/// is better.
pub const DATA_URI_MAX: u64 = 2 * 1024 * 1024;

/// The media type a `data:` URI gives `f`, for formats browsers show.
pub fn data_uri_type(f: Format) -> Option<&'static str> {
    Some(match f {
        Format::Png => "image/png",
        Format::Jpeg => "image/jpeg",
        Format::Gif => "image/gif",
        Format::Webp => "image/webp",
        Format::Avif => "image/avif",
        Format::Svg => "image/svg+xml",
        Format::Ico => "image/x-icon",
        Format::Bmp => "image/bmp",
        Format::Ttf => "font/ttf",
        Format::Otf => "font/otf",
        Format::Woff => "font/woff",
        Format::Woff2 => "font/woff2",
        _ => return None,
    })
}

/// The file as a `data:` URI.
pub fn data_uri(path: &Path, fmt: Format) -> Result<String> {
    use base64::Engine;
    let kind = data_uri_type(fmt)
        .ok_or_else(|| Error::UnsupportedInput(format!("{fmt} files don't go in data URIs")))?;
    let size = std::fs::metadata(path)?.len();
    if size > DATA_URI_MAX {
        return Err(Error::UnsupportedInput(
            "it's over 2 MB, too big for a data URI; link to the file instead".into(),
        ));
    }
    let bytes = std::fs::read(path)?;
    Ok(format!(
        "data:{kind};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// The largest file Base64 encodes or decodes.
pub const BASE64_MAX: u64 = 256 * 1024 * 1024;

/// True for files Base64 decodes rather than encodes.
pub fn is_base64_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("b64") || e.eq_ignore_ascii_case("base64"))
}

/// One line of Base64, with a newline at the end.
pub fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    let mut out = base64::engine::general_purpose::STANDARD.encode(bytes);
    out.push('\n');
    out
}

/// Decodes Base64 text: the standard or URL-safe alphabet, padded or not,
/// across any number of lines, or a whole `data:` URI.
pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::{alphabet, Engine};
    let text = text.trim_start_matches('\u{feff}').trim();
    let body = match text.strip_prefix("data:") {
        Some(rest) => rest
            .split_once(";base64,")
            .map(|(_, b)| b)
            .ok_or_else(|| Error::Invalid("this data URI isn't Base64".into()))?,
        None => text,
    };
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return Err(Error::NothingToDo("it's empty".into()));
    }
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let url_safe = compact.contains(['-', '_']);
    let engine = if url_safe {
        GeneralPurpose::new(&alphabet::URL_SAFE, config)
    } else {
        GeneralPurpose::new(&alphabet::STANDARD, config)
    };
    engine
        .decode(compact.as_bytes())
        .map_err(|e| Error::Invalid(format!("this isn't Base64 ({e})")))
}

/// Reads a file through `update`, a megabyte at a time.
fn feed(path: &Path, cancel: &AtomicBool, update: &mut dyn FnMut(&[u8])) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        update(&buf[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A file's checksum, in lowercase hex.
pub fn digest(path: &Path, kind: HashKind, cancel: &AtomicBool) -> Result<String> {
    match kind {
        HashKind::Sha256 => super::sha256(path, cancel),
        HashKind::Sha512 => {
            use sha2::Digest;
            let mut h = sha2::Sha512::new();
            feed(path, cancel, &mut |b| h.update(b))?;
            Ok(hex(&h.finalize()))
        }
        HashKind::Md5 => {
            use md5::Digest;
            let mut h = md5::Md5::new();
            feed(path, cancel, &mut |b| h.update(b))?;
            Ok(hex(&h.finalize()))
        }
        HashKind::Sha1 => {
            use sha1::Digest;
            let mut h = sha1::Sha1::new();
            feed(path, cancel, &mut |b| h.update(b))?;
            Ok(hex(&h.finalize()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid(r: Result<impl std::fmt::Debug>) -> String {
        match r {
            Err(Error::Invalid(m)) => m,
            other => panic!("expected an Invalid error, got {other:?}"),
        }
    }

    #[test]
    fn lines_and_columns_count_characters() {
        let s = "ab\ncdé\nf";
        assert_eq!(line_col(s, 0), (1, 1));
        assert_eq!(line_col(s, 3), (2, 1));
        assert_eq!(line_col(s, s.find('f').unwrap()), (3, 1));
        assert_eq!(line_col(s, s.len()), (3, 2));
        // Inside a multi-byte character: the character's own column.
        assert_eq!(line_col(s, s.find('é').unwrap() + 1), (2, 3));
    }

    #[test]
    fn json_keys_sort_without_touching_values() {
        let src = "\u{feff}{\"b\": [1.50, {\"y\": 1e3, \"x\": \"\\u00e9\"}], \"a\": true, \"\\u0041\": null, \"a\": 2}";
        assert_eq!(
            sort_json_keys(src).unwrap(),
            r#"{"\u0041":null,"a":true,"a":2,"b":[1.50,{"x":"\u00e9","y":1e3}]}"#
        );
        assert_eq!(sort_json_keys("[3, 1]").unwrap(), "[3,1]");
        assert_eq!(sort_json_keys(" \"x\" ").unwrap(), "\"x\"");
        assert!(sort_json_keys("{\"a\":").is_err());
    }

    #[test]
    fn xml_formats_and_minifies_keeping_text_exactly() {
        let src = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [ <!ENTITY me \"x\"> ]>\n<!-- c -->\n<r a='1'><p>Hi <b>you</b> &amp; &me; &#169;</p><list>\n  <i/> <i>2</i></list><![CDATA[ignored]]></r>";
        // CDATA in <r> makes it hold text: kept as it is.
        let formatted = format_xml(src, false).unwrap();
        assert!(formatted.starts_with(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE r [ <!ENTITY me \"x\"> ]>\n<!-- c -->\n<r a='1'>"
        ));
        assert!(formatted
            .contains("<p>Hi <b>you</b> &amp; &me; &#169;</p><list>\n  <i/> <i>2</i></list>"));
        let plain =
            "<r>\n<a><b>1</b>  <c/></a><d></d>\n<!-- x --><e xml:space=\"preserve\">  </e></r>";
        assert_eq!(
            format_xml(plain, false).unwrap(),
            "<r>\n  <a>\n    <b>1</b>\n    <c/>\n  </a>\n  <d></d>\n  <!-- x -->\n  <e xml:space=\"preserve\">  </e>\n</r>\n"
        );
        let small = format_xml(plain, true).unwrap();
        assert_eq!(
            small,
            "<r><a><b>1</b><c/></a><d></d><e xml:space=\"preserve\">  </e></r>"
        );
        // Formatting is settled after one go, and minified text formats the same.
        let once = format_xml(plain, false).unwrap();
        assert_eq!(format_xml(&once, false).unwrap(), once);
        assert_eq!(
            format_xml(&small, false).unwrap(),
            once.replace("  <!-- x -->\n", "")
        );
    }

    #[test]
    fn broken_xml_says_where() {
        assert_eq!(
            invalid(format_xml("<a>\n  <b></a>", false)),
            "line 2, column 6: ill-formed document: expected `</b>`, but `</a>` was found"
        );
        assert_eq!(
            invalid(format_xml("<a>\n<b>", false)),
            "line 2, column 1: <b> is never closed"
        );
        assert!(invalid(format_xml("<a/><b/>", false)).contains("second root element"));
        assert!(invalid(format_xml("<a>&nope;</a>", false)).contains("unknown entity &nope;"));
        assert!(invalid(format_xml("<a x=1/>", false)).starts_with("line 1, column 1:"));
        assert!(invalid(format_xml("<a x='1' x='2'/>", false)).contains("line 1"));
        assert!(invalid(format_xml("hello <a/>", false)).contains("text outside the root"));
        assert!(invalid(format_xml("  ", false)).contains("no root element"));
    }

    const INKSCAPE: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- Created with Inkscape -->
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dc="http://purl.org/dc/elements/1.1/" width="48" height="48" viewBox="0 0 48 48" inkscape:version="1.3">
  <sodipodi:namedview id="base" inkscape:zoom="8"><inkscape:grid id="g1"/></sodipodi:namedview>
  <metadata><rdf:RDF><dc:title>Icon</dc:title></rdf:RDF></metadata>
  <defs><linearGradient id="lg"><stop offset="0" stop-color="#f80"/><stop offset="1" stop-color="#08f"/></linearGradient></defs>
  <title>Icon</title>
  <g inkscape:label="Layer" inkscape:groupmode="layer">
    <rect x="4" y="4" width="40" height="40" rx="8" fill="url(#lg)" sodipodi:nodetypes="cccc"/>
    <use xlink:href="#dot" x="2"/>
    <circle id="dot" cx="24" cy="24" r="6" fill="#fff"/>
  </g>
  <g>
  </g>
  <defs/>
</svg>
"##;

    #[test]
    fn optimized_svg_draws_the_same() {
        let small = optimize_svg(INKSCAPE).unwrap();
        for gone in [
            "inkscape", "sodipodi", "metadata", "rdf", "dc:", "<!--", "<?xml", "<g>", "<defs/>",
        ] {
            assert!(!small.contains(gone), "{gone} left in {small}");
        }
        for kept in [
            "xmlns:xlink",
            "xlink:href=\"#dot\"",
            "<title>Icon</title>",
            "url(#lg)",
            "viewBox",
        ] {
            assert!(small.contains(kept), "{kept} missing from {small}");
        }
        assert!(small.len() * 2 < INKSCAPE.len());
        let dir = tempfile::tempdir().unwrap();
        let render = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            let tree = crate::engines::svg::load(&path).unwrap();
            crate::engines::svg::render(&tree, 96, 96)
                .unwrap()
                .img
                .to_rgba8()
                .into_raw()
        };
        assert_eq!(render("a.svg", INKSCAPE), render("b.svg", &small));
        // Already as small as it gets: the same again.
        assert_eq!(optimize_svg(&small).unwrap(), small);
    }

    #[test]
    fn illustrator_entities_survive_optimizing() {
        let src = r#"<?xml version="1.0"?>
<!DOCTYPE svg [ <!ENTITY ns_ai "http://ns.adobe.com/AdobeIllustrator/10.0/"> ]>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:i="&ns_ai;" viewBox="0 0 10 10"><rect width="10" height="10" i:knockout="Off"/></svg>"#;
        let small = optimize_svg(src).unwrap();
        assert!(small.contains("<!DOCTYPE svg ["), "{small}");
        assert!(format_xml(&small, false).is_ok());
    }

    #[test]
    fn syntax_checks_name_the_first_problem() {
        assert_eq!(
            check_syntax("{\"a\": [1, 2]}", Format::Json).unwrap(),
            "Valid JSON"
        );
        assert_eq!(
            invalid(check_syntax("{\"a\": 1,\n \"b\": }", Format::Json)),
            "line 2, column 7: expected value"
        );
        assert_eq!(
            invalid(check_syntax("", Format::Json)),
            "line 1, column 1: EOF while parsing a value"
        );
        assert_eq!(
            check_syntax("a: 1\n---\nb: 2\n", Format::Yaml).unwrap(),
            "Valid YAML · 2 documents"
        );
        assert!(invalid(check_syntax("a: [1, 2\n", Format::Yaml)).starts_with("line 2, column 1: "));
        assert_eq!(
            check_syntax("a = 1 # c\n[t]\nb = \"x\"\n", Format::Toml).unwrap(),
            "Valid TOML"
        );
        assert!(
            invalid(check_syntax("a = 1\nb = \n", Format::Toml)).starts_with("line 2, column 5: ")
        );
        assert_eq!(
            check_syntax("<a/>", Format::Xml).unwrap(),
            "Well-formed XML"
        );
        assert_eq!(
            check_syntax(INKSCAPE, Format::Svg).unwrap(),
            "Well-formed SVG"
        );
        assert_eq!(
            check_syntax("a,b\n1,\"2,3\"\n", Format::Csv).unwrap(),
            "Valid CSV · 2 rows × 2 columns"
        );
        assert_eq!(
            invalid(check_syntax("a,b\n1\n", Format::Csv)),
            "line 2: 1 fields, but the first row has 2"
        );
        assert_eq!(
            check_syntax("a\tb\n", Format::Tsv).unwrap(),
            "Valid TSV · 1 row × 2 columns"
        );
        assert!(matches!(
            check_syntax("x", Format::Md),
            Err(Error::UnsupportedInput(_))
        ));
    }

    #[test]
    fn data_uris_and_base64() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("a.svg");
        std::fs::write(&svg, "<svg/>").unwrap();
        assert_eq!(
            data_uri(&svg, Format::Svg).unwrap(),
            "data:image/svg+xml;base64,PHN2Zy8+"
        );
        assert!(matches!(
            data_uri(&svg, Format::Pdf),
            Err(Error::UnsupportedInput(_))
        ));
        let big = dir.path().join("big.png");
        std::fs::write(&big, vec![0u8; DATA_URI_MAX as usize + 1]).unwrap();
        assert!(matches!(
            data_uri(&big, Format::Png),
            Err(Error::UnsupportedInput(_))
        ));

        let bytes: Vec<u8> = (0u8..=255).collect();
        let text = base64_encode(&bytes);
        assert!(text.ends_with('\n') && !text.trim_end().contains('\n'));
        assert_eq!(base64_decode(&text).unwrap(), bytes);
        // Wrapped, unpadded, URL-safe, or a whole data URI.
        assert_eq!(base64_decode("aGVs\nbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64_decode("data:text/plain;base64,aGk=").unwrap(), b"hi");
        assert!(matches!(
            base64_decode("not base64!"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(base64_decode("  "), Err(Error::NothingToDo(_))));
        assert!(is_base64_file(Path::new("x/photo.png.B64")));
        assert!(!is_base64_file(Path::new("photo.png")));
    }

    #[test]
    fn checksums_match_known_values() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("abc.txt");
        std::fs::write(&f, "abc").unwrap();
        let no = AtomicBool::new(false);
        assert_eq!(
            digest(&f, HashKind::Md5, &no).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            digest(&f, HashKind::Sha1, &no).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            digest(&f, HashKind::Sha256, &no).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(digest(&f, HashKind::Sha512, &no)
            .unwrap()
            .starts_with("ddaf35a193617aba"));
        assert!(matches!(
            digest(&f, HashKind::Md5, &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
    }
}
