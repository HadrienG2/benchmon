use chrono::{
    format::{Item, StrftimeItems},
    DateTime, Datelike, TimeZone,
};

use crate::format;

use std::fmt::Display;

/// Maximum year that we allow ourselves to support in date formatting
///
/// The code of this module can technically support any Gregorian year that
/// `chrono::DateTime` can handle. However, doing so would make us unnecessarily
/// pessimistic in our table's date column width upper bound. So we make the
/// arguably reasonable assumption that this software will not be in use in a
/// couple thousand years and 4-digit years are enough.
///
/// If you are from the far future and this assumption turned out to be
/// incorrect, please adjust the following constant.
///
const MAX_SUPPORTED_YEAR: i32 = 9999;

/// Strftime-style time column formatting
pub struct Formatter {
    /// Decoded version of the format string
    owned_items: Box<[Item<'static>]>,

    /// Cached max output width expected from the format string
    max_output_width: usize,
}

impl Formatter {
    /// Construct a time formatter from a format string following `chrono`'s
    /// flavor of strftime date/time format syntax.
    ///
    /// The input format string must only contain elements which have a maximum
    /// width that can be computed at compile time. This noticeably excludes
    /// timezone names, which can be arbitrarily large depending on what your
    /// system's timezone database contains.
    ///
    pub fn new(s: &str) -> Self {
        // Parse the format string and compute an owned version of the results
        let owned_items = StrftimeItems::new(s)
            .map(|item: Item<'_>| -> Item<'static> {
                let into_box_str = |s: &str| s.to_owned().into_boxed_str();
                match item {
                    Item::Literal(l) => Item::OwnedLiteral(into_box_str(l)),
                    Item::Space(s) => Item::OwnedSpace(into_box_str(s)),

                    // FIXME: In an ideal world, I'd be able to just
                    //        `other => other` here, but instead it seems I must
                    //        enumerate all remaining cases for the borrow
                    //        checker to understand what I'm doing.
                    Item::OwnedLiteral(o) => Item::OwnedLiteral(o),
                    Item::OwnedSpace(o) => Item::OwnedSpace(o),
                    Item::Numeric(n, p) => Item::Numeric(n, p),
                    Item::Fixed(f) => Item::Fixed(f),
                    Item::Error => Item::Error,
                }
            })
            .collect::<Box<[_]>>();

        // Compute the maximal width of formatted time produced using this
        // format string (in grapheme clusters), panic if there is no maximum or
        // the format string did not parse.
        let max_output_width = owned_items
            .iter()
            .map(max_item_width)
            .sum::<usize>()
            .max(format::str_width(Self::TITLE));

        // Return the result
        Self {
            owned_items,
            max_output_width,
        }
    }

    /// Title of the column in tabular output
    const TITLE: &'static str = "time";

    /// Display the title of a column of results
    pub fn display_title(&self) -> impl Display {
        format::display_col_header(Self::TITLE, self.max_output_width)
    }

    /// Display a time point within a column of results
    pub fn display_data<Tz>(&self, date_time: DateTime<Tz>) -> impl Display + '_
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        assert!(date_time.year() <= MAX_SUPPORTED_YEAR);
        format::display_col_data(
            date_time.format_with_items(self.owned_items.iter()),
            self.max_output_width,
        )
    }

    /// Indicate the width of the output column in grapheme clusters
    #[allow(unused)]
    pub fn output_width(&self) -> usize {
        self.max_output_width
    }
}

