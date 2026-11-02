//! Number formats for the grid and Explore: a column's values shown as money, a percentage,
//! a rounded count, instead of every digit the engine sent. The value itself is untouched
//! (sorting, filtering, totals, exports see the number); only the text changes.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberFormat {
    /// Thousands separators and a fixed number of decimals: `1,234.56`.
    Number { decimals: u8 },
    /// A leading currency sign: `$1,234.56`.
    Currency { decimals: u8 },
    /// The value times 100 with a percent sign: `12.3%`.
    Percent { decimals: u8 },
    /// `1.2K`, `3.4M`, `5.6B`, `7.8T`.
    Compact,
}

impl NumberFormat {
    /// The presets menus offer, with their example text.
    pub const PRESETS: [NumberFormat; 7] = [
        NumberFormat::Number { decimals: 0 },
        NumberFormat::Number { decimals: 1 },
        NumberFormat::Number { decimals: 2 },
        NumberFormat::Currency { decimals: 2 },
        NumberFormat::Currency { decimals: 0 },
        NumberFormat::Percent { decimals: 1 },
        NumberFormat::Compact,
    ];

    /// The menu label: the format shown on an example value.
    pub fn label(self) -> String {
        match self {
            NumberFormat::Percent { .. } => self.format(0.1234),
            NumberFormat::Compact => "1.2M".to_string(),
            _ => self.format(1234.5678),
        }
    }

    /// A short key for the agent and settings: `number2`, `currency0`, `percent1`, `compact`.
    pub fn key(self) -> String {
        match self {
            NumberFormat::Number { decimals } => format!("number{decimals}"),
            NumberFormat::Currency { decimals } => format!("currency{decimals}"),
            NumberFormat::Percent { decimals } => format!("percent{decimals}"),
            NumberFormat::Compact => "compact".into(),
        }
    }

    /// `number`, `number0`…`number6`, `integer`, `currency[n]`, `percent[n]`, `compact`.
    pub fn parse(s: &str) -> Option<NumberFormat> {
        let s = s.trim().to_ascii_lowercase();
        let (word, digits): (String, String) = s.chars().partition(|c| !c.is_ascii_digit());
        let d = digits.parse::<u8>().ok().map(|d| d.min(6));
        Some(match word.as_str() {
            "number" | "thousands" | "fixed" => NumberFormat::Number { decimals: d.unwrap_or(2) },
            "integer" | "int" => NumberFormat::Number { decimals: 0 },
            "currency" | "money" => NumberFormat::Currency { decimals: d.unwrap_or(2) },
            "percent" | "pct" => NumberFormat::Percent { decimals: d.unwrap_or(1) },
            "compact" | "short" => NumberFormat::Compact,
            _ => return None,
        })
    }

    /// A format from a DAX / Excel format string (`#,0.00`, `$#,0`, `0.0%`, `"Currency"`,
    /// `"Standard"`), as semantic models carry on measures and columns; None when it says
    /// nothing a preset covers (dates, text, `General`).
    pub fn from_format_string(s: &str) -> Option<NumberFormat> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let first = s.split(';').next().unwrap_or(s);
        match first.to_ascii_lowercase().as_str() {
            "general number" | "general" => return None,
            "standard" | "fixed" => return Some(NumberFormat::Number { decimals: 2 }),
            "currency" => return Some(NumberFormat::Currency { decimals: 2 }),
            "percent" => return Some(NumberFormat::Percent { decimals: 2 }),
            _ => {}
        }
        if first.contains(['y', 'm', 'd', 'h', 's', 'Y', 'M', 'D', 'H', 'S']) && !first.contains(['#', '0']) {
            return None;
        }
        if !first.contains(['#', '0']) {
            return None;
        }
        let decimals = first.find('.').map(|dot| first[dot + 1..].chars().take_while(|c| *c == '0' || *c == '#').count().min(6) as u8).unwrap_or(0);
        Some(if first.contains('%') {
            NumberFormat::Percent { decimals }
        } else if first.contains(['$', '€', '£', '¥']) {
            NumberFormat::Currency { decimals }
        } else {
            NumberFormat::Number { decimals }
        })
    }

    /// The text for a value.
    pub fn format(self, v: f64) -> String {
        if v.is_nan() {
            return "NaN".into();
        }
        if v.is_infinite() {
            return if v > 0.0 { "∞".into() } else { "-∞".into() };
        }
        match self {
            NumberFormat::Number { decimals } => fixed(v, decimals, "", ""),
            NumberFormat::Currency { decimals } => fixed(v, decimals, "$", ""),
            NumberFormat::Percent { decimals } => fixed(v * 100.0, decimals, "", "%"),
            NumberFormat::Compact => {
                let a = v.abs();
                let (scaled, suffix) = if a >= 1e12 {
                    (v / 1e12, "T")
                } else if a >= 1e9 {
                    (v / 1e9, "B")
                } else if a >= 1e6 {
                    (v / 1e6, "M")
                } else if a >= 1e3 {
                    (v / 1e3, "K")
                } else {
                    return fixed(v, if v.fract() == 0.0 { 0 } else { 2 }, "", "");
                };
                let s = format!("{:.1}", (scaled * 10.0).round() / 10.0);
                let s = s.strip_suffix(".0").map(str::to_string).unwrap_or(s);
                format!("{s}{suffix}")
            }
        }
    }
}

