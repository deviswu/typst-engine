//! 编辑器里的**底色标记**：错误行整行染色 + 配对括号高亮。
//!
//! # 为什么走 documentColor 这条路
//!
//! gpui-component 的 `DocumentColorProvider`（LSP 的 textDocument/documentColor）
//! 拿到的颜色会被画成**文字底下的色块**（库内部叫 `bg_segments`），
//! 而且完全在公开 API 内 —— 不用 fork 库。用诊断（波浪线）做不到这件事：
//! 那是下划线，不是底色；想在**行号列**上标红则必须改库的 `element.rs`。
//!
//! # 两条已知限制（写在这里，免得后来者以为坏了）
//!
//! 1. 库只在**文本变化**后重新要一次颜色（`InputState::render` 里的
//!    `_pending_update` 分支）。所以纯移动光标不会让「配对括号」换色 ——
//!    打字时是准的，移动后要等下一次编辑/重排。
//! 2. 大文档跳过括号配对（见 `BRACKET_SCAN_LIMIT`）：每帧扫全文不划算。

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui::{Hsla, Rgba, Task, Window};
use gpui_component::input::{DocumentColorProvider, RopeExt as _};
use lsp_types::{Color, ColorInformation, Position, Range as LspRange};
use ropey::Rope;

/// 超过这个字节数就不做括号配对。
const BRACKET_SCAN_LIMIT: usize = 512 * 1024;

/// 最多返回多少条颜色（库会按可见范围再筛一次，这里只是兜底）。
const MAX_MARKS: usize = 4096;

/// 标记的共享状态：**外壳往里写**（错误行、光标、配色），provider 往里读。
///
/// 配色由外壳从主题里取 —— 这个模块不该知道主题长什么样。
#[derive(Default)]
pub struct MarkState {
    /// 错误行（0 起，闭区间 `[起, 止]`）
    pub error_lines: Vec<(usize, usize)>,
    /// 警告行
    pub warning_lines: Vec<(usize, usize)>,
    /// 光标字节偏移（配对括号高亮用）
    pub cursor: usize,
    pub error_color: Option<Hsla>,
    pub warning_color: Option<Hsla>,
    /// 三档括号底色（按嵌套深度轮换）
    /// 光标所在那一对的强调色
    pub bracket_active: Option<Hsla>,
}

pub struct EditorMarks {
    pub state: Rc<RefCell<MarkState>>,
}

impl DocumentColorProvider for EditorMarks {
    fn document_colors(
        &self,
        text: &Rope,
        _window: &mut Window,
        _cx: &mut gpui::App,
    ) -> Task<Result<Vec<ColorInformation>>> {
        let state = self.state.borrow();
        let mut out: Vec<ColorInformation> = Vec::new();

        // 所有范围都**夹到字符边界**上再交出去。
        //
        // 这些偏移本来就是按行/按括号算的（落在边界上），但底下的排版引擎是按
        // 字节切片的：万一有个范围落在汉字中间，gpui 的 DirectWrite 会直接 panic
        // （日志里见过 `end byte index 11 is not a char boundary`）。
        // 多夹一次，换「最多少画一点底色，绝不崩」。
        let clamp = |offset: usize| {
            let offset = offset.min(text.len());
            if text.is_char_boundary(offset) {
                offset
            } else {
                text.floor_char_boundary(offset)
            }
        };
        let push_range = |out: &mut Vec<ColorInformation>, from: usize, to: usize, color: Hsla| {
            let (from, to) = (clamp(from), clamp(to));
            if out.len() >= MAX_MARKS || to <= from {
                return;
            }
            let start = text.offset_to_position(from);
            let end = text.offset_to_position(to);
            out.push(ColorInformation {
                range: LspRange {
                    start: Position::new(start.line, start.character),
                    end: Position::new(end.line, end.character),
                },
                color: to_lsp(color),
            });
        };

        // ① 错误行 / 警告行：整行的字节范围
        for (color, lines) in [
            (state.error_color, &state.error_lines),
            (state.warning_color, &state.warning_lines),
        ] {
            let Some(color) = color else { continue };
            for (first, last) in lines {
                let from = text.line_start_offset(*first);
                let to = text.line_end_offset(*last);
                push_range(&mut out, from, to, color);
            }
        }

        // ② 配对括号：**只给光标所在（或紧邻）的那一对**上底色。
        //
        // 所有括号都铺一层背景会很花 —— 括号本身已经有自己的前景色了
        // （`themes.rs::tint_brackets`），底色只负责「此刻在哪一对里面」。
        if text.len() <= BRACKET_SCAN_LIMIT {
            let source = text.to_string();
            let pairs = bracket_pairs(&source);

            if let Some((open, close)) = active_pair(&pairs, state.cursor)
                && let Some(color) = state.bracket_active
            {
                push_range(&mut out, open, open + 1, color);
                push_range(&mut out, close, close + 1, color);
            }
        }

        Task::ready(Ok(out))
    }
}

