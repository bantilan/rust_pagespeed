//! HTML colors (`htmlcolor.h`, `htmlcolor.cc`, system colors in
//! `util.cc`).
//!
//! Only the surface the CSS parser and minifier use is ported: hex
//! and named-color parsing plus the CSS shorthand selection. HSL,
//! similarity, luminance, and adjustment helpers have no callers on
//! these paths and are deferred.
#![forbid(unsafe_code)]

#[path = "gen/colors_gen.rs"]
mod generated;

use generated::{NAMED_COLORS, SYSTEM_COLORS};

/// Case-insensitive ASCII comparison (`memcasecmp`/`CaseEqual`).
fn ascii_case_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Looks up a CSS/SVG color name (`GetKnownColorValue`): names of 3+
/// bytes, ASCII case-insensitive.
fn known_color_value(name: &[u8]) -> Option<(u8, u8, u8)> {
    if name.len() <= 2 {
        return None;
    }
    NAMED_COLORS
        .iter()
        .find(|(known, _)| ascii_case_equal(known.as_bytes(), name))
        .map(|(_, rgb)| *rgb)
}

/// Looks up a CSS2 system color (`GetKnownSystemColorValue`).
fn known_system_color_value(name: &str) -> Option<(u8, u8, u8)> {
    SYSTEM_COLORS
        .iter()
        .find(|(known, _)| ascii_case_equal(known.as_bytes(), name.as_bytes()))
        .map(|(_, rgb)| *rgb)
}

/// Resolves a system color name (`GetSystemColor`).
#[must_use]
pub fn system_color(name: &str) -> Option<HtmlColor> {
    known_system_color_value(name).map(|(r, g, b)| HtmlColor::rgb(r, g, b))
}

/// An HTML color (`HtmlColor`): defined RGB or a bad-value marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlColor {
    r: u8,
    g: u8,
    b: u8,
    /// 0 = good; 1 = bad name; 2 = bad hex (`is_bad_value_`).
    bad: u8,
}

impl HtmlColor {
    /// Builds from RGB values (`SetValueFromRGB` / `(r, g, b)`).
    #[must_use]
    pub fn rgb(r: u8, g: u8, b: u8) -> HtmlColor {
        HtmlColor { r, g, b, bad: 0 }
    }

    /// Undefined color (bad name).
    #[must_use]
    pub fn undefined() -> HtmlColor {
        HtmlColor {
            r: 0,
            g: 0,
            b: 0,
            bad: 1,
        }
    }

    /// Const undefined color (for static fallbacks).
    #[must_use]
    pub const fn const_undefined() -> HtmlColor {
        HtmlColor {
            r: 0,
            g: 0,
            b: 0,
            bad: 1,
        }
    }

    /// Whether the color is defined (`IsDefined`).
    #[must_use]
    pub fn is_defined(&self) -> bool {
        self.bad == 0
    }

    /// Parses `xxxxxx` or shorthand `xxx` hex (`SetValueFromHexStr`).
    #[must_use]
    pub fn from_hex(hex: &[u8]) -> HtmlColor {
        let mut expanded = [0u8; 6];
        let digits: &[u8] = if hex.len() == 3 {
            for (index, byte) in hex.iter().enumerate() {
                if !byte.is_ascii_hexdigit() {
                    return HtmlColor::bad_hex();
                }
                expanded[2 * index] = *byte;
                expanded[2 * index + 1] = *byte;
            }
            &expanded
        } else if hex.len() == 6 {
            if !hex.iter().all(|byte| byte.is_ascii_hexdigit()) {
                return HtmlColor::bad_hex();
            }
            hex
        } else {
            return HtmlColor::bad_hex();
        };
        HtmlColor {
            r: hex_pair(digits[0], digits[1]),
            g: hex_pair(digits[2], digits[3]),
            b: hex_pair(digits[4], digits[5]),
            bad: 0,
        }
    }

    /// Parses a color name (`SetValueFromName`).
    #[must_use]
    pub fn from_name(name: &[u8]) -> HtmlColor {
        known_color_value(name)
            .map_or_else(HtmlColor::undefined, |(r, g, b)| HtmlColor::rgb(r, g, b))
    }

    fn bad_hex() -> HtmlColor {
        HtmlColor {
            r: 0,
            g: 0,
            b: 0,
            bad: 2,
        }
    }

    /// Red channel (`r()`).
    #[must_use]
    pub fn r(&self) -> u8 {
        self.r
    }