/// Given a parsed `chrono` format string item, return an upper bound on the
/// amount of grapheme clusters (~ characters) that will be printed upon
/// printing a date/time using this format, if one exists.
///
/// If there is no upper bound, or if the input is more generally unsuitable for
/// tabular output, panic with a clear error message.
///
fn max_item_width(item: &Item) -> usize {
    let space_width = |space: &str| -> usize {
        for ch in space.chars() {
            if let 10 | 11 | 12 | 13 | 133 | 8232 | 8233 = ch as u32 {
                panic!("Line breaks are not acceptable in tabular output");
            }
        }
        format::str_width(space)
    };

    use chrono::format::{Fixed, Numeric};
    match item {
        Item::Literal(l) => format::str_width(l),
        Item::OwnedLiteral(ol) => format::str_width(&ol),

        Item::Space(s) => space_width(s),
        Item::OwnedSpace(os) => space_width(&os),

        Item::Numeric(numeric, _pad) => {
            let digits = |number: u64| (number as f32).log10().ceil() as usize;
            let max_supported_year_digits = digits(MAX_SUPPORTED_YEAR as u64);

            match numeric {
                Numeric::Year | Numeric::IsoYear => {
                    // Per RFC 8601, year 10k+ will need an explicit sign
                    let sign_length = (MAX_SUPPORTED_YEAR > 10_000) as usize;
                    max_supported_year_digits + sign_length
                }

                Numeric::YearDiv100 | Numeric::IsoYearDiv100 => max_supported_year_digits - 2,

                Numeric::YearMod100
                | Numeric::IsoYearMod100
                | Numeric::Month
                | Numeric::Day
                | Numeric::WeekFromSun
                | Numeric::WeekFromMon
                | Numeric::IsoWeek
                | Numeric::Hour
                | Numeric::Hour12
                | Numeric::Minute
                | Numeric::Second => 2,

                Numeric::NumDaysFromSun | Numeric::WeekdayFromMon => 1,

                // Day of year
                Numeric::Ordinal => 3,

                Numeric::Nanosecond => 9,

                Numeric::Timestamp => {
                    let max_unix_year = (MAX_SUPPORTED_YEAR - 1970) as f32;
                    let max_timestamp = max_unix_year * 365.25 * 24.0 * 3600.0;
                    digits(max_timestamp as u64)
                }

                // Internal chrono stuff, shouldn't pop up in normal formatting
                Numeric::Internal(_internal) => unreachable!(),
            }
        }

        Item::Fixed(fixed) => {
            let max_str_width = |strs: &[&str]| -> usize {
                strs.iter().cloned().map(format::str_width).max().unwrap()
            };
            let max_format_width = |format: &str| {
                StrftimeItems::new(format)
                    .map(|item| max_item_width(&item))
                    .sum()
            };

            match fixed {
                Fixed::ShortMonthName | Fixed::ShortWeekdayName => 3,

                Fixed::LongMonthName => {
                    // NOTE: chrono is English-only for now, we may need to stop
                    //       supporting this format if chrono ever starts
                    //       supporting month name localization.
                    const MONTH_NAMES: [&'static str; 12] = [
                        "January",
                        "February",
                        "March",
                        "April",
                        "May",
                        "June",
                        "July",
                        "August",
                        "September",
                        "October",
                        "November",
                        "December",
                    ];
                    max_str_width(&MONTH_NAMES[..])
                }

                Fixed::LongWeekdayName => {
                    // NOTE: chrono is English-only for now, we may need to stop
                    //       supporting this format if chrono ever starts
                    //       supporting day name localization.
                    const WEEKDAY_NAMES: [&'static str; 7] = [
                        "Monday",
                        "Tuesday",
                        "Wednesday",
                        "Thursday",
                        "Friday",
                        "Saturday",
                        "Sunday",
                    ];
                    max_str_width(&WEEKDAY_NAMES[..])
                }

                Fixed::LowerAmPm | Fixed::UpperAmPm => 2,

                Fixed::Nanosecond => 10,
                Fixed::Nanosecond3 => 4,
                Fixed::Nanosecond6 => 7,
                Fixed::Nanosecond9 => 10,

                Fixed::TimezoneName => panic!(
                    "Timezone names are not supported as tabular output \
                     because their length is unbounded"
                ),

                Fixed::TimezoneOffsetColon | Fixed::TimezoneOffsetColonZ => 6,

                Fixed::TimezoneOffset | Fixed::TimezoneOffsetZ => 5,

                Fixed::RFC2822 => {
                    const RFC2822: &'static str = "%a, %e %b %Y %H:%M:%S %z";
                    max_format_width(RFC2822)
                }

                Fixed::RFC3339 => {
                    const RFC3339: &'static str = "%Y-%m-%dT%H:%M:%S%.f%:z";
                    max_format_width(RFC3339)
                }

                // Internal chrono stuff, shouldn't pop up in normal formatting
                Fixed::Internal(_internal) => unreachable!(),
            }
        }

        Item::Error => panic!("Input time format string is invalid!"),
    }
}
