//! 终端渲染元素：把 alacritty 的字符网格高性能地渲染到 gpui。
//! 参考 Zed 的 terminal_element.rs：合并相邻同色 cell 成 TextRun，用 text_system 批量绘制。

use crate::term_colors::{TerminalPalette, ensure_contrast};
use crate::terminal::Terminal;
use alacritty_terminal::{
    term::cell::Flags,
    vte::ansi::{Color, NamedColor, Rgb},
};
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, DispatchPhase, Element, ElementId, Entity,
    FocusHandle, Font, FontStyle, FontWeight, GlobalElementId, Hitbox, Hsla, InspectorElementId,
    InteractiveElement, Interactivity, IntoElement, KeyDownEvent, LayoutId, Modifiers, MouseButton,
    Pixels, Rgba, ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement, TextAlign, TextRun,
    UnderlineStyle, Window, fill, point, px, relative, size,
};
use std::rc::Rc;

// ==================== 颜色转换 ====================

fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    Hsla::from(Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    })
}

/// 16 个基本色按主题调色板取色（浅色主题必须用深色系，否则黄/青在近白底上看不清）。
fn named_rgb(c: NamedColor, p: &TerminalPalette) -> (u8, u8, u8) {
    let rgb = match c {
        NamedColor::Black => p.black,
        NamedColor::Red => p.red,
        NamedColor::Green => p.green,
        NamedColor::Yellow => p.yellow,
        NamedColor::Blue => p.blue,
        NamedColor::Magenta => p.magenta,
        NamedColor::Cyan => p.cyan,
        NamedColor::White => p.white,
        NamedColor::BrightBlack => p.bright_black,
        NamedColor::BrightRed => p.bright_red,
        NamedColor::BrightGreen => p.bright_green,
        NamedColor::BrightYellow => p.bright_yellow,
        NamedColor::BrightBlue => p.bright_blue,
        NamedColor::BrightMagenta => p.bright_magenta,
        NamedColor::BrightCyan => p.bright_cyan,
        NamedColor::BrightWhite => p.bright_white,
        // Dim* 是「标准色变暗」的语义，直接复用标准色（深色主题下再压暗会更难读）
        NamedColor::DimBlack => p.black,
        NamedColor::DimRed => p.red,
        NamedColor::DimGreen => p.green,
        NamedColor::DimYellow => p.yellow,
        NamedColor::DimBlue => p.blue,
        NamedColor::DimMagenta => p.magenta,
        NamedColor::DimCyan => p.cyan,
        NamedColor::DimWhite => p.white,
        // 下面这些由 convert_color 单独处理，兜底给白色
        _ => [0xff, 0xff, 0xff],
    };
    (rgb[0], rgb[1], rgb[2])
}

/// 256 色（Indexed）色表，按 xterm 规则生成。
fn indexed_rgb(i: u8, palette: &TerminalPalette) -> (u8, u8, u8) {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..=15 => {
            let names = [
                NamedColor::Black,
                NamedColor::Red,
                NamedColor::Green,
                NamedColor::Yellow,
                NamedColor::Blue,
                NamedColor::Magenta,
                NamedColor::Cyan,
                NamedColor::White,
                NamedColor::BrightBlack,
                NamedColor::BrightRed,
                NamedColor::BrightGreen,
                NamedColor::BrightYellow,
                NamedColor::BrightBlue,
                NamedColor::BrightMagenta,
                NamedColor::BrightCyan,
                NamedColor::BrightWhite,
            ];
            named_rgb(names[i as usize], palette)
        }
        16..=231 => {
            let n = (i - 16) as usize;
            (CUBE[n / 36], CUBE[(n / 6) % 6], CUBE[n % 6])
        }
        232..=255 => {
            let v = 8 + (i as u16 - 232) * 10;
            (v as u8, v as u8, v as u8)
        }
    }
}

