//! 工具栏那几个「一键插入」的纯逻辑。
//!
//! 与界面无关：给「文本 + 选区（字节）」→ 返回「改哪一段、改成什么、光标放哪」。
//! 所以能直接单测 —— 包括两个一眼看不出错的点：
//!
//! - **中文选区**：字节偏移必须落在字符边界上（`*中文*` 不是 `*中` 加半个字）
//! - **标题插到行首**：光标停在一行中间时，`= ` 该去行首而不是光标处
//!
//! 顺带记一条老账：Typst 的粗体是 `*粗*`，不是 Markdown 的 `**粗**`
//! （示例文档里写错过一次，编译报波浪线）。工具栏这点尤其不能搞混。

use std::ops::Range;

/// 工具栏上的按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Markup {
    Heading,
    Bold,
    Italic,
    Super,
    Sub,
    Math,
    Align,
    Color,
    Image,
    Table,
    Pagebreak,
}

impl Markup {
    /// 按钮顺序：常用的在前（对着 `wu` 的工具栏排的）。
    pub const ALL: [Self; 11] = [
        Self::Heading,
        Self::Bold,
        Self::Italic,
        Self::Super,
        Self::Sub,
        Self::Math,
        Self::Align,
        Self::Color,
        Self::Image,
        Self::Table,
        Self::Pagebreak,
    ];

    /// 按钮上的字。
    pub fn label(self) -> &'static str {
        match self {
            Self::Heading => "标题",
            Self::Bold => "粗",
            Self::Italic => "斜",
            Self::Super => "上标",
            Self::Sub => "下标",
            Self::Math => "公式",
            Self::Align => "对齐",
            Self::Color => "颜色",
            Self::Image => "图片",
            Self::Table => "表格",
            Self::Pagebreak => "分页",
        }
    }

    /// 悬停提示：把「会插入什么」直接写出来。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Heading => "插入 = （到行首）",
            Self::Bold => "包住选中：*粗体*",
            Self::Italic => "包住选中：_斜体_",
            Self::Super => "包住选中：上标",
            Self::Sub => "包住选中：下标",
            Self::Math => "包住选中：$ 公式 $",
            Self::Align => "包住选中：居中",
            Self::Color => "包住选中：红色文字",
            Self::Image => "插入图片（光标落在引号里）",
            Self::Table => "插入两列两行的表格骨架",
            Self::Pagebreak => "插入分页",
        }
    }
}

/// 表格骨架。
const TABLE: &str = "#table(\n  columns: 2,\n  [表头 1], [表头 2],\n  [单元格], [单元格],\n)";

/// 一次编辑：把 `range` 换成 `text`，然后把光标放到 `cursor`（字节）。
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    /// 要替换掉的字节范围。空范围 = 纯插入。
    pub range: Range<usize>,
    pub text: String,
    /// 改完之后光标该在的字节位置。
    pub cursor: usize,
}

impl Edit {
    /// 把这次编辑应用到文本上。**只有测试用** —— 界面上那次编辑交给
    /// gpui-component 的编辑器执行（`insert` / `replace`），不用自己拼。
    #[cfg(test)]
    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + self.text.len());
        out.push_str(&text[..self.range.start]);
        out.push_str(&self.text);
        out.push_str(&text[self.range.end..]);
        out
    }
}

/// 算出「一键插入」该做的那一次编辑。
///
/// `selection` 是字节范围（gpui-component 的 `selected_range()` 就是字节）；
/// 越界会被夹到合法范围，不会 panic。
pub fn edit(text: &str, selection: Range<usize>, kind: Markup) -> Edit {
    let start = selection.start.min(text.len());
    let end = selection.end.clamp(start, text.len());

    match kind {
        // ── 包起来：有选区就包住原文；没选区就给个空壳，光标夹在中间 ──
        Markup::Bold => wrap("*", "*", text, start..end),
        Markup::Italic => wrap("_", "_", text, start..end),
        Markup::Super => wrap("#super[", "]", text, start..end),
        Markup::Sub => wrap("#sub[", "]", text, start..end),
        Markup::Math => wrap("$ ", " $", text, start..end),
        Markup::Align => wrap("#align(center)[", "]", text, start..end),
        Markup::Color => wrap("#text(fill: red)[", "]", text, start..end),

        // ── 插入：光标处（标题是行首）放一段模板 ──
        Markup::Heading => insert(line_start(text, start), "= ", 2),
        Markup::Image => insert(start, "#image(\"\")", "#image(\"".len()),
        Markup::Table => insert(start, TABLE, TABLE.len()),
        Markup::Pagebreak => insert(start, "#pagebreak()", "#pagebreak()".len()),
    }
}

/// 包住一段文本。
fn wrap(prefix: &str, suffix: &str, text: &str, selection: Range<usize>) -> Edit {
    let inner = &text[selection.clone()];
    let empty = selection.start == selection.end;

    let mut out = String::with_capacity(prefix.len() + inner.len() + suffix.len());
    out.push_str(prefix);
    out.push_str(inner);
    out.push_str(suffix);

    Edit {
        // 有选区：光标落在原文末尾（接着打字就还在这个壳里）
        // 没选区：夹在 prefix 与 suffix 中间
        cursor: if empty {
            selection.start + prefix.len()
        } else {
            selection.start + prefix.len() + inner.len()
        },
        range: selection,
        text: out,
    }
}

