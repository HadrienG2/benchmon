use heim::units::{information::byte, Information};

use std::fmt;

use unicode_segmentation::UnicodeSegmentation;

/// Delay the display of something until we know what it should be displayed to
///
/// This allows us to support all of `write!`, `print!` and `format!` without
/// unnecessary memory allocations or error-handling boilerplate.
///
struct DelayedDisplay<DisplayFn: Fn(&mut fmt::Formatter<'_>) -> fmt::Result>(DisplayFn);

impl<DisplayFn> fmt::Display for DelayedDisplay<DisplayFn>
where
    DisplayFn: Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0(f)
    }
}

/// Display the header of a column of measurements
pub fn display_col_header(text: &str, width: usize) -> impl fmt::Display + '_ {
    DelayedDisplay(move |dest| write!(dest, "{0:─^1$}", text, width))
}

pub const COL_HEADER_SEPARATOR: char = '┼';

/// Display a measurement within a column
pub fn display_col_data(data: impl fmt::Display, width: usize) -> impl fmt::Display {
    DelayedDisplay(move |dest| write!(dest, "{0:1$}", data, width))
}

pub const COL_DATA_SEPARATOR: char = '│';

/// Display a quantity of information from heim
pub fn display_information(quantity: Information) -> impl fmt::Display {
    DelayedDisplay(move |dest| {
        // Get the quantity of information in bytes
        let bytes = quantity.get::<byte>();

        // Check that quantity's order of magnitude
        let magnitude = if bytes > 0 {
            (bytes as f64).log10().trunc() as u8
        } else {
            0
        };

        // General recipe for printing fractional SI information quantities
        let write_si = |dest: &mut fmt::Formatter<'_>, unit_magnitude, unit| {
            let base = 10_u64.pow(unit_magnitude);
            let integral_part = bytes / base;
            let fractional_part = (bytes / (base / 1000)) % 1000;
            write!(dest, "{}.{:03} {}", integral_part, fractional_part, unit)
        };

        // Select the right recipe depending on the order of magnitude
        match magnitude {
            0..=2 => write!(dest, "{} B", bytes),
            3..=5 => write_si(dest, 3, "kB"),
            6..=8 => write_si(dest, 6, "MB"),
            9..=11 => write_si(dest, 9, "GB"),
            _ => write_si(dest, 12, "TB"),
        }
    })
}

/// Compute the width of a string in grapheme clusters
///
/// This should roughly match the number of terminal columns that this string
/// will occupy when printed to stdout.
///
pub fn str_width(s: &str) -> usize {
    s.graphemes(true).count()
}
