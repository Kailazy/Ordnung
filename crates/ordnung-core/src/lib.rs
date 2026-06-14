//! Ordnung core: domain model and engines.
//!
//! This crate is a pure library — no UI, no policy, no process I/O decisions.
//! See the `ordnung-architecture` skill for the rules that keep it that way.
//!
//! Phase 0 ships the domain model and a fully-tested key/Camelot module. Engines
//! (`scan`, `analysis`, `tag`, `convert`, `catalog`) arrive in later phases.

pub mod analysis;
pub mod catalog;
pub mod convert;
pub mod discogs;
pub mod error;
pub mod model;
pub mod scan;
pub mod tag;

pub use catalog::{
    best_copy_index, Catalog, DuplicateGroup, DuplicateKind, MissingArtwork, MissingTrack,
    ScannedTrack,
};
pub use error::{Error, Result};
pub use model::key::{Camelot, Key, Mode, PitchClass};
pub use model::{AudioProperties, Format, Playlist, Tags, Track, VinylRecord};
