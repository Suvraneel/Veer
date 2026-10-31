//! Format-specific engines. Each one is a set of plain functions over paths
//! and decoded data; orchestration lives in [`crate::pipeline`].

pub mod anim;
pub mod archive;
pub mod avmedia;
pub mod bsdtar;
pub mod codes;
pub mod data;
pub mod devtools;
pub mod doc;
pub mod docpdf;
pub mod docx;
pub mod epub;
pub mod folder;
pub mod font;
pub mod htmlread;
pub mod icons;
pub mod image;
pub mod iwork;
pub mod media;
pub mod meta;
pub mod pdf;
pub mod pdfedit;
pub mod pdfium;
pub mod pptx;
pub mod rtf;
pub mod sheet;
pub mod shortcuts;
pub mod sips;
pub mod speech;
pub mod svg;
pub mod text;
pub mod trace;
pub mod translate;
pub mod tree;
pub mod vision;

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Streams a file through SHA-256 and returns the lowercase hex digest.
pub fn sha256(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert_eq!(
            super::sha256(&p, &cancel).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
