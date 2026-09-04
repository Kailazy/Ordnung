//! Cover art for the USB export — `/PIONEER/Artwork`.
//!
//! rekordbox writes each referenced cover four times per artwork id `N`:
//! `aN.jpg` (80×80) + `aN_m.jpg` (240×240), referenced by `export.pdb`'s
//! artwork table, and byte-identical `bN.jpg` + `bN_m.jpg` copies referenced
//! by `exportLibrary.db`'s image table. Ids pack 20 per directory:
//! `Artwork/%05d` where the dir number is `(N-1)/20 + 1` (verified against
//! the EYEBAGS golden reference; see `docs/rekordbox-export-structure.md` §6).
//!
//! Covers come out of the audio files' own tags at export time; identical
//! images (byte-for-byte) shared across an album's tracks intern to one id.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

/// One artwork id's processed images, ready to hit the stick.
pub(crate) struct ArtworkFiles {
    pub id: u32,
    /// 80×80 JPEG (`aN.jpg` / `bN.jpg`).
    pub small: Vec<u8>,
    /// 240×240 JPEG (`aN_m.jpg` / `bN_m.jpg`).
    pub medium: Vec<u8>,
}

/// Interns raw cover bytes to dense 1-based artwork ids, decoding and scaling
/// each distinct image once. Covers that fail to decode intern to id 0 (no
/// artwork) — a corrupt embedded picture must never fail an export.
#[derive(Default)]
pub(crate) struct ArtworkStore {
    by_hash: HashMap<u64, u32>,
    pub files: Vec<ArtworkFiles>,
}

impl ArtworkStore {
    /// Intern one cover; returns its artwork id, or 0 if it can't be decoded.
    pub fn intern(&mut self, raw: &[u8]) -> u32 {
        let mut h = DefaultHasher::new();
        raw.hash(&mut h);
        let key = h.finish();
        if let Some(&id) = self.by_hash.get(&key) {
            return id;
        }
        let Some((small, medium)) = scale_cover(raw) else {
            self.by_hash.insert(key, 0);
            return 0;
        };
        let id = self.files.len() as u32 + 1;
        self.by_hash.insert(key, id);
        self.files.push(ArtworkFiles { id, small, medium });
        id
    }
}

/// The `export.pdb` artwork path for id `N` (the a-file).
pub(crate) fn pdb_path(id: u32) -> String {
    format!("/PIONEER/Artwork/{:05}/a{}.jpg", (id - 1) / 20 + 1, id)
}

/// The `exportLibrary.db` image path for id `N` (the b-file).
pub(crate) fn dlp_path(id: u32) -> String {
    format!("/PIONEER/Artwork/{:05}/b{}.jpg", (id - 1) / 20 + 1, id)
}

/// Write one artwork id's four files under `dest_root`, creating the
/// numbered directory as needed.
pub(crate) fn write_files(dest_root: &Path, art: &ArtworkFiles) -> std::io::Result<PathBuf> {
    let dir = dest_root
        .join("PIONEER")
        .join("Artwork")
        .join(format!("{:05}", (art.id - 1) / 20 + 1));
    std::fs::create_dir_all(&dir)?;
    for (prefix, bytes) in [("a", &art.small), ("b", &art.small)] {
        crate::export::write_synced(&dir.join(format!("{prefix}{}.jpg", art.id)), bytes)?;
    }
    for (prefix, bytes) in [("a", &art.medium), ("b", &art.medium)] {
        crate::export::write_synced(&dir.join(format!("{prefix}{}_m.jpg", art.id)), bytes)?;
    }
    Ok(dir)
}

/// Decode a cover and produce the two fixed-size JPEGs rekordbox writes:
/// center-cropped square, scaled to exactly 80×80 and 240×240.
fn scale_cover(raw: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let img = image::load_from_memory(raw).ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let jpeg = |px: u32| -> Option<Vec<u8>> {
        let scaled = square.resize_exact(px, px, image::imageops::FilterType::Lanczos3);
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        // JPEG has no alpha; flatten to RGB first.
        scaled.to_rgb8().write_with_encoder(enc).ok()?;
        Some(out)
    };
    Some((jpeg(80)?, jpeg(240)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny valid PNG to intern (blue 10×6 — non-square, exercising the crop).
    fn test_png() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(10, 6, image::Rgb([20, 40, 200]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn interns_dedupes_and_scales() {
        let mut store = ArtworkStore::default();
        let png = test_png();
        let id = store.intern(&png);
        assert_eq!(id, 1);
        assert_eq!(store.intern(&png), 1, "same bytes intern to the same id");
        assert_eq!(store.intern(b"not an image"), 0, "undecodable interns to 0");
        assert_eq!(store.files.len(), 1);

        let art = &store.files[0];
        let small = image::load_from_memory(&art.small).unwrap();
        assert_eq!((small.width(), small.height()), (80, 80));
        let medium = image::load_from_memory(&art.medium).unwrap();
        assert_eq!((medium.width(), medium.height()), (240, 240));
    }

    #[test]
    fn paths_pack_twenty_per_directory() {
        assert_eq!(pdb_path(1), "/PIONEER/Artwork/00001/a1.jpg");
        assert_eq!(pdb_path(20), "/PIONEER/Artwork/00001/a20.jpg");
        assert_eq!(pdb_path(21), "/PIONEER/Artwork/00002/a21.jpg");
        assert_eq!(dlp_path(21), "/PIONEER/Artwork/00002/b21.jpg");
    }

    #[test]
    fn writes_four_files() {
        let dir = std::env::temp_dir().join(format!("ordnung-art-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = ArtworkStore::default();
        store.intern(&test_png());
        let out = write_files(&dir, &store.files[0]).unwrap();
        for name in ["a1.jpg", "a1_m.jpg", "b1.jpg", "b1_m.jpg"] {
            assert!(out.join(name).is_file(), "{name} missing");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
