//! Turns config strings into chat components. Understands the MiniMessage tags the Java
//! version uses (colors, hex, gradient, rainbow, bold and so on) plus old style &a codes.

use pumpkin_plugin_api::common::{NamedColor, RgbColor};
use pumpkin_plugin_api::text::TextComponent;

#[derive(Clone, Copy, PartialEq)]
enum Color {
    Named(NamedColor),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Default)]
struct Style {
    color: Option<Color>,
    bold: Option<bool>,
    italic: Option<bool>,
    underlined: Option<bool>,
    strikethrough: Option<bool>,
    obfuscated: Option<bool>,
    /// Index into the list of gradients, colored per character afterwards.
    gradient: Option<usize>,
}

struct Frame {
    tag: String,
    style: Style,
}

const NAMES: [(&str, NamedColor, (u8, u8, u8)); 17] = [
    ("black", NamedColor::Black, (0, 0, 0)),
    ("dark_blue", NamedColor::DarkBlue, (0, 0, 170)),
    ("dark_green", NamedColor::DarkGreen, (0, 170, 0)),
    ("dark_aqua", NamedColor::DarkAqua, (0, 170, 170)),
    ("dark_red", NamedColor::DarkRed, (170, 0, 0)),
    ("dark_purple", NamedColor::DarkPurple, (170, 0, 170)),
    ("gold", NamedColor::Gold, (255, 170, 0)),
    ("gray", NamedColor::Gray, (170, 170, 170)),
    ("grey", NamedColor::Gray, (170, 170, 170)),
    ("dark_gray", NamedColor::DarkGray, (85, 85, 85)),
    ("blue", NamedColor::Blue, (85, 85, 255)),
    ("green", NamedColor::Green, (85, 255, 85)),
    ("aqua", NamedColor::Aqua, (85, 255, 255)),
    ("red", NamedColor::Red, (255, 85, 85)),
    ("light_purple", NamedColor::LightPurple, (255, 85, 255)),
    ("yellow", NamedColor::Yellow, (255, 255, 85)),
    ("white", NamedColor::White, (255, 255, 255)),
];

pub fn named_color(name: &str) -> Option<NamedColor> {
    NAMES.iter().find(|(n, _, _)| *n == name).map(|(_, c, _)| *c)
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let v = u32::from_str_radix(hex, 16).ok()?;
            return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return None;
    }
    NAMES.iter().find(|(n, _, _)| *n == s).map(|(_, c, _)| Color::Named(*c))
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Named(n) => NAMES.iter().find(|(_, c, _)| *c == n).map(|(_, _, v)| *v).unwrap_or((255, 255, 255)),
    }
}

const LEGACY: [(char, &str); 22] = [
    ('0', "<black>"), ('1', "<dark_blue>"), ('2', "<dark_green>"), ('3', "<dark_aqua>"), ('4', "<dark_red>"),
    ('5', "<dark_purple>"), ('6', "<gold>"), ('7', "<gray>"), ('8', "<dark_gray>"), ('9', "<blue>"),
    ('a', "<green>"), ('b', "<aqua>"), ('c', "<red>"), ('d', "<light_purple>"), ('e', "<yellow>"),
    ('f', "<white>"), ('k', "<obfuscated>"), ('l', "<bold>"), ('m', "<strikethrough>"), ('n', "<underlined>"),
    ('o', "<italic>"), ('r', "<reset>"),
];