    /// Green channel (`g()`).
    #[must_use]
    pub fn g(&self) -> u8 {
        self.g
    }

    /// Blue channel (`b()`).
    #[must_use]
    pub fn b(&self) -> u8 {
        self.b
    }

    /// Packed `0xRRGGBB` (`rgb()`).
    #[must_use]
    pub fn rgb_value(&self) -> u32 {
        u32::from(self.b) | u32::from(self.g) << 8 | u32::from(self.r) << 16
    }

    /// `#rrggbb` rendering (`ToString`).
    #[must_use]
    pub fn to_hex_string(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Debug serialization (`HtmlColor::ToString`, bytes).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        self.to_hex_string().into_bytes()
    }

    /// Defined colors compare by RGB; two undefined colors are equal
    /// (`Equals`).
    #[must_use]
    pub fn equals(&self, other: &HtmlColor) -> bool {
        if self.is_defined() != other.is_defined() {
            return false;
        }
        if self.is_defined() {
            return self.rgb_value() == other.rgb_value();
        }
        true
    }

    /// Shortest CSS representation (`MaybeConvertToCssShorthand`): a
    /// short color name, `#rgb`, or `#rrggbb`. The caller must only
    /// pass defined colors (as `CssMinify` does after its CHECK).
    #[must_use]
    pub fn to_css_shorthand(&self) -> String {
        match self.rgb_value() {
            0x000080 => return "navy".to_string(),
            0x008000 => return "green".to_string(),
            0x008080 => return "teal".to_string(),
            0x800000 => return "maroon".to_string(),
            0x800080 => return "purple".to_string(),
            0x808000 => return "olive".to_string(),
            0x808080 => return "gray".to_string(),
            0xC0C0C0 => return "silver".to_string(),
            0xFF0000 => return "red".to_string(),
            _ => {}
        }
        if self.r >> 4 != self.r & 0xF || self.g >> 4 != self.g & 0xF || self.b >> 4 != self.b & 0xF
        {
            return self.to_hex_string();
        }
        format!(
            "#{:01x}{:01x}{:01x}",
            self.r & 0xF,
            self.g & 0xF,
            self.b & 0xF
        )
    }
}

fn hex_pair(high: u8, low: u8) -> u8 {
    let digit = |byte: u8| {
        if byte.is_ascii_digit() {
            byte - b'0'
        } else {
            byte.to_ascii_lowercase() - b'a' + 10
        }
    };
    digit(high) * 16 + digit(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_vectors() {
        assert_eq!(HtmlColor::from_hex(b"fff").rgb_value(), 0xFFFFFF);
        assert_eq!(HtmlColor::from_hex(b"bcdefa").rgb_value(), 0xBCDEFA);
        assert_eq!(HtmlColor::from_hex(b"ABC").rgb_value(), 0xAABBCC);
        assert!(!HtmlColor::from_hex(b"ff").is_defined());
        assert!(!HtmlColor::from_hex(b"ffff").is_defined());
        assert!(!HtmlColor::from_hex(b"ffg").is_defined());
        assert!(!HtmlColor::from_hex(b"").is_defined());
    }

    #[test]
    fn name_vectors() {
        assert_eq!(HtmlColor::from_name(b"darkgreen").rgb_value(), 0x006400);
        assert_eq!(HtmlColor::from_name(b"DarkGreen").rgb_value(), 0x006400);
        assert_eq!(HtmlColor::from_name(b"RED").rgb_value(), 0xFF0000);
        assert!(!HtmlColor::from_name(b"re").is_defined());
        assert!(!HtmlColor::from_name(b"notacolor").is_defined());
        // Table size guards against extraction drift.
        assert_eq!(NAMED_COLORS.len(), 147);
        assert_eq!(SYSTEM_COLORS.len(), 28);
    }

    #[test]
    fn shorthand_vectors() {
        assert_eq!(HtmlColor::from_hex(b"008000").to_css_shorthand(), "green");
        assert_eq!(HtmlColor::from_hex(b"ff0000").to_css_shorthand(), "red");
        assert_eq!(HtmlColor::from_hex(b"aabbcc").to_css_shorthand(), "#abc");
        assert_eq!(HtmlColor::from_hex(b"006400").to_css_shorthand(), "#006400");
        assert_eq!(HtmlColor::from_hex(b"abc").to_css_shorthand(), "#abc");
    }

    #[test]
    fn system_vectors() {
        let color = system_color("window").expect("window");
        assert_eq!(color.rgb_value(), 0xFFFFFF);
        assert!(system_color("nope").is_none());
    }
}
