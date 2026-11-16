//! Colors for the contrast checks of skin palettes (docs/skins.md, "Contrast"): parsing the CSS color forms a palette
//! uses and the WCAG 2 contrast ratio between two colors.
//!
//! Hex colors, `rgb()`/`rgba()`, `hsl()`/`hsla()`, `oklab()`, `oklch()`, `transparent` and the named colors are
//! understood; anything else (`color-mix()`, `light-dark()`, relative colors) cannot be checked and is reported as
//! such. Colors with transparency are laid over the ground they sit on before measuring.

/// An sRGB color with components and alpha from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub fn opaque(self) -> bool {
        self.a >= 1.0 - 1e-9
    }

    /// This color laid over `ground` (which should be opaque).
    pub fn over(self, ground: Self) -> Self {
        let a = self.a.clamp(0.0, 1.0);
        let mix = |top: f64, bottom: f64| top * a + bottom * (1.0 - a);
        Self {
            r: mix(self.r, ground.r),
            g: mix(self.g, ground.g),
            b: mix(self.b, ground.b),
            a: 1.0,
        }
    }

    /// WCAG 2 relative luminance.
    fn luminance(self) -> f64 {
        let channel = |c: f64| {
            let c = c.clamp(0.0, 1.0);
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// `#rrggbb` (alpha dropped), for messages and the generated guard rules.
    pub fn hex(self) -> String {
        let byte = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(self.r),
            byte(self.g),
            byte(self.b)
        )
    }
}

/// The WCAG 2 contrast ratio of two opaque colors, from 1 to 21.
pub fn contrast(a: Rgba, b: Rgba) -> f64 {
    let (x, y) = (a.luminance(), b.luminance());
    let (light, dark) = if x >= y { (x, y) } else { (y, x) };
    (light + 0.05) / (dark + 0.05)
}

/// Parses one CSS color; `None` for forms this module does not understand.
pub fn parse(value: &str) -> Option<Rgba> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    if value == "transparent" {
        return Some(Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        });
    }
    if let Some((name, rest)) = value.split_once('(') {
        let body = rest.strip_suffix(')')?;
        return match name.trim() {
            "rgb" | "rgba" => parse_rgb(body),
            "hsl" | "hsla" => parse_hsl(body),
            "oklab" => parse_oklab(body, false),
            "oklch" => parse_oklab(body, true),
            _ => None,
        };
    }
    NAMED
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, rgb)| Rgba {
            r: f64::from((rgb >> 16) & 0xff) / 255.0,
            g: f64::from((rgb >> 8) & 0xff) / 255.0,
            b: f64::from(rgb & 0xff) / 255.0,
            a: 1.0,
        })
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digits: Vec<u8> = match hex.len() {
        3 | 4 => hex
            .chars()
            .map(|c| u8::from_str_radix(&format!("{c}{c}"), 16).ok())
            .collect::<Option<_>>()?,
        6 | 8 => (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect::<Option<_>>()?,
        _ => return None,
    };
    let channel = |i: usize| f64::from(digits[i]) / 255.0;
    Some(Rgba {
        r: channel(0),
        g: channel(1),
        b: channel(2),
        a: if digits.len() == 4 { channel(3) } else { 1.0 },
    })
}

/// The components of a color function: comma or space separated, with an optional `/ alpha`.
fn components(body: &str) -> Option<(Vec<&str>, Option<&str>)> {
    let (main, alpha) = match body.split_once('/') {
        Some((main, alpha)) => (main, Some(alpha.trim())),
        None => (body, None),
    };
    let mut parts: Vec<&str> = if main.contains(',') {
        main.split(',').map(str::trim).collect()
    } else {
        main.split_whitespace().collect()
    };
    let mut alpha = alpha;
    if parts.len() == 4 && alpha.is_none() {
        alpha = parts.pop();
    }
    (parts.len() == 3).then_some((parts, alpha))
}

/// A number, or a percentage scaled so that 100% is `percent_of`.
fn number(text: &str, percent_of: f64) -> Option<f64> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        return percent.parse::<f64>().ok().map(|p| p / 100.0 * percent_of);
    }
    text.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn alpha(text: Option<&str>) -> Option<f64> {
    match text {
        None => Some(1.0),
        Some(text) => number(text, 1.0).map(|a| a.clamp(0.0, 1.0)),
    }
}

