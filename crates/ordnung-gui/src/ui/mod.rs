//! Ordnung's visual component library.
//!
//! [`tokens`] holds the design tokens (colours, radii, spacing, type ramp) and
//! [`theme`] pushes them into egui's global style. Bespoke component helpers will
//! live alongside these in a later pass.

pub mod hover;
pub mod theme;
// Tokens are an intentionally ahead-of-use palette: Pass 1 wires only a subset
// into the global style; the rest are consumed as call sites migrate off inline
// literals. Allow the interim dead-code until that pass lands.
#[allow(dead_code)]
pub mod tokens;
