//! Terminal rendering helpers: tables, human sizes, progress lines.

use std::io::{IsTerminal, Write};

use flipper_companion_core::Progress;
use flipper_companion_dfu::{FlashProgress, FlashStage};

/// Prints a left-aligned table with a separator line.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();

    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(cell.chars().count());
            }
        }
    }

    println!("{}", format_row(headers, &widths));
    println!(
        "{}",
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  ")
    );

    for row in rows {
        println!("{}", format_row(row, &widths));
    }
}

fn format_row(cells: &[impl AsRef<str>], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let width = widths.get(index).copied().unwrap_or(0);
            format!("{:<width$}", cell.as_ref(), width = width)
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// Formats a byte count with binary units.
#[must_use]
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

    if value < 1024 {
        return format!("{value} B");
    }

    let mut scaled = value as f64;
    let mut unit = 0;

    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }

    format!("{scaled:.1} {}", UNITS[unit])
}

/// Formats a Unix timestamp as `dd-mm-yyyy`.
#[must_use]
pub fn date_from_timestamp(timestamp: u64) -> String {
    let (year, month, day) = civil_from_days((timestamp / 86_400) as i64);
    format!("{day:02}-{month:02}-{year:04}")
}

/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;

    (year + i64::from(month <= 2), month, day)
}

/// Asks for confirmation, unless `assume_yes` is set.
///
/// Refuses to continue when stdin is not a terminal: destructive operations
/// must never be silently approved by a script.
pub fn confirm(prompt: &str, assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }

    if !std::io::stdin().is_terminal() {
        eprintln!("error: refusing to continue without --yes in a non-interactive session");
        return false;
    }

    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }

    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// A single-line, in-place progress indicator.
///
/// It stays silent when stderr is not a terminal, which keeps piped output
/// clean.
pub struct LineProgress {
    label: String,
    last_percent: i32,
    enabled: bool,
}

impl LineProgress {
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            last_percent: -1,
            enabled: std::io::stderr().is_terminal(),
        }
    }

    pub fn set(&mut self, progress: Progress) {
        if let Some(fraction) = progress.fraction_value() {
            self.set_fraction(fraction);
        }
    }

    pub fn set_fraction(&mut self, fraction: f32) {
        if !self.enabled {
            return;
        }

        let percent = (fraction.clamp(0.0, 1.0) * 100.0).round() as i32;
        if percent == self.last_percent {
            return;
        }

        self.last_percent = percent;
        eprint!("\r{}: {percent:>3}%", self.label);
        let _ = std::io::stderr().flush();
    }

    /// Maps the two DFU phases onto a single 0–100 % bar.
    pub fn dfu(&mut self, progress: FlashProgress) {
        let fraction = progress.fraction();

        match progress.stage {
            FlashStage::Erasing => self.set_fraction(fraction * 0.5),
            FlashStage::Writing => self.set_fraction(0.5 + fraction * 0.5),
        }
    }

    pub fn finish(&mut self) {
        if self.enabled {
            eprintln!("\r{}: 100%", self.label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_binary_sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn converts_timestamps_to_dates() {
        assert_eq!(date_from_timestamp(0), "01-01-1970");
        assert_eq!(date_from_timestamp(1_791_000_000), "03-10-2026");
    }
}