/// 在某个位置插入一段文本，并把光标放到片段内的指定位置。
fn insert(at: usize, snippet: &str, cursor_in_snippet: usize) -> Edit {
    Edit {
        range: at..at,
        text: snippet.to_string(),
        cursor: at + cursor_in_snippet,
    }
}

/// 字节 `at` 所在行的行首字节偏移。
fn line_start(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空选区时每种「包起来」的光标都该夹在中间 —— 插完就能直接打字。
    #[test]
    fn an_empty_selection_puts_the_cursor_between_the_markers() {
        let edit = edit("abc", 3..3, Markup::Bold);

        assert_eq!(edit.text, "**");
        assert_eq!(edit.apply("abc"), "abc**");
        assert_eq!(edit.cursor, 4, "光标该在 `**` 中间");
    }

    /// 中文选区按**字节**算，但绝不能切在半个字上。
    #[test]
    fn wrapping_a_cjk_selection_keeps_every_character() {
        let text = "你好世界";
        // 四个汉字 = 12 字节（每个 3 字节）—— 这里最容易把字节当字符数写错
        let edit = edit(text, 0..12, Markup::Bold);

        assert_eq!(edit.text, "*你好世界*");
        assert_eq!(edit.apply(text), "*你好世界*");
        // 光标落在「世界」之后、`*` 之前
        assert_eq!(edit.cursor, "*你好世界".len());
    }

    #[test]
    fn wrapping_keeps_the_original_text_untouched() {
        let text = "前 中 后";
        let selection = text.find('中').unwrap();

        for kind in [Markup::Bold, Markup::Italic, Markup::Super, Markup::Math] {
            let edit = edit(text, selection..selection + '中'.len_utf8(), kind);
            let out = edit.apply(text);

            assert!(out.contains('中'), "{kind:?} 把选中的字搞丢了：{out:?}");
            assert!(
                out.starts_with("前 "),
                "{kind:?} 动了选区之前的内容：{out:?}"
            );
            assert!(out.ends_with(" 后"), "{kind:?} 动了选区之后的内容：{out:?}");
        }
    }

    /// 标题是**行首**插入，不是光标处 —— 光标停在一行中间时最容易被写错。
    #[test]
    fn the_heading_marker_goes_to_the_line_start() {
        let text = "第一行\n第二行";
        let at = text.find('二').unwrap(); // 第二行中间（字节 13）
        let line_start = text.find('\n').unwrap() + 1; // 第二行行首

        let edit = edit(text, at..at, Markup::Heading);

        assert_eq!(
            edit.range,
            line_start..line_start,
            "该插在第二行行首，不是光标处"
        );
        assert_eq!(edit.apply(text), "第一行\n= 第二行");
    }

    #[test]
    fn the_heading_marker_works_on_the_first_line() {
        let edit = edit("正文", 3..3, Markup::Heading);

        assert_eq!(edit.range, 0..0);
        assert_eq!(edit.apply("正文"), "= 正文");
    }

    #[test]
    fn the_image_button_leaves_the_cursor_inside_the_quotes() {
        let text = "";
        let edit = edit(text, 0..0, Markup::Image);
        let out = edit.apply(text);

        assert_eq!(out, "#image(\"\")");
        assert_eq!(&out[..edit.cursor], "#image(\"", "光标该在引号里");
    }

    #[test]
    fn the_table_button_inserts_a_skeleton() {
        let edit = edit("", 0..0, Markup::Table);

        assert!(edit.text.contains("#table("), "{}", edit.text);
        assert!(edit.text.contains("columns:"), "{}", edit.text);
        assert_eq!(edit.cursor, edit.text.len(), "光标落在骨架末尾");
    }

    /// 选区越界（或者刚好在末尾）不该 panic。
    #[test]
    fn an_out_of_range_selection_is_clamped() {
        // 反着的范围要构造出来，别写字面量（clippy 会拦 `3..1`）
        let (from, to) = (3usize, 1usize);

        for selection in [5..99, 99..99, from..to] {
            let edit = edit("abc", selection.clone(), Markup::Bold);
            let out = edit.apply("abc");

            assert!(out.contains("abc"), "{selection:?} 把原文弄坏了：{out:?}");
            assert!(edit.cursor <= out.len());
        }
    }

    /// 每种按钮都要能算出一次**自洽**的编辑：应用之后光标位置合法、
    /// 且落在一处能继续打字的边界上。
    #[test]
    fn every_button_produces_a_sane_edit() {
        let text = "一段正文，带中文。";
        let at = text.find('中').unwrap();

        for kind in Markup::ALL {
            let edit = edit(text, at..at + '中'.len_utf8(), kind);
            let out = edit.apply(text);

            assert!(
                out.is_char_boundary(edit.cursor),
                "{kind:?} 的光标落在了半个字符上：{out:?}"
            );
            assert!(
                edit.range.start <= edit.range.end,
                "{kind:?} 的范围反了：{edit:?}"
            );
        }
    }
}