/// 光标所在（或紧邻）的那一对括号 —— 取**最内层**的那个。
///
/// 两条规则，按顺序：
/// 1. 光标**踩在某个括号上**（`cursor == 开` 或 `cursor == 闭`）→ 就是那一对；
///    同时踩到嵌套的两层时取更内层。
/// 2. 否则取**包住光标的最内层**那一对（`开 < 光标 <= 闭`）。
///
/// 「紧邻」的两种常见位置都落在规则 2 里：光标在 `(` 右边一格，或者在最内层的
/// 内容中间 —— 那时最内层那一对正是用户想看的。
fn active_pair(pairs: &[(usize, usize, usize)], cursor: usize) -> Option<(usize, usize)> {
    let innermost = |candidates: &mut dyn Iterator<Item = &(usize, usize, usize)>| {
        candidates
            .min_by_key(|pair: &&(usize, usize, usize)| pair.1 - pair.0)
            .map(|(open, close, _)| (*open, *close))
    };

    // ① 踩在括号上
    if let Some(pair) = innermost(
        &mut pairs
            .iter()
            .filter(|(open, close, _)| cursor == *open || cursor == *close),
    ) {
        return Some(pair);
    }

    // ② 被包住 → 最内层
    innermost(
        &mut pairs
            .iter()
            .filter(|(open, close, _)| *open < cursor && cursor <= *close),
    )
}

/// 括号配对：返回 `(开括号字节, 闭括号字节, 嵌套深度)`。
///
/// 跳过注释、字符串与原始串里的括号（那些不是语法括号）。
/// 按字节扫：多字节字符的字节都 ≥ 0x80，不会撞上我们匹配的 ASCII，所以安全。
fn bracket_pairs(source: &str) -> Vec<(usize, usize, usize)> {
    let bytes = source.as_bytes();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut pairs = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            // 行注释
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // 块注释（Typst 的块注释可以嵌套）
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            // 字符串（含转义）
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            // 原始串（`` ` `` 与 ```` ``` ````）：跳到下一个反引号即可
            b'`' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1
                }
                i += 1;
            }
            b'(' | b'[' | b'{' => {
                stack.push((bytes[i], i));
                i += 1;
            }
            b')' | b']' | b'}' => {
                let want = match bytes[i] {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if let Some((open, pos)) = stack.pop()
                    && open == want
                {
                    pairs.push((pos, i, stack.len()));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }

    pairs
}

/// `Hsla` → LSP 的 srgb 颜色（0..1 的 f32，alpha 保留 —— 库会把它当底色画）。
fn to_lsp(color: Hsla) -> Color {
    let rgba: Rgba = color.into();
    Color {
        red: rgba.r,
        green: rgba.g,
        blue: rgba.b,
        alpha: rgba.a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depths(pairs: &[(usize, usize, usize)]) -> Vec<usize> {
        pairs.iter().map(|(_, _, d)| *d).collect()
    }

    #[test]
    fn pairs_are_found_with_depth() {
        let pairs = bracket_pairs("f(g(x))");

        assert_eq!(pairs.len(), 2);
        // 顺序是**按闭合先后**：内层先闭，所以内层排在前面、深度 1，外层深度 0
        assert_eq!(depths(&pairs), vec![1, 0]);
    }

    /// 注释与字符串里的括号不算语法括号。
    #[test]
    fn brackets_in_comments_and_strings_are_ignored() {
        assert!(bracket_pairs("// ( 只有左括号\n").is_empty());
        assert!(bracket_pairs("/* ( */").is_empty());
        assert!(bracket_pairs("\"( 括号在字符串里\"").is_empty());
        assert!(bracket_pairs("`( 原始串 `").is_empty());
    }

    /// 嵌套的块注释也要正确跳过。
    #[test]
    fn nested_block_comments_are_skipped() {
        assert!(bracket_pairs("/* ( /* ) */ */").is_empty());
    }

    /// 跨行的括号（表格那种）要成对：外层 `(` 一对，两个 `[...]` 各一对。
    #[test]
    fn multiline_pairs_work() {
        let pairs = bracket_pairs("#table(\n  columns: 3,\n  [a], [b],\n)\n");

        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn unmatched_brackets_do_not_pair() {
        assert!(bracket_pairs("f(1, 2").is_empty());
        assert!(bracket_pairs("f1, 2)").is_empty());
        // 类型不匹配也不算配对
        assert!(bracket_pairs("f(1]").is_empty());
    }

    #[test]
    fn cursor_picks_the_innermost_pair() {
        // 偏移：f(0) ((1) g(2) ((3) x(4) )(5) )(6)
        let pairs = bracket_pairs("f(g(x))");

        // 光标踩在括号上 → 那一对
        assert_eq!(active_pair(&pairs, 3), Some((3, 5)), "内层 `(`");
        assert_eq!(active_pair(&pairs, 6), Some((1, 6)), "外层 `)`");
        // 光标在最内层的内容里 → 最内层
        assert_eq!(active_pair(&pairs, 4), Some((3, 5)));
        // 光标在外层里、内层外（`g` 上）→ 外层
        assert_eq!(active_pair(&pairs, 2), Some((1, 6)));
        // 光标在所有括号之外
        assert_eq!(active_pair(&pairs, 0), None);
        assert_eq!(active_pair(&pairs, 8), None);
    }

    /// 中文文档不会因为字节偏移错位而 panic（偏移落在字符中间也安全）。
    #[test]
    fn multibyte_text_is_safe() {
        let pairs = bracket_pairs("#table([中文], [英文])");

        assert_eq!(pairs.len(), 3, "外层 + 两个 [");
        let _ = active_pair(&pairs, 8);
    }

    #[test]
    fn color_conversion_keeps_alpha() {
        let color = to_lsp(gpui::hsla(0.5, 0.5, 0.5, 0.25));

        assert!((color.alpha - 0.25).abs() < 1e-3, "alpha 要保留：{color:?}");
    }
}
