//! Query and display CPU frequency information

use heim::units::Frequency;

/// CPU clock frequency column formatting
pub struct Formatter {
    /// Minimal CPU frequency, if known
    min: Option<Frequency>,

    /// Maximal CPU frequency, if known
    max: Option<Frequency>,
}

// TODO: Implement this