/// "&a" and "&#ff8800" become MiniMessage tags.
pub fn legacy_to_mini(s: &str) -> String {
    if !s.contains('&') && !s.contains('§') {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 16);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '&' || c == '§') && i + 1 < chars.len() {
            let next = chars[i + 1].to_ascii_lowercase();
            if next == '#' && i + 7 < chars.len() {
                let hex: String = chars[i + 2..i + 8].iter().collect();
                if hex.chars().all(|h| h.is_ascii_hexdigit()) {
                    out.push_str(&format!("<#{hex}>"));
                    i += 8;
                    continue;
                }
            }
            if let Some((_, tag)) = LEGACY.iter().find(|(k, _)| *k == next) {
                out.push_str(tag);
                i += 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Escapes text so tags in it are shown literally, for showing raw config values.
pub fn escape(s: &str) -> String {
    s.replace('<', "\\<")
}

struct Run {
    text: String,
    style: Style,
}

/// Parses into styled runs. `inserts` are tags like <message> replaced by literal text.
fn runs(input: &str, inserts: &[(&str, &str)]) -> Vec<Run> {
    let s = legacy_to_mini(input);
    let chars: Vec<char> = s.chars().collect();
    let mut stack: Vec<Frame> = Vec::new();
    let mut out: Vec<Run> = Vec::new();
    let mut gradients: Vec<Vec<(u8, u8, u8)>> = Vec::new();
    let mut buf = String::new();
    let current = |stack: &Vec<Frame>| stack.last().map(|f| f.style.clone()).unwrap_or_default();

    let flush = |buf: &mut String, out: &mut Vec<Run>, stack: &Vec<Frame>| {
        if !buf.is_empty() {
            out.push(Run { text: std::mem::take(buf), style: current(stack) });
        }
    };

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '<' {
            buf.push('<');
            i += 2;
            continue;
        }
        if c == '<' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '>') {
                let tag: String = chars[i + 1..i + 1 + end].iter().collect();
                let lower = tag.to_ascii_lowercase();
                let closing = lower.starts_with('/');
                let name = lower.trim_start_matches('/').to_string();
                let base = name.split(':').next().unwrap_or("").to_string();

                if let Some((_, value)) = inserts.iter().find(|(k, _)| *k == name) {
                    flush(&mut buf, &mut out, &stack);
                    out.push(Run { text: value.to_string(), style: current(&stack) });
                    i += end + 2;
                    continue;
                }

                let known = parse_color(&name).is_some()
                    || matches!(base.as_str(), "color" | "c" | "colour" | "gradient" | "rainbow" | "bold" | "b"
                        | "italic" | "i" | "em" | "underlined" | "u" | "strikethrough" | "st" | "obfuscated"
                        | "obf" | "reset" | "newline" | "br");
                if !known {
                    // Unknown tags such as <password> in help texts stay visible.
                    buf.push(c);
                    i += 1;
                    continue;
                }
                flush(&mut buf, &mut out, &stack);
                i += end + 2;

                if closing {
                    if let Some(pos) = stack.iter().rposition(|f| f.tag == base || f.tag == name) {
                        stack.truncate(pos);
                    }
                    continue;
                }
                match base.as_str() {
                    "reset" => {
                        stack.clear();
                        continue;
                    }
                    "newline" | "br" => {
                        out.push(Run { text: "\n".into(), style: current(&stack) });
                        continue;
                    }
                    _ => {}
                }
                let mut style = current(&stack);
                let mut tag_name = base.clone();
                if let Some(col) = parse_color(&name) {
                    style.color = Some(col);
                    style.gradient = None;
                    tag_name = name.clone();
                } else {
                    let args: Vec<&str> = name.split(':').skip(1).collect();
                    match base.as_str() {
                        "color" | "c" | "colour" => {
                            if let Some(col) = args.first().and_then(|a| parse_color(a)) {
                                style.color = Some(col);
                                style.gradient = None;
                            }
                        }
                        "gradient" => {
                            let mut cols: Vec<(u8, u8, u8)> = args.iter().filter_map(|a| parse_color(a)).map(rgb).collect();
                            if cols.is_empty() {
                                cols = vec![(255, 255, 255), (0, 0, 0)];
                            }
                            if cols.len() == 1 {
                                cols.push(cols[0]);
                            }
                            gradients.push(cols);
                            style.gradient = Some(gradients.len() - 1);
                        }
                        "rainbow" => {
                            gradients.push(vec![(255, 0, 0), (255, 255, 0), (0, 255, 0), (0, 255, 255), (0, 0, 255), (255, 0, 255), (255, 0, 0)]);
                            style.gradient = Some(gradients.len() - 1);
                        }
                        "bold" | "b" => style.bold = Some(true),
                        "italic" | "i" | "em" => style.italic = Some(true),
                        "underlined" | "u" => style.underlined = Some(true),
                        "strikethrough" | "st" => style.strikethrough = Some(true),
                        "obfuscated" | "obf" => style.obfuscated = Some(true),
                        _ => {}
                    }
                }
                stack.push(Frame { tag: tag_name, style });
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out, &stack);

    // Spread every gradient over all the characters it covers.
    for (gi, cols) in gradients.iter().enumerate() {
        let total: usize = out.iter().filter(|r| r.style.gradient == Some(gi)).map(|r| r.text.chars().count()).sum();
        if total == 0 {
            continue;
        }
        let mut n = 0usize;
        let mut split: Vec<Run> = Vec::new();
        for r in out.drain(..) {
            if r.style.gradient != Some(gi) {
                split.push(r);
                continue;
            }
            for ch in r.text.chars() {
                let t = if total <= 1 { 0.0 } else { n as f32 / (total - 1) as f32 };
                let seg = t * (cols.len() - 1) as f32;
                let idx = (seg.floor() as usize).min(cols.len() - 2);
                let f = seg - idx as f32;
                let (a, b) = (cols[idx], cols[idx + 1]);
                let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
                let mut st = r.style.clone();
                st.gradient = None;
                st.color = Some(Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2)));
                split.push(Run { text: ch.to_string(), style: st });
                n += 1;
            }
        }
        out = split;
    }
    out
}

fn build(runs: Vec<Run>, item: bool) -> TextComponent {
    let mut root = TextComponent::text("");
    if item {
        root = root.italic(false);
    }
    for r in runs {
        let mut c = TextComponent::text(&r.text);
        match r.style.color {
            Some(Color::Named(n)) => c = c.color_named(n),
            Some(Color::Rgb(red, g, b)) => c = c.color_rgb(RgbColor { r: red, g, b }),
            None => {}
        }
        if let Some(v) = r.style.bold { c = c.bold(v); }
        if let Some(v) = r.style.italic { c = c.italic(v); }
        if let Some(v) = r.style.underlined { c = c.underlined(v); }
        if let Some(v) = r.style.strikethrough { c = c.strikethrough(v); }
        if let Some(v) = r.style.obfuscated { c = c.obfuscated(v); }
        root = root.add_child(c);
    }
    root
}

pub fn parse(s: &str) -> TextComponent {
    build(runs(s, &[]), false)
}

pub fn parse_with(s: &str, inserts: &[(&str, &str)]) -> TextComponent {
    build(runs(s, inserts), false)
}

/// For item names and lore, which are italic by default in vanilla.
pub fn item(s: &str) -> TextComponent {
    build(runs(s, &[]), true)
}

/// The text without any formatting.
pub fn plain(s: &str) -> String {
    runs(s, &[]).into_iter().map(|r| r.text).collect()
}

/// Plain text from a chat component's JSON.
pub fn plain_json(json: &str) -> String {
    fn walk(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::String(s) => out.push_str(s),
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => {
                if let Some(serde_json::Value::String(t)) = o.get("text") {
                    out.push_str(t);
                }
                if let Some(e) = o.get("extra") {
                    walk(e, out);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
        walk(&v, &mut out);
    }
    out
}
