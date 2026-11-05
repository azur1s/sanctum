//! Lining lore up to the pixel. Tooltips are as wide as their widest line, so
//! once that is known, `{fill}` and dividers are stretched to match it.

use super::font_widths::{ASCII, OTHER};

/// Tooltips are at least this wide, in pixels, so short items still look right.
const MIN_WIDTH: u32 = 150;
/// The fewest pixels between a stat's name and its value.
const MIN_FILL: u32 = 12;
/// The shortest stretch of line on each side of a divider's title.
const MIN_STRIP: u32 = 12;
/// Spaces that share one color, to keep the item's size down. 8 pixels of
/// gradient at a time looks smooth.
const SPACES_PER_COLOR: usize = 2;
/// Glyphs outside the bitmap fonts fall back to unifont, which has no table
/// here. This is close for most of them.
const UNKNOWN_WIDTH: u32 = 8;

const DOT_WIDTH: u32 = 6;
const SPACE_WIDTH: u32 = 4;

/// Stands in for `{fill}` until the tooltip's width is known.
pub(super) const FILL: char = '\u{E000}';

/// An RGB color.
pub type Rgb = (u8, u8, u8);

/// A line across the tooltip that fades out at both ends, with text in the
/// middle if it has a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divider {
    /// With color codes; may be empty.
    pub title: String,
    pub color: Rgb,
    pub fade: Rgb,
}

impl Divider {
    pub const DEFAULT_COLOR: Rgb = (0x7A, 0x6A, 0x9E);
    pub const DEFAULT_FADE: Rgb = (0x24, 0x1A, 0x33);

    /// The fewest pixels this divider can take.
    pub(super) fn min_width(&self) -> u32 {
        match self.gaps() {
            0 => MIN_STRIP * 2,
            gaps => text_width(&self.title) + gaps + MIN_STRIP * 2,
        }
    }

    /// Space around the title.
    fn gaps(&self) -> u32 {
        if self.title.is_empty() { 0 } else { SPACE_WIDTH * 2 }
    }

    /// The line, `width` pixels wide.
    pub(super) fn render(&self, width: u32) -> String {
        let strips = width.saturating_sub(text_width(&self.title) + self.gaps()).max(MIN_STRIP * 2);
        let left = strips / 2;
        let mut line = strip(left, self.fade, self.color);
        if !self.title.is_empty() {
            // Lore is purple unless told otherwise.
            line.push_str(&format!("&r &7{}&r ", self.title));
        }
        line.push_str(&strip(strips - left, self.color, self.fade));
        line
    }
}

/// The pixels a line of lore takes up, without its color codes.
pub(super) fn text_width(line: &str) -> u32 {
    let mut width = 0;
    let mut bold = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == FILL {
            continue;
        }
        // Lore goes through Pumpkin's legacy parser: it takes whatever follows
        // an `&` as a code, known or not.
        if c == '&' {
            match chars.next().map(|code| code.to_ascii_lowercase()) {
                Some('l') => bold = true,
                Some('r') => bold = false,
                Some(code) if code.is_ascii_hexdigit() => bold = false,
                // A hex color: its six `&R` are not colors of their own, and
                // it leaves bold on.
                Some('x') => {
                    for _ in 0..6 {
                        chars.next();
                        chars.next();
                    }
                }
                _ => {}
            }
            continue;
        }
        width += char_width(c) + u32::from(bold);
    }
    width
}