/// alacritty Color → gpui Hsla。
fn convert_color(c: &Color, default_fg: Hsla, default_bg: Hsla, palette: &TerminalPalette) -> Hsla {
    match c {
        Color::Named(n) => match n {
            NamedColor::Foreground
            | NamedColor::Cursor
            | NamedColor::BrightForeground
            | NamedColor::DimForeground => default_fg,
            NamedColor::Background => default_bg,
            other => {
                let (r, g, b) = named_rgb(*other, palette);
                rgb_to_hsla(r, g, b)
            }
        },
        Color::Spec(Rgb { r, g, b }) => rgb_to_hsla(*r, *g, *b),
        Color::Indexed(i) => {
            let (r, g, b) = indexed_rgb(*i, palette);
            rgb_to_hsla(r, g, b)
        }
    }
}

/// 把前景色钳制到与背景至少 `min_ratio` 的对比度（保持色相）。
///
/// 程序可能用真彩色/256 色直接给颜色（例如纯黄 `#FFFF00`），浅色背景上对比度只有
/// 1.04:1，完全看不清；换调色板救不了这种情况，只能在渲染时兜底。
fn clamp_fg(fg: Hsla, bg: Hsla, min_ratio: f32) -> Hsla {
    if min_ratio <= 1.0 {
        return fg;
    }
    let to_rgb = |c: Hsla| {
        let c = gpui::Rgba::from(c);
        [
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        ]
    };
    let (f, b) = (to_rgb(fg), to_rgb(bg));
    let fixed = ensure_contrast(f, b, min_ratio);
    if fixed == f {
        fg
    } else {
        rgb_to_hsla(fixed[0], fixed[1], fixed[2])
    }
}

// ==================== 按键 → 转义序列 ====================

