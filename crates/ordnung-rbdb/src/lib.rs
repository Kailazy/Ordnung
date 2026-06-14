//! rekordbox/CDJ export.
//!
//! Writes a native USB layout: `/CONTENTS`, `/PIONEER/rekordbox/export.pdb`, and
//! per-track ANLZ `.DAT`/`.EXT` files. Implemented in Phase 5 on top of
//! `rekordcrate`. All format knowledge and invariants live in the
//! `rekordbox-format` skill — consult it before touching this crate.
//!
//! Phase 0: module skeleton only.

pub mod pdb;
pub mod anlz;