fn char_width(c: char) -> u32 {
    let code = c as u32;
    if (32..127).contains(&code) {
        return ASCII[code as usize - 32].into();
    }
    match OTHER.binary_search_by(|&(first, last, _)| {
        if code < first {
            std::cmp::Ordering::Greater
        } else if code > last {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(run) => OTHER[run].2.into(),
        Err(_) => UNKNOWN_WIDTH,
    }
}

/// What a stat line needs at least, with `{fill}` as thin as it goes.
pub(super) fn min_line_width(line: &str) -> u32 {
    text_width(line) + if line.contains(FILL) { MIN_FILL } else { 0 }
}

/// How wide a tooltip has to be for these line widths.
pub(super) fn tooltip_width(widths: impl IntoIterator<Item = u32>) -> u32 {
    widths.into_iter().fold(MIN_WIDTH, u32::max)
}

/// Puts dots in place of `{fill}` so the line is `width` pixels wide.
pub(super) fn fill_line(line: &str, width: u32) -> String {
    let dots = fill(width.saturating_sub(text_width(line)));
    line.replacen(FILL, &dots, 1)
}

/// Exactly `pixels` of dim dots, counted from the right so they line up in
/// columns across every line. What the dots leave over goes in front.
fn fill(pixels: u32) -> String {
    let pixels = pixels.max(MIN_FILL);
    // Spaces are 4 pixels and bold ones 5: these are the smallest gaps that
    // leave a multiple of the dot width.
    let gap = [12, 13, 8, 9, 4, 5][(pixels % DOT_WIDTH) as usize];
    let dots = ((pixels - gap) / DOT_WIDTH) as usize;
    let bold = (gap % SPACE_WIDTH) as usize;
    let plain = ((gap - 5 * bold as u32) / SPACE_WIDTH) as usize;
    let mut out = String::from("&8");
    if bold > 0 {
        out.push_str(&format!("&l{}&8", " ".repeat(bold)));
    }
    out.push_str(&" ".repeat(plain));
    out.push_str(&". ".repeat(dots));
    out
}

/// A solid line, `pixels` wide, going from one color to the other. Struck-out
/// spaces join up, where `─` glyphs would leave gaps. Needs at least 12 pixels.
fn strip(pixels: u32, from: Rgb, to: Rgb) -> String {
    let pixels = pixels.max(MIN_STRIP);
    let bold = (pixels % SPACE_WIDTH) as usize;
    let plain = ((pixels - 5 * bold as u32) / SPACE_WIDTH) as usize;
    let count = bold + plain;
    let mut out = String::new();
    let mut last = None;
    for i in 0..count {
        let is_bold = i < bold;
        let step = i / SPACES_PER_COLOR * SPACES_PER_COLOR;
        // The last step lands on `to`, whichever space it starts on.
        let steps = (count - 1) / SPACES_PER_COLOR * SPACES_PER_COLOR;
        let color = lerp(from, to, step as f32 / steps.max(1) as f32);
        if last != Some((color, is_bold)) {
            out.push_str(&format!("&r{}{}&m", hex_code(color), if is_bold { "&l" } else { "" }));
            last = Some((color, is_bold));
        }
        out.push(' ');
    }
    out
}

fn lerp(from: Rgb, to: Rgb, t: f32) -> Rgb {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// `&x&R&R&G&G&B&B`, the legacy way to spell a hex color.
fn hex_code((r, g, b): Rgb) -> String {
    let mut out = String::from("&x");
    for c in format!("{r:02x}{g:02x}{b:02x}").chars() {
        out.push('&');
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_match_the_client() {
        assert_eq!(text_width("Damage"), 36);
        assert_eq!(text_width("&c&lHi"), 6 + 1 + 2 + 1);
        assert_eq!(text_width("&lA&r "), 7 + 4);
        assert_eq!(text_width("&x&1&2&3&4&5&6Hi"), 8);
        assert_eq!(text_width("❤ ⚔"), 8 + 4 + 8);
        assert_eq!(text_width("🛡"), 8);
        assert_eq!(text_width("&l&x&1&2&3&4&5&6Hi"), 10);
        // Not a real code, and the parser drops it anyway.
        assert_eq!(text_width("&zHi"), 8);
        assert_eq!(text_width(&format!("a{FILL}b")), 12);
    }

    #[test]
    fn fill_is_exact() {
        for pixels in MIN_FILL..200 {
            assert_eq!(text_width(&fill(pixels)), pixels, "{:?}", fill(pixels));
        }
    }

    #[test]
    fn strip_is_exact() {
        for pixels in MIN_STRIP..200 {
            assert_eq!(text_width(&strip(pixels, (0, 0, 0), (255, 255, 255))), pixels);
        }
    }

    #[test]
    fn divider_is_exact() {
        for title in ["", "&fGems", "&l<>"] {
            let divider = Divider { title: title.into(), color: Divider::DEFAULT_COLOR, fade: Divider::DEFAULT_FADE };
            for width in divider.min_width()..300 {
                assert_eq!(text_width(&divider.render(width)), width, "{title} at {width}");
            }
        }
    }

    #[test]
    fn strip_fades_from_end_to_end() {
        let line = strip(40, (0, 0, 0), (255, 0, 0));
        assert!(line.starts_with("&r&x&0&0&0&0&0&0&m"), "{line}");
        assert!(line.contains("&r&x&f&f&0&0&0&0&m"), "{line}");
    }
}
