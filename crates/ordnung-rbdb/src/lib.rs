//! rekordbox/CDJ export.
//!
//! Writes a native USB layout: `/CONTENTS`, `/PIONEER/rekordbox/export.pdb`, and
//! per-track ANLZ `.DAT`/`.EXT` files. Implemented in Phase 5 on top of
//! `rekordcrate`. All format knowledge and invariants live in the
//! `rekordbox-format` skill — consult it before touching this crate.
//!
//! Self-contained writers (`pdbw`, `anlz`, `dlp`, `export`) validated against
//! the EYEBAGS golden dissection; `golden` diffs our output against a
//! rekordbox-made export field by field.

pub mod anlz;
pub(crate) mod artwork;
pub mod dlp;
pub mod edit;
pub mod export;
pub mod golden;
pub mod pdb;
mod pdbw;