fn parse_rgb(body: &str) -> Option<Rgba> {
    let (parts, a) = components(body)?;
    let channel = |text: &str| number(text, 255.0).map(|c| c / 255.0);
    Some(Rgba {
        r: channel(parts[0])?,
        g: channel(parts[1])?,
        b: channel(parts[2])?,
        a: alpha(a)?,
    })
}

fn parse_hsl(body: &str) -> Option<Rgba> {
    let (parts, a) = components(body)?;
    let hue = parts[0]
        .trim()
        .trim_end_matches("deg")
        .parse::<f64>()
        .ok()?;
    let saturation = number(parts[1], 1.0)?.clamp(0.0, 1.0);
    let lightness = number(parts[2], 1.0)?.clamp(0.0, 1.0);
    let k = |n: f64| (n + hue / 30.0).rem_euclid(12.0);
    let chroma = saturation * lightness.min(1.0 - lightness);
    let f = |n: f64| lightness - chroma * (-1.0_f64).max((k(n) - 3.0).min((9.0 - k(n)).min(1.0)));
    Some(Rgba {
        r: f(0.0),
        g: f(8.0),
        b: f(4.0),
        a: alpha(a)?,
    })
}

/// `oklab(L a b)` or `oklch(L C H)`, converted to sRGB and clamped into its gamut.
fn parse_oklab(body: &str, polar: bool) -> Option<Rgba> {
    let (parts, a) = components(body)?;
    let lightness = number(parts[0], 1.0)?;
    let (ok_a, ok_b) = if polar {
        let chroma = number(parts[1], 0.4)?;
        let hue = parts[2]
            .trim()
            .trim_end_matches("deg")
            .parse::<f64>()
            .ok()?
            .to_radians();
        (chroma * hue.cos(), chroma * hue.sin())
    } else {
        (number(parts[1], 0.4)?, number(parts[2], 0.4)?)
    };
    let l = (lightness + 0.396_337_777_4 * ok_a + 0.215_803_757_3 * ok_b).powi(3);
    let m = (lightness - 0.105_561_345_8 * ok_a - 0.063_854_172_8 * ok_b).powi(3);
    let s = (lightness - 0.089_484_177_5 * ok_a - 1.291_485_548 * ok_b).powi(3);
    let linear = [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
    ];
    let gamma = |c: f64| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    };
    Some(Rgba {
        r: gamma(linear[0]),
        g: gamma(linear[1]),
        b: gamma(linear[2]),
        a: alpha(a)?,
    })
}

/// The CSS named colors.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ratio(a: &str, b: &str) -> f64 {
        (contrast(parse(a).unwrap(), parse(b).unwrap()) * 100.0).round() / 100.0
    }

    #[test]
    fn contrast_matches_the_wcag_formula() {
        assert_eq!(ratio("#000", "#fff"), 21.0);
        assert_eq!(ratio("#fff", "#fff"), 1.0);
        assert_eq!(ratio("#777777", "#ffffff"), 4.48);
        assert_eq!(ratio("#98a2b1", "#ffffff"), 2.58);
        assert_eq!(ratio("#151b2b", "#f7f8fb"), 16.16);
    }

    #[test]
    fn the_color_forms_of_palettes_parse() {
        let white = parse("#ffffff").unwrap();
        for (text, hex) in [
            ("#4F46E5", "#4f46e5"),
            ("#abc", "#aabbcc"),
            ("rgb(79 70 229)", "#4f46e5"),
            ("rgb(79, 70, 229)", "#4f46e5"),
            ("rgba(79,70,229,1)", "#4f46e5"),
            ("rgb(100% 0% 0%)", "#ff0000"),
            ("hsl(0 100% 50%)", "#ff0000"),
            ("hsl(120deg, 100%, 25%)", "#008000"),
            ("oklch(62.8% 0.2577 29.23)", "#ff0000"),
            ("oklab(1 0 0)", "#ffffff"),
            ("rebeccapurple", "#663399"),
        ] {
            assert_eq!(parse(text).map(Rgba::hex).as_deref(), Some(hex), "{text}");
        }
        let half = parse("rgb(148 163 184 / 50%)").unwrap();
        assert!((half.a - 0.5).abs() < 1e-9);
        assert_eq!(half.over(parse("#121827").unwrap()).hex(), "#535e70");
        assert_eq!(parse("transparent").unwrap().over(white).hex(), "#ffffff");
        for unknown in [
            "color-mix(in srgb, red, blue)",
            "currentcolor",
            "var(--x)",
            "#12",
            "rgb(1 2)",
        ] {
            assert_eq!(parse(unknown), None, "{unknown}");
        }
    }
}
