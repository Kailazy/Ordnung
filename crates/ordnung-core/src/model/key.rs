//! Musical key: canonical storage + Camelot / Open Key / classical rendering.
//!
//! Camelot is the display contract (see the `audio-analysis` skill). Keys are
//! stored canonically as `(PitchClass, Mode)` and rendered on demand. The Camelot
//! number and Open Key number are derived, never stored.

/// Pitch class, C = 0 .. B = 11 (chromatic, sharps preferred internally).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PitchClass(pub u8);

impl PitchClass {
    pub fn new(semitone: u8) -> Self {
        PitchClass(semitone % 12)
    }

    /// Classical name (sharp spelling).
    pub fn name(self) -> &'static str {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        NAMES[(self.0 % 12) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Major,
    Minor,
}

/// A musical key. Canonical: pitch class of the tonic + mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub tonic: PitchClass,
    pub mode: Mode,
}

/// A Camelot wheel position: number 1..=12 and side (A = minor, B = major).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camelot {
    pub number: u8,
    pub major: bool,
}

impl Camelot {
    /// "8B", "5A", etc.
    pub fn label(self) -> String {
        format!("{}{}", self.number, if self.major { 'B' } else { 'A' })
    }

    /// Harmonic-mixing compatibility between two wheel positions: same number
    /// (same key or relative major/minor) or one step around the wheel on the
    /// same side.
    pub fn compatible_with(self, other: Camelot) -> bool {
        if self.number == other.number {
            return true;
        }
        if self.major == other.major {
            let diff = (self.number as i8 - other.number as i8).rem_euclid(12);
            return diff == 1 || diff == 11;
        }
        false
    }
}

impl Key {
    pub fn new(tonic: PitchClass, mode: Mode) -> Self {
        Key { tonic, mode }
    }

    /// Camelot number for a *major* pitch class: stepping up a fifth (+7 semitones)
    /// advances the number by one; C major = 8B.
    fn major_number(pc: u8) -> u8 {
        // fifths-from-C = pc * 7 (mod 12), since 7 is its own inverse mod 12.
        ((7 + (pc % 12) * 7) % 12) + 1
    }

    /// Camelot wheel position.
    pub fn camelot(self) -> Camelot {
        match self.mode {
            Mode::Major => Camelot {
                number: Self::major_number(self.tonic.0),
                major: true,
            },
            // A minor key shares its Camelot number with its relative major,
            // whose tonic is a minor third (+3 semitones) up.
            Mode::Minor => Camelot {
                number: Self::major_number((self.tonic.0 + 3) % 12),
                major: false,
            },
        }
    }

    /// Open Key number 1..=12 with 'd' (major) / 'm' (minor). C major = 1d.
    pub fn open_key(self) -> String {
        let c = self.camelot();
        let open = ((c.number + 12 - 8) % 12) + 1; // shift so Camelot 8 -> Open Key 1
        format!("{}{}", open, if c.major { 'd' } else { 'm' })
    }

    /// Classical name, e.g. "Am", "C", "F#m".
    pub fn classical(self) -> String {
        match self.mode {
            Mode::Major => self.tonic.name().to_string(),
            Mode::Minor => format!("{}m", self.tonic.name()),
        }
    }

    /// Default display: Camelot label.
    pub fn display(self) -> String {
        self.camelot().label()
    }

    /// Harmonic-mixing compatibility: same key, ±1 on the wheel, or relative
    /// major/minor (same number, opposite side). See [`Camelot::compatible_with`].
    pub fn compatible_with(self, other: Key) -> bool {
        self.camelot().compatible_with(other.camelot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: u8, mode: Mode) -> Key {
        Key::new(PitchClass::new(name), mode)
    }

    #[test]
    fn camelot_reference_points() {
        // C major = 8B, A minor = 8A (relative), G major = 9B, E minor = 9A.
        assert_eq!(key(0, Mode::Major).camelot().label(), "8B");
        assert_eq!(key(9, Mode::Minor).camelot().label(), "8A");
        assert_eq!(key(7, Mode::Major).camelot().label(), "9B");
        assert_eq!(key(4, Mode::Minor).camelot().label(), "9A");
        // A major = 11B, F# minor = 11A.
        assert_eq!(key(9, Mode::Major).camelot().label(), "11B");
        assert_eq!(key(6, Mode::Minor).camelot().label(), "11A");
    }

    #[test]
    fn camelot_numbers_cover_one_to_twelve() {
        let mut seen = std::collections::BTreeSet::new();
        for pc in 0..12 {
            seen.insert(key(pc, Mode::Major).camelot().number);
        }
        assert_eq!(seen.len(), 12, "all 12 Camelot numbers present for major keys");
        assert!(seen.iter().all(|&n| (1..=12).contains(&n)));
    }

    #[test]
    fn open_key_mapping() {
        assert_eq!(key(0, Mode::Major).open_key(), "1d"); // C major
        assert_eq!(key(9, Mode::Minor).open_key(), "1m"); // A minor
        assert_eq!(key(7, Mode::Major).open_key(), "2d"); // G major
    }

    #[test]
    fn classical_names() {
        assert_eq!(key(9, Mode::Minor).classical(), "Am");
        assert_eq!(key(0, Mode::Major).classical(), "C");
        assert_eq!(key(6, Mode::Minor).classical(), "F#m");
    }

    #[test]
    fn harmonic_compatibility() {
        let am = key(9, Mode::Minor); // 8A
        assert!(am.compatible_with(key(0, Mode::Major))); // relative major C (8B)
        assert!(am.compatible_with(key(4, Mode::Minor))); // 9A, +1
        assert!(am.compatible_with(key(2, Mode::Minor))); // 7A, -1
        assert!(!am.compatible_with(key(6, Mode::Minor))); // 11A, not adjacent
    }

    #[test]
    fn camelot_compatibility_wraps_and_respects_side() {
        let c = |number: u8, major: bool| Camelot { number, major };
        assert!(c(12, false).compatible_with(c(1, false))); // wheel wraps 12 -> 1
        assert!(c(1, false).compatible_with(c(12, false)));
        assert!(c(8, false).compatible_with(c(8, true))); // relative major/minor
        assert!(!c(8, false).compatible_with(c(9, true))); // +1 across sides
        assert!(!c(3, true).compatible_with(c(5, true))); // two steps away
    }
}