/// 把 keystroke 转成终端输入字节（简化版，覆盖常用键）。
fn keystroke_to_bytes(key: &str, modifiers: &Modifiers) -> Option<Vec<u8>> {
    let ctrl = modifiers.control;
    let alt = modifiers.alt;
    let shift = modifiers.shift;

    // Ctrl + 字母 → 控制字符（0x01-0x1a）
    if ctrl && !alt {
        if key.chars().count() == 1
            && let Some(c) = key.chars().next()
            && c.is_ascii_alphabetic()
        {
            let code = (c.to_ascii_lowercase() as u8) - b'a' + 1;
            return Some(vec![code]);
        }
        return match key {
            "space" => Some(vec![0x00]),
            "backspace" => Some(vec![0x08]),
            "tab" => Some(vec![0x09]),
            "enter" => Some(vec![0x0a]),
            _ => None,
        };
    }

    // Alt + 字符 → ESC + 字符
    if alt
        && key.chars().count() == 1
        && let Some(c) = key.chars().next()
        && c.is_ascii()
    {
        return Some(vec![0x1b, c as u8]);
    }

    // 特殊键
    let esc = match key {
        "enter" => Some(vec![if shift { 0x0a } else { 0x0d }]),
        "backspace" => Some(vec![0x7f]),
        "tab" => Some(if shift {
            vec![0x1b, b'[', b'Z']
        } else {
            vec![0x09]
        }),
        "escape" => Some(vec![0x1b]),
        "up" => Some(b"\x1b[A".to_vec()),
        "down" => Some(b"\x1b[B".to_vec()),
        "right" => Some(b"\x1b[C".to_vec()),
        "left" => Some(b"\x1b[D".to_vec()),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "insert" => Some(b"\x1b[2~".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(b"\x1b[15~".to_vec()),
        "f6" => Some(b"\x1b[17~".to_vec()),
        "f7" => Some(b"\x1b[18~".to_vec()),
        "f8" => Some(b"\x1b[19~".to_vec()),
        "f9" => Some(b"\x1b[20~".to_vec()),
        "f10" => Some(b"\x1b[21~".to_vec()),
        "f11" => Some(b"\x1b[23~".to_vec()),
        "f12" => Some(b"\x1b[24~".to_vec()),
        _ => None,
    };
    if esc.is_some() {
        return esc;
    }

    // 普通可打印字符交给 InputHandler（IME/文本输入）处理，这里返回 None 避免重复输入
    None
}

// ==================== 渲染数据 ====================

/// 一段合并的文本（相邻同色 cell）。
pub struct BatchedTextRun {
    line: i32,
    column: usize,
    text: String,
    style: TextRun,
}

/// 一段背景色区域（同一行的连续同色 cell）。
pub struct LayoutRect {
    line: i32,
    column: usize,
    cells: usize,
    color: Hsla,
}

/// 光标信息。
pub struct CursorInfo {
    line: i32,
    column: usize,
}

/// prepaint 阶段构建的渲染状态。
pub struct TerminalLayoutState {
    background: Hsla,
    rects: Vec<LayoutRect>,
    runs: Vec<BatchedTextRun>,
    cursor: Option<CursorInfo>,
    cell_width: Pixels,
    line_height: Pixels,
    font_size: Pixels,
    hitbox: Option<Hitbox>,
}

/// 单元格渲染参数（字形尺寸 + 前景/背景 + 调色板 + 对比度下限）。
/// 打成一个结构体，避免 `build_layout` 参数过长。
#[derive(Clone, Copy)]
struct CellStyle {
    cell_width: Pixels,
    line_height: Pixels,
    font_size: Pixels,
    default_fg: Hsla,
    default_bg: Hsla,
    palette: TerminalPalette,
    min_contrast: f32,
}

/// 从终端后端取字符网格，合并成渲染数据（自由函数，供 prepaint 闭包调用）。
fn build_layout(terminal: &Terminal, style: CellStyle) -> TerminalLayoutState {
    let CellStyle {
        cell_width,
        line_height,
        font_size,
        default_fg,
        default_bg,
        palette,
        min_contrast,
    } = style;
    let base_font = Font {
        family: "Consolas".into(),
        ..Font::default()
    };

    // 选区高亮色（前景色低透明度叠加）
    let selection_bg = default_fg.opacity(0.30);

    let mut runs: Vec<BatchedTextRun> = Vec::new();
    let mut rects: Vec<LayoutRect> = Vec::new();
    let mut cursor: Option<CursorInfo> = None;

    terminal.render(|content| {
        let display_offset = content.display_offset as i32;
        let selection = content.selection;
        let cursor_shape = content.cursor.shape;
        cursor = Some(CursorInfo {
            line: content.cursor.point.line.0 + display_offset,
            column: content.cursor.point.column.0,
        });

        let mut current: Option<BatchedTextRun> = None;

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let line = point.line.0 + display_offset;
            let column = point.column.0;

            let mut fg = cell.fg;
            let mut bg = cell.bg;
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }

            // 背景色：选区高亮优先，否则用单元格自身背景（跳过默认背景）
            let is_selected = selection
                .as_ref()
                .is_some_and(|sel| sel.contains_cell(&indexed, point, cursor_shape));
            let cell_bg = convert_color(&bg, default_fg, default_bg, &palette);
            let is_default_bg = matches!(bg, Color::Named(NamedColor::Background));
            let bg_hsla = if is_selected {
                Some(selection_bg)
            } else if !is_default_bg && cell_bg != default_bg {
                Some(cell_bg)
            } else {
                None
            };
            // 前景按「这一格实际会画的背景」钳制对比度（有自绘背景时也要看得清）
            let effective_bg = bg_hsla.unwrap_or(default_bg);
            let fg_hsla = clamp_fg(
                convert_color(&fg, default_fg, default_bg, &palette),
                effective_bg,
                min_contrast,
            );
            if let Some(color) = bg_hsla {
                if let Some(last) = rects.last_mut() {
                    if last.line == line
                        && last.column + last.cells == column
                        && last.color == color
                    {
                        last.cells += 1;
                    } else {
                        rects.push(LayoutRect {
                            line,
                            column,
                            cells: 1,
                            color,
                        });
                    }
                } else {
                    rects.push(LayoutRect {
                        line,
                        column,
                        cells: 1,
                        color,
                    });
                }
            }

            // 跳过宽字符占位符
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let ch = cell.c;
            let is_blank = ch == ' ' && !cell.flags.contains(Flags::UNDERLINE);

            if !is_blank {
                let weight = if cell.flags.contains(Flags::BOLD) {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                };
                let style = if cell.flags.contains(Flags::ITALIC) {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                };
                let underline = cell
                    .flags
                    .contains(Flags::UNDERLINE)
                    .then(|| UnderlineStyle {
                        color: Some(fg_hsla),
                        thickness: px(1.),
                        wavy: false,
                    });

                let run_style = TextRun {
                    len: ch.len_utf8(),
                    font: Font {
                        weight,
                        style,
                        ..base_font.clone()
                    },
                    color: fg_hsla,
                    background_color: None,
                    underline,
                    strikethrough: None,
                };

                let can_append = match &current {
                    Some(b) => {
                        b.line == line
                            && b.column + b.text.chars().count() == column
                            && b.style.color == run_style.color
                            && b.style.font.weight == run_style.font.weight
                            && b.style.font.style == run_style.font.style
                            && b.style.underline.is_some() == run_style.underline.is_some()
                    }
                    None => false,
                };

                if can_append {
                    if let Some(b) = current.as_mut() {
                        b.text.push(ch);
                        b.style.len += ch.len_utf8();
                    }
                } else {
                    if let Some(b) = current.take() {
                        runs.push(b);
                    }
                    current = Some(BatchedTextRun {
                        line,
                        column,
                        text: ch.to_string(),
                        style: run_style,
                    });
                }
            }
        }

        if let Some(b) = current.take() {
            runs.push(b);
        }
    });

    TerminalLayoutState {
        background: default_bg,
        rects,
        runs,
        cursor,
        cell_width,
        line_height,
        font_size,
        hitbox: None,
    }
}

