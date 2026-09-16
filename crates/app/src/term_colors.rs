//! 终端配色：16 色调色板 + 对比度保证。
//!
//! 摘录自参考项目 `wu` 的 `src/theme.rs` —— 那边 800 多行是整套主题系统，
//! 本项目只需要终端这一小块，所以单独成模块（搬需要的东西，不搬整棵树）。
//!
//! 两块都值得留：
//!
//! - **调色板要跟背景走**：ANSI 的经典 X11 固定色（黄 `#CDCD00`）在浅色主题的
//!   近白底上对比度不到 1.5:1，基本看不清
//! - **`ensure_contrast`**：程序可能用真彩色直接指定颜色（纯黄 `#FFFF00`），
//!   换调色板救不了 —— 这时只调亮度、保住色相与饱和度

/// 终端的 16 个 ANSI 颜色。
///
/// 原先终端用的是经典 X11 固定色（黄 `#CDCD00`、亮黄 `#FFFF00`），在浅色主题的
/// 近白背景上对比度不到 1.5:1，基本看不清——ANSI 配色必须跟背景走。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TerminalPalette {
    pub black: [u8; 3],
    pub red: [u8; 3],
    pub green: [u8; 3],
    pub yellow: [u8; 3],
    pub blue: [u8; 3],
    pub magenta: [u8; 3],
    pub cyan: [u8; 3],
    pub white: [u8; 3],
    pub bright_black: [u8; 3],
    pub bright_red: [u8; 3],
    pub bright_green: [u8; 3],
    pub bright_yellow: [u8; 3],
    pub bright_blue: [u8; 3],
    pub bright_magenta: [u8; 3],
    pub bright_cyan: [u8; 3],
    pub bright_white: [u8; 3],
}

impl TerminalPalette {
    /// 浅色背景用：深饱和色，保证在近白底上对比度 ≥ 4.5:1。
    pub fn light() -> Self {
        Self {
            black: [0x3b, 0x3b, 0x3b],
            red: [0xb3, 0x26, 0x1e],
            green: [0x2b, 0x6f, 0x2d],
            yellow: [0x80, 0x62, 0x00], // 琥珀：黄在浅底上必须压亮度（旧值 #CDCD00 只有 1.6:1）
            blue: [0x1a, 0x56, 0xc4],
            magenta: [0x9c, 0x27, 0xb0],
            cyan: [0x0b, 0x6e, 0x78],
            white: [0x6b, 0x6b, 0x6b],
            bright_black: [0x6e, 0x6e, 0x6e],
            bright_red: [0xb9, 0x1c, 0x1c],
            bright_green: [0x17, 0x6b, 0x28],
            bright_yellow: [0x8a, 0x62, 0x00],
            bright_blue: [0x0f, 0x5c, 0xb0],
            bright_magenta: [0x99, 0x12, 0x4d],
            bright_cyan: [0x00, 0x69, 0x5c],
            bright_white: [0x2b, 0x2b, 0x2b],
        }
    }

    /// 深色背景用：中亮度柔和色（One Half 风格），避免纯黄/纯红在深底上刺眼发晕。
    pub fn dark() -> Self {
        Self {
            black: [0x3b, 0x40, 0x48],
            red: [0xe0, 0x6c, 0x75],
            green: [0x98, 0xc3, 0x79],
            yellow: [0xe5, 0xc0, 0x7b],
            blue: [0x61, 0xaf, 0xef],
            magenta: [0xc6, 0x78, 0xdd],
            cyan: [0x56, 0xb6, 0xc2],
            white: [0xab, 0xb2, 0xbf],
            bright_black: [0x7f, 0x84, 0x8e],
            bright_red: [0xff, 0x7b, 0x86],
            bright_green: [0xb4, 0xd8, 0x9b],
            bright_yellow: [0xff, 0xd6, 0x8a],
            bright_blue: [0x7c, 0xc0, 0xff],
            bright_magenta: [0xda, 0x8e, 0xe7],
            bright_cyan: [0x6f, 0xd3, 0xdf],
            bright_white: [0xe6, 0xe9, 0xef],
        }
    }
}

/// WCAG 相对亮度。
fn relative_luminance(rgb: [u8; 3]) -> f32 {
    let f = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(rgb[0]) + 0.7152 * f(rgb[1]) + 0.0722 * f(rgb[2])
}

/// WCAG 对比度（1.0 ~ 21.0）。
pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// RGB → HSL（h 0~360，s/l 0~1）。
fn rgb_to_hsl(rgb: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / d) % 6.0)
    } else if (max - g).abs() < f32::EPSILON {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (if h < 0.0 { h + 360.0 } else { h }, s, l)
}

/// HSL → RGB（h 0~360，s/l 0~1）。
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    [to(r1), to(g1), to(b1)]
}

/// 把前景色调整到与背景至少 `min_ratio` 的对比度：**保持色相与饱和度**，只改亮度。
///
/// 为什么需要：程序可能用真彩色/256 色直接指定颜色（例如纯黄 `#FFFF00`），
/// 在浅色终端背景上对比度只有 1.04:1，完全看不清——换调色板救不了这种情况。
/// `min_ratio <= 1.0` 表示不处理。
pub fn ensure_contrast(fg: [u8; 3], bg: [u8; 3], min_ratio: f32) -> [u8; 3] {
    if min_ratio <= 1.0 || contrast_ratio(fg, bg) >= min_ratio {
        return fg;
    }
    let (h, s, l) = rgb_to_hsl(fg);
    let light_bg = relative_luminance(bg) > 0.5;
    let (mut lo, mut hi) = if light_bg { (0.0, l) } else { (l, 1.0) };
    let mut best: Option<[u8; 3]> = None;
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        let cand = hsl_to_rgb(h, s, mid);
        if contrast_ratio(cand, bg) >= min_ratio {
            best = Some(cand);
            if light_bg {
                lo = mid;
            } else {
                hi = mid;
            }
        } else if light_bg {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    best.unwrap_or(fg)
}