/// `-$1,234.56%`-style assembly: sign, prefix, grouped integer digits, decimals, suffix.
fn fixed(v: f64, decimals: u8, prefix: &str, suffix: &str) -> String {
    // half away from zero (1,234.5 → 1,235), not the formatter's half to even
    let p = 10f64.powi(decimals as i32);
    let rounded = (v.abs() * p).round() / p;
    let s = format!("{:.*}", decimals as usize, rounded);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s.as_str(), None),
    };
    let mut out = String::with_capacity(s.len() + 8);
    // a value that rounds to zero carries no sign
    if v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
        out.push('-');
    }
    out.push_str(prefix);
    out.push_str(&group(int));
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out.push_str(suffix);
    out
}

/// `1234567` → `1,234,567`.
fn group(digits: &str) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_format() {
        assert_eq!(NumberFormat::Number { decimals: 2 }.format(3966531807.239975), "3,966,531,807.24");
        assert_eq!(NumberFormat::Number { decimals: 0 }.format(-1234.5), "-1,235");
        assert_eq!(NumberFormat::Currency { decimals: 2 }.format(-0.001), "$0.00");
        assert_eq!(NumberFormat::Currency { decimals: 2 }.format(-12.5), "-$12.50");
        assert_eq!(NumberFormat::Percent { decimals: 1 }.format(0.1234), "12.3%");
        assert_eq!(NumberFormat::Compact.format(3966531807.0), "4B");
        assert_eq!(NumberFormat::Compact.format(1_250_000.0), "1.3M");
        assert_eq!(NumberFormat::Compact.format(950.25), "950.25");
        assert_eq!(NumberFormat::Number { decimals: 2 }.format(f64::NAN), "NaN");
        assert_eq!(NumberFormat::Number { decimals: 1 }.label(), "1,234.6");
    }

    #[test]
    fn keys_round_trip() {
        for p in NumberFormat::PRESETS {
            assert_eq!(NumberFormat::parse(&p.key()), Some(p));
        }
        assert_eq!(NumberFormat::parse("integer"), Some(NumberFormat::Number { decimals: 0 }));
        assert_eq!(NumberFormat::parse("money"), Some(NumberFormat::Currency { decimals: 2 }));
        assert_eq!(NumberFormat::parse("nope"), None);
    }

    #[test]
    fn dax_format_strings() {
        assert_eq!(NumberFormat::from_format_string("$#,0"), Some(NumberFormat::Currency { decimals: 0 }));
        assert_eq!(NumberFormat::from_format_string("\\$#,0.00;(\\$#,0.00);\\$#,0.00"), Some(NumberFormat::Currency { decimals: 2 }));
        assert_eq!(NumberFormat::from_format_string("#,0.00"), Some(NumberFormat::Number { decimals: 2 }));
        assert_eq!(NumberFormat::from_format_string("0.0%;-0.0%;0.0%"), Some(NumberFormat::Percent { decimals: 1 }));
        assert_eq!(NumberFormat::from_format_string("#,0"), Some(NumberFormat::Number { decimals: 0 }));
        assert_eq!(NumberFormat::from_format_string("General Number"), None);
        assert_eq!(NumberFormat::from_format_string("yyyy-mm-dd"), None);
        assert_eq!(NumberFormat::from_format_string("Currency"), Some(NumberFormat::Currency { decimals: 2 }));
        assert_eq!(NumberFormat::from_format_string(""), None);
    }
}