// ==================== IME 状态 ====================

/// IME 预编辑（组合中）文本状态。存成 Entity 以便组合态变化时能 notify 触发重绘。
pub struct ImeState {
    pub marked_text: Option<String>,
}

// ==================== 终端元素 ====================

pub struct TerminalElement {
    terminal: Rc<Terminal>,
    focus: FocusHandle,
    interactivity: Interactivity,
    ime: Entity<ImeState>,
    cell_width: Pixels,
    line_height: Pixels,
    font_size: Pixels,
    last_columns: usize,
    last_lines: usize,
    default_fg: Hsla,
    default_bg: Hsla,
    /// 终端 16 色调色板（跟随主题，可在 settings.json 里覆盖单色）
    palette: TerminalPalette,
    /// 前景色最低对比度（1.0 = 关闭）；程序输出过浅的颜色会被自动压深
    min_contrast: f32,
}

impl TerminalElement {
    pub fn new(terminal: Rc<Terminal>, focus: FocusHandle, ime: Entity<ImeState>) -> Self {
        Self {
            terminal,
            focus,
            interactivity: Interactivity::new(),
            ime,
            cell_width: px(8.4),
            line_height: px(18.),
            font_size: px(14.),
            last_columns: 0,
            last_lines: 0,
            default_fg: gpui::hsla(0.0, 0.0, 0.9, 1.0),
            default_bg: gpui::hsla(0.0, 0.0, 0.1, 1.0),
            palette: TerminalPalette::dark(),
            min_contrast: 4.5,
        }
    }

    /// 设置默认前景/背景色（跟随主题）。
    pub fn colors(mut self, fg: Hsla, bg: Hsla) -> Self {
        self.default_fg = fg;
        self.default_bg = bg;
        self
    }

    /// 设置 16 色调色板（跟随主题；浅色主题必须用深色系）。
    pub fn palette(mut self, palette: TerminalPalette) -> Self {
        self.palette = palette;
        self
    }

    /// 设置前景色最低对比度（WCAG，1.0 = 关闭）。程序自带的过浅颜色会被自动压深/提亮。
    pub fn min_contrast(mut self, ratio: f32) -> Self {
        self.min_contrast = ratio;
        self
    }

    /// 跟踪焦点（InteractiveElement::track_focus 的封装）。
    pub fn track_focus(self, focus_handle: &FocusHandle) -> Self {
        <Self as InteractiveElement>::track_focus(self, focus_handle)
    }
}

impl IntoElement for TerminalElement {
    type Element = TerminalElement;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl InteractiveElement for TerminalElement {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl StatefulInteractiveElement for TerminalElement {}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalLayoutState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = self.interactivity.request_layout(
            global_id,
            inspector_id,
            window,
            cx,
            |mut style, window, cx| {
                style.size.width = relative(1.).into();
                style.size.height = relative(1.).into();
                window.request_layout(style, None, cx)
            },
        );
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let columns = (bounds.size.width / self.cell_width).floor().max(1.0) as usize;
        let lines = (bounds.size.height / self.line_height).floor().max(1.0) as usize;
        let need_resize = columns != self.last_columns || lines != self.last_lines;
        self.last_columns = columns;
        self.last_lines = lines;

        let terminal = self.terminal.clone();
        let style = CellStyle {
            cell_width: self.cell_width,
            line_height: self.line_height,
            font_size: self.font_size,
            default_fg: self.default_fg,
            default_bg: self.default_bg,
            palette: self.palette,
            min_contrast: self.min_contrast,
        };

        self.interactivity.prepaint(
            global_id,
            inspector_id,
            bounds,
            bounds.size,
            window,
            cx,
            move |_, _, hitbox, _window, _cx| {
                if need_resize {
                    terminal.resize(columns, lines);
                }
                let mut layout = build_layout(&terminal, style);
                layout.hitbox = hitbox;
                layout
            },
        )
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, layout.background));

            // 鼠标：点击聚焦 + 拖拽选择文本
            let focus_for_click = self.focus.clone();
            // 一次拖拽只记一行日志：诊断「拖拽事件到底有没有到」用，平时不刷屏
            let dragging_logged = std::rc::Rc::new(std::cell::Cell::new(false));
            let dragging_logged_down = dragging_logged.clone();
            let down_terminal = self.terminal.clone();
            let sel_ox = bounds.origin.x;
            let sel_oy = bounds.origin.y;
            let sel_cw = layout.cell_width;
            let sel_lh = layout.line_height;
            // 一次拖拽只记一行日志：诊断「拖拽事件到底有没有到」用，平时不刷屏

            self.interactivity
                .on_mouse_down(MouseButton::Left, move |e, window, cx| {
                    window.focus(&focus_for_click, cx);
                    let col = ((e.position.x - sel_ox) / sel_cw).floor().max(0.0) as usize;
                    let row = ((e.position.y - sel_oy) / sel_lh).floor().max(0.0) as i32;
                    down_terminal.start_selection(row, col);
                    dragging_logged_down.set(false);
                    logln!("[typst-live] 终端：按下左键 row={row} col={col}（开始选择）");
                });
            // 拖拽中更新选区终点。
            //
            // ⚠️ 每次都要 `window.refresh()`：终端平时靠「有输出 → Wakeup → 重画」，
            // 而拖拽期间根本没有输出 —— 不主动重画的话，选区只在下次有输出时才显形，
            // 用起来就像「选中不了」。
            let move_terminal = self.terminal.clone();
            let dragging_logged_move = dragging_logged.clone();
            self.interactivity.on_mouse_move(move |e, window, _cx| {
                if e.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if !dragging_logged_move.replace(true) {
                    logln!("[typst-live] 终端：拖拽中（收到第一个 Move 事件）");
                }
                let col = ((e.position.x - sel_ox) / sel_cw).floor().max(0.0) as usize;
                let row = ((e.position.y - sel_oy) / sel_lh).floor().max(0.0) as i32;
                move_terminal.update_selection(row, col);
                window.refresh();
            });
            // 松开鼠标：单击（空选区）时清掉选区，避免残留一个高亮格
            let up_terminal = self.terminal.clone();
            self.interactivity
                .on_mouse_up(MouseButton::Left, move |_e, window, _cx| {
                    let n = up_terminal
                        .selection_text()
                        .map(|t| t.chars().count())
                        .unwrap_or(0);
                    if n == 0 {
                        up_terminal.clear_selection();
                    }
                    logln!("[typst-live] 终端：松开左键，选中 {n} 字");
                    window.refresh();
                });

            // 右键：**有选区就复制、没有就粘贴**（Windows 控制台/终端的老习惯，
            // 也是最容易被发现的入口 —— 不用记快捷键）。
            let right_terminal = self.terminal.clone();
            self.interactivity
                .on_mouse_down(MouseButton::Right, move |_e, window, cx| {
                    match right_terminal.selection_text() {
                        Some(text) => {
                            logln!(
                                "[typst-live] 终端复制（右键）：{} 字 → 剪贴板",
                                text.chars().count()
                            );
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            right_terminal.clear_selection();
                        }
                        None => {
                            if let Some(text) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                            {
                                logln!(
                                    "[typst-live] 终端粘贴（右键）：{} 字 → pty",
                                    text.chars().count()
                                );
                                right_terminal.paste(&text);
                            }
                        }
                    }
                    window.refresh();
                });

            // 鼠标滚轮：滚动历史输出（向上滚看更早历史，向下滚回最新）
            let scroll_terminal = self.terminal.clone();
            let scroll_line_height = layout.line_height;
            self.interactivity
                .on_scroll_wheel(move |event: &ScrollWheelEvent, _window, _cx| {
                    let delta_lines = match event.delta {
                        ScrollDelta::Lines(d) => d.y as i32,
                        ScrollDelta::Pixels(d) => (d.y / scroll_line_height).round() as i32,
                    };
                    if delta_lines != 0 {
                        scroll_terminal.scroll(delta_lines);
                    }
                });

            self.interactivity.paint(
                global_id,
                inspector_id,
                bounds,
                layout.hitbox.as_ref(),
                window,
                cx,
                |_, window, cx| {
                    // 背景色区域
                    for rect in &layout.rects {
                        let r = Bounds::new(
                            point(
                                bounds.origin.x + rect.column as f32 * layout.cell_width,
                                bounds.origin.y + rect.line as f32 * layout.line_height,
                            ),
                            size(rect.cells as f32 * layout.cell_width, layout.line_height),
                        );
                        window.paint_quad(fill(r, rect.color));
                    }

                    // 合并文本
                    for run in &layout.runs {
                        let pos = point(
                            bounds.origin.x + run.column as f32 * layout.cell_width,
                            bounds.origin.y + run.line as f32 * layout.line_height,
                        );
                        let shaped = window.text_system().shape_line(
                            run.text.clone().into(),
                            layout.font_size,
                            std::slice::from_ref(&run.style),
                            Some(layout.cell_width),
                        );
                        let _ = shaped.paint(
                            pos,
                            layout.line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }

                    // 光标：反色方块
                    if let Some(c) = &layout.cursor {
                        let r = Bounds::new(
                            point(
                                bounds.origin.x + c.column as f32 * layout.cell_width,
                                bounds.origin.y + c.line as f32 * layout.line_height,
                            ),
                            size(layout.cell_width, layout.line_height),
                        );
                        window.paint_quad(fill(r, self.default_fg));

                        // IME 组合中文本：在光标处画出（带下划线），提交前不写入 PTY
                        if let Some(marked) = self.ime.read(cx).marked_text.as_ref()
                            && !marked.is_empty()
                        {
                            let pos = point(
                                bounds.origin.x + c.column as f32 * layout.cell_width,
                                bounds.origin.y + c.line as f32 * layout.line_height,
                            );
                            let marked_style = TextRun {
                                len: marked.len(),
                                font: Font {
                                    family: "Consolas".into(),
                                    ..Font::default()
                                },
                                color: self.default_fg,
                                background_color: None,
                                underline: Some(UnderlineStyle {
                                    color: Some(self.default_fg),
                                    thickness: px(1.),
                                    wavy: false,
                                }),
                                strikethrough: None,
                            };
                            let shaped = window.text_system().shape_line(
                                marked.clone().into(),
                                layout.font_size,
                                &[marked_style],
                                Some(layout.cell_width),
                            );
                            let _ = shaped.paint(
                                pos,
                                layout.line_height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                    }

                    // 键盘输入（仅在终端聚焦时处理）
                    let terminal = self.terminal.clone();
                    let focus_for_key = self.focus.clone();
                    window.on_key_event(move |event: &KeyDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        if !focus_for_key.is_focused(window) {
                            return;
                        }
                        let ctrl = event.keystroke.modifiers.control;
                        let shift = event.keystroke.modifiers.shift;
                        let key = event.keystroke.key.as_str();
                        let has_selection = terminal.selection_text().is_some();

                        // 复制：Ctrl+Shift+C / Ctrl+Insert，以及**有选区时的 Ctrl+C**
                        // （没有选区时 Ctrl+C 照旧发给 shell = 中断信号，不能无条件抢）。
                        let wants_copy = (ctrl && shift && key == "c")
                            || (ctrl && key == "insert")
                            || (ctrl && key == "c" && has_selection);

                        // 粘贴：Ctrl+V / Ctrl+Shift+V / Shift+Insert
                        // （跟 Windows Terminal 一致；Ctrl+V 在 shell 里本来是
                        //  readline 的「按字面插入下一个字符」，用得极少，让给粘贴）。
                        let wants_paste = (ctrl && key == "v")
                            || (ctrl && shift && key == "v")
                            || (shift && key == "insert");

                        if wants_copy {
                            match terminal.selection_text() {
                                Some(text) => {
                                    logln!(
                                        "[typst-live] 终端复制：{} 字 → 剪贴板",
                                        text.chars().count()
                                    );
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    window.refresh();
                                }
                                None => logln!("[typst-live] 终端复制：没有选中文本"),
                            }
                            return;
                        }
                        if wants_paste {
                            if let Some(text) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                            {
                                logln!("[typst-live] 终端粘贴：{} 字 → pty", text.chars().count());
                                terminal.paste(&text);
                            }
                            return;
                        }
                        if let Some(bytes) =
                            keystroke_to_bytes(&event.keystroke.key, &event.keystroke.modifiers)
                        {
                            terminal.write_input(&bytes);
                        }
                    });

                    // 文本输入（IME / 普通文本提交）
                    let terminal = self.terminal.clone();
                    let ime = self.ime.clone();
                    window.handle_input(&self.focus, TerminalTextHandler { terminal, ime }, cx);
                },
            );
        });
    }
}

/// 文本输入处理器：IME 组合态只标记、提交时才写入终端。
struct TerminalTextHandler {
    terminal: Rc<Terminal>,
    ime: Entity<ImeState>,
}

impl gpui::InputHandler for TerminalTextHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<gpui::UTF16Selection> {
        None
    }

    fn marked_text_range(
        &mut self,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<std::ops::Range<usize>> {
        self.ime
            .read(cx)
            .marked_text
            .as_ref()
            .map(|t| 0..t.encode_utf16().count())
    }

    fn text_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        // 提交（IME 选词确认 / 普通文本）：先清除组合态标记，再写进终端
        self.ime.update(cx, |s, cx| {
            s.marked_text = None;
            cx.notify();
        });
        if !text.is_empty() {
            self.terminal.write_input(text.as_bytes());
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        // IME 组合中间态：只标记显示，不写入 PTY（否则拼音/候选被当最终输入）
        self.ime.update(cx, |s, cx| {
            s.marked_text = if new_text.is_empty() {
                None
            } else {
                Some(new_text.to_string())
            };
            cx.notify();
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.ime.update(cx, |s, cx| {
            s.marked_text = None;
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_of(h: Hsla) -> [u8; 3] {
        let c = gpui::Rgba::from(h);
        [
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        ]
    }

    /// 终端渲染必须走主题调色板（回归：原先写死 X11 固定色，浅色主题下黄字看不清）
    #[test]
    fn named_colors_come_from_palette() {
        let light = TerminalPalette::light();
        let fg = gpui::hsla(0.0, 0.0, 0.2, 1.0);
        let bg = gpui::hsla(0.0, 0.0, 0.96, 1.0);
        for (named, expected) in [
            (NamedColor::Yellow, light.yellow),
            (NamedColor::BrightYellow, light.bright_yellow),
            (NamedColor::Red, light.red),
            (NamedColor::Cyan, light.cyan),
            (NamedColor::Green, light.green),
            (NamedColor::BrightWhite, light.bright_white),
        ] {
            let got = convert_color(&Color::Named(named), fg, bg, &light);
            assert_eq!(rgb_of(got), expected, "{named:?} 未按调色板取色");
        }
    }

    /// 256 色里前 16 个索引也要走调色板（程序常用 `\e[33m` / `38;5;3` 两种写法）
    #[test]
    fn indexed_first_16_use_palette() {
        let light = TerminalPalette::light();
        let fg = gpui::hsla(0.0, 0.0, 0.2, 1.0);
        let bg = gpui::hsla(0.0, 0.0, 0.96, 1.0);
        let yellow = convert_color(&Color::Indexed(3), fg, bg, &light);
        let bright_yellow = convert_color(&Color::Indexed(11), fg, bg, &light);
        assert_eq!(rgb_of(yellow), light.yellow);
        assert_eq!(rgb_of(bright_yellow), light.bright_yellow);
    }

    /// 256 色立方体（16 以上）保持 xterm 标准色，程序依赖它做精确配色
    #[test]
    fn indexed_cube_stays_standard() {
        let p = TerminalPalette::light();
        let fg = gpui::hsla(0.0, 0.0, 0.2, 1.0);
        let bg = gpui::hsla(0.0, 0.0, 0.96, 1.0);
        assert_eq!(
            rgb_of(convert_color(&Color::Indexed(16), fg, bg, &p)),
            [0, 0, 0]
        );
        assert_eq!(
            rgb_of(convert_color(&Color::Indexed(231), fg, bg, &p)),
            [255, 255, 255]
        );
    }

    /// 默认前景/背景仍跟随主题（不因调色板改动而变）
    #[test]
    fn default_fg_bg_follow_theme() {
        let p = TerminalPalette::dark();
        let fg = gpui::hsla(0.0, 0.0, 0.9, 1.0);
        let bg = gpui::hsla(0.0, 0.0, 0.1, 1.0);
        assert_eq!(
            rgb_of(convert_color(
                &Color::Named(NamedColor::Foreground),
                fg,
                bg,
                &p
            )),
            rgb_of(fg)
        );
        assert_eq!(
            rgb_of(convert_color(
                &Color::Named(NamedColor::Background),
                fg,
                bg,
                &p
            )),
            rgb_of(bg)
        );
    }
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    fn rgb_of(h: Hsla) -> [u8; 3] {
        let c = gpui::Rgba::from(h);
        [
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        ]
    }

    /// 纯黄在浅底上必须被压深（这就是 123.png 里看不清的那个颜色）
    #[test]
    fn clamp_fixes_pure_yellow_on_light_background() {
        let bg = rgb_to_hsla(0xf9, 0xf1, 0xf1);
        let yellow = rgb_to_hsla(0xff, 0xff, 0x00);
        let fixed = clamp_fg(yellow, bg, 4.5);
        let (f, b) = (rgb_of(fixed), rgb_of(bg));
        let ratio = crate::term_colors::contrast_ratio(f, b);
        assert!(ratio >= 4.5, "钳制后 {f:?} 对比度 {ratio:.2}");
        assert!(rgb_of(fixed) != rgb_of(yellow), "黄色未被压深，仍会看不清");
    }

    /// min_contrast <= 1.0 表示关闭：颜色原样保留
    #[test]
    fn clamp_disabled_keeps_color() {
        let bg = rgb_to_hsla(0xf9, 0xf1, 0xf1);
        let yellow = rgb_to_hsla(0xff, 0xff, 0x00);
        assert_eq!(rgb_of(clamp_fg(yellow, bg, 1.0)), rgb_of(yellow));
    }

    /// 已经达标的颜色不动
    #[test]
    fn clamp_keeps_readable_color() {
        let bg = rgb_to_hsla(0xf9, 0xf1, 0xf1);
        let blue = rgb_to_hsla(0x0f, 0x5c, 0xb0);
        assert_eq!(rgb_of(clamp_fg(blue, bg, 4.5)), rgb_of(blue));
    }

    /// 深色终端背景上，过暗的颜色要被提亮
    #[test]
    fn clamp_lightens_on_dark_background() {
        let bg = rgb_to_hsla(0x1e, 0x22, 0x27);
        let dark_blue = rgb_to_hsla(0x10, 0x18, 0x50);
        let fixed = rgb_of(clamp_fg(dark_blue, bg, 4.5));
        assert!(crate::term_colors::contrast_ratio(fixed, rgb_of(bg)) >= 4.5);
        assert!(fixed[2] > rgb_of(dark_blue)[2], "应变亮：{fixed:?}");
    }
}
