//! AI 这一轮改的是**哪儿**、结果又**落在哪儿**。
//!
//! 五个入口、五种范围（前四种在 AI 浮层里点小按钮切换，参考 `wu` 的
//! `AiEditScope`；第五种只有目录树右键那一个入口）：
//!
//! | 范围 | 上下文给模型什么 | 结果落在哪儿 |
//! |---|---|---|
//! | 选区 | 选中的那段 | 替换这段 |
//! | 光标段落 | 光标所在行上下各 `PARA_PAD` 行 | 替换这一段 |
//! | 全文 | 整篇（超长截断） | 覆盖全文 |
//! | **插入** | 光标段落（**只是位置参照**） | **插在光标处，原文一个字不动** |
//! | 一个文件 | 那个文件的内容 | 写回那个文件 |
//!
//! 单独成模块的理由与 `autosave` / `disk_watch` 一样：**算错范围**或**落错地方**
//! 的后果是改坏用户的文件。所以这里全是纯函数（段落切分、换行统计、拼接、
//! 落点判定），每条边界都有单测钉住。

use std::ops::Range;
use std::path::{Path, PathBuf};

/// 光标段落上下文：上下各留多少行。
///
/// 与 `wu` 的 `AI_EDIT_PARA_PAD` 取同一个值（5）：太小了模型看不到上下文，
/// 太大了等于「全文」—— 这一段写的东西往往要跟前后的语气、术语对齐。
pub const PARA_PAD: usize = 5;

/// 这一轮 AI 的作用范围。
#[derive(Clone, Debug)]
pub enum AiScope {
    /// 编辑器里的选中文字（字节范围）。
    Selection(Range<usize>),
    /// 光标所在段落（上下各 [`PARA_PAD`] 行，字节范围）。
    Paragraph(Range<usize>),
    /// 编辑器里的整篇文档。
    Document,
    /// **插入**：AI 生成的内容放在光标处，原文一个字不动。
    ///
    /// 上下文仍旧取光标段落 —— 但那是给模型看「你写到哪儿了」，
    /// 不是要它改写那一段（提示词里专门交代了，见 `ai::INSERT_HINT`）。
    Insert { at: usize },
    /// 磁盘上的一个文件。
    ///
    /// 可以是编辑器里打开着的那个（那就一切以**编辑器里的文本**为准 —— 它可能
    /// 带着没按 Ctrl+S 的改动），也可以是别的文件。
    File(PathBuf),
}

/// 浮层上给用户点的几个按钮。
///
/// 与 [`AiScope`] 分开：那个是「已经算好的范围」（带字节偏移 / 路径），
/// 这个是「选择」——点一下才去算。有选区时才会出现「选区」那一个。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeChoice {
    Selection,
    Paragraph,
    Document,
    Insert,
}

impl ScopeChoice {
    /// 按钮上的字。
    ///
    /// 「插入」这个名字是自取的：它说的不是**改哪儿**而是**内容放哪儿** ——
    /// 在光标处插入新内容，原文一个字不动。与 `wu` 的「插入到光标」同一个意思。
    pub fn label(self) -> &'static str {
        match self {
            Self::Selection => "选区",
            Self::Paragraph => "光标段落",
            Self::Document => "全文",
            Self::Insert => "插入",
        }
    }
}

/// 结果该落到哪儿。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// 替换编辑器里的这一段（选区 / 光标段落）。
    Replace(Range<usize>),
    /// 覆盖编辑器里的整篇。
    WholeDocument,
    /// 在编辑器里的这个偏移处插入。
    Insert { at: usize },
    /// 写回磁盘上的一个文件。
    File(PathBuf),
}

/// 这一轮回结果落在哪里。
///
/// `File(main_path)` 判成 `WholeDocument` 是这里唯一的弯：**编辑器里那一份才是真相**。
/// 用户可能刚在里面敲了两句还没保存，绕过它去读写盘等于把那两句丢掉 ——
/// 与 `open_in_editor` 拒绝在有未保存改动时换文件是同一条规矩。
pub fn landing(scope: &AiScope, main_path: &Path) -> Landing {
    match scope {
        AiScope::Selection(range) | AiScope::Paragraph(range) => Landing::Replace(range.clone()),
        AiScope::Document => Landing::WholeDocument,
        AiScope::Insert { at } => Landing::Insert { at: *at },
        AiScope::File(path) if path == main_path => Landing::WholeDocument,
        AiScope::File(path) => Landing::File(path.clone()),
    }
}

/// 把字节偏移夹到 UTF-8 字符边界（切开一个汉字就是 panic）。
pub fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// 光标所在段落：包含光标那一行、上下各 `pad` 行，返回**字节范围**。
///
/// 与 `wu::paragraph_at` 同一套切法（按 `\n` 数行、只取行不取字符），
/// 只是这里返回范围而不是文本 —— 因为结果要**替换这一段**，得知道它从哪儿到哪儿。
pub fn paragraph_range(doc: &str, cursor: usize, pad: usize) -> Range<usize> {
    let cursor = floor_char_boundary(doc, cursor);

    // 每一行的起点（第一行恒为 0）。空文档就是 `[0]`
    let mut starts = vec![0usize];
    for (ix, byte) in doc.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(ix + 1);
        }
    }

    // 光标落在哪一行：最后一个「起点 ≤ 光标」的行
    let row = starts
        .partition_point(|start| *start <= cursor)
        .saturating_sub(1);
    let first = row.saturating_sub(pad);
    let last = row + pad;

    let from = starts[first];
    let to = starts.get(last + 1).copied().unwrap_or(doc.len());
    from..to
}

/// 一个字节偏移在第几行（1 起，给界面看的）。
pub fn line_at(doc: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(doc, offset);
    1 + doc[..offset].bytes().filter(|b| *b == b'\n').count()
}

/// 一段字覆盖第几到第几行（1 起、含两端）。
pub fn line_span(doc: &str, range: &Range<usize>) -> (usize, usize) {
    let from = line_at(doc, range.start);
    // 末尾落在下一行的行首时（段落正好到行尾）要退一个字符，否则会多算一行
    let end = floor_char_boundary(doc, range.end);
    let end = if end > range.start { end - 1 } else { end };
    (from, line_at(doc, end))
}

/// 把 `text` 放进 `doc` 的 `range` 处（`range` 为空 = 就是插入），
/// 顺带算出**应用之后光标该在哪儿**。
///
/// 越界一律夹住、并停在字符边界上：光标偏移可能落在多字节字符中间
/// （输入法合成、外部改动重读都可能造成），切开一个汉字就是 panic。
pub fn splice_at(doc: &str, range: Range<usize>, text: &str) -> (String, usize) {
    let start = floor_char_boundary(doc, range.start);
    let end = floor_char_boundary(doc, range.end).max(start);

    let mut out = String::with_capacity(doc.len() + text.len());
    out.push_str(&doc[..start]);
    out.push_str(text);
    out.push_str(&doc[end..]);

    // 光标落在插入内容之后 —— 接着往下写就是从这里开始
    (out, start + text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> String {
        // 8 行：第 1..=8 行分别是 a..h
        "a\nb\nc\nd\ne\nf\ng\nh\n".to_string()
    }

    #[test]
    fn paragraph_takes_pad_lines_around_the_cursor() {
        let doc = doc();
        // 光标在第 4 行（"d" 的偏移 6），上下各 2 行 → 第 2..=6 行
        let range = paragraph_range(&doc, 6, 2);
        assert_eq!(&doc[range], "b\nc\nd\ne\nf\n");
    }

    #[test]
    fn paragraph_clamps_at_the_document_edges() {
        let doc = doc();
        // 光标在第 1 行：不能往前越界
        assert_eq!(&doc[paragraph_range(&doc, 0, 5)], "a\nb\nc\nd\ne\nf\n");
        // 光标在最后一行（末尾那个 `\n`）：下面没得越，上面最多 5 行
        assert_eq!(
            &doc[paragraph_range(&doc, doc.len() - 1, 5)],
            "c\nd\ne\nf\ng\nh\n"
        );
        // 光标在文档末尾（往后追加的场景）：给最后两行当上下文
        assert_eq!(&doc[paragraph_range(&doc, doc.len(), 2)], "g\nh\n");
    }

    #[test]
    fn paragraph_of_an_empty_document_is_empty() {
        assert_eq!(paragraph_range("", 0, 5), 0..0);
    }

    #[test]
    fn paragraph_on_a_never_newline_document_is_the_whole_thing() {
        let doc = "一整行没有换行";
        assert_eq!(paragraph_range(doc, 6, 5), 0..doc.len());
    }

    #[test]
    fn paragraph_survives_a_cursor_inside_a_character() {
        // 偏移 1 落在「排」的第二个字节上：得退到字符边界，不能 panic
        let doc = "排版\n第二行\n";
        let range = paragraph_range(doc, 1, 0);
        assert_eq!(&doc[range], "排版\n");
    }

    #[test]
    fn paragraph_with_crlf_lines() {
        let doc = "a\r\nb\r\nc\r\n";
        // 光标在第 2 行（偏移 3）
        assert_eq!(&doc[paragraph_range(doc, 3, 0)], "b\r\n");
    }

    #[test]
    fn line_numbers_are_one_based_and_inclusive() {
        let doc = doc();
        assert_eq!(line_at(&doc, 0), 1);
        assert_eq!(line_at(&doc, 2), 2); // "b"
        assert_eq!(line_at(&doc, doc.len()), 9); // 末尾那个空行

        // 第 2..=4 行
        let range = paragraph_range(&doc, 4, 1);
        assert_eq!(&doc[range.clone()], "b\nc\nd\n");
        assert_eq!(line_span(&doc, &range), (2, 4));
    }

    #[test]
    fn empty_range_reports_a_single_line() {
        let doc = doc();
        assert_eq!(line_span(&doc, &(6..6)), (4, 4));
        assert_eq!(line_span("", &(0..0)), (1, 1));
    }

    #[test]
    fn splice_replaces_the_range_and_reports_the_caret() {
        let (out, caret) = splice_at("abcdef", 2..4, "XY");
        assert_eq!(out, "abXYef");
        assert_eq!(caret, 4); // 光标停在 "XY" 之后
    }

    #[test]
    fn splice_with_an_empty_range_is_an_insert() {
        let (out, caret) = splice_at("abef", 2..2, "CD");
        assert_eq!(out, "abCDef");
        assert_eq!(caret, 4);
    }

    #[test]
    fn splice_clamps_out_of_range_and_character_boundaries() {
        // 越界：夹到末尾
        let (out, caret) = splice_at("abc", 99..200, "Z");
        assert_eq!(out, "abcZ");
        assert_eq!(caret, 4);
        // 汉字中间：退到字符边界，不 panic
        let doc = "排版中文";
        let at = floor_char_boundary(doc, 1);
        assert_eq!(at, 0);
        let (out, _) = splice_at(doc, at..at, "【");
        assert_eq!(out, "【排版中文");
    }

    #[test]
    fn selection_and_paragraph_replace_in_place() {
        let main = Path::new("D:/w/正文.typ");
        assert_eq!(
            landing(&AiScope::Selection(3..9), main),
            Landing::Replace(3..9)
        );
        assert_eq!(
            landing(&AiScope::Paragraph(10..50), main),
            Landing::Replace(10..50)
        );
    }

    #[test]
    fn document_and_insert() {
        let main = Path::new("D:/w/正文.typ");
        assert_eq!(landing(&AiScope::Document, main), Landing::WholeDocument);
        assert_eq!(
            landing(&AiScope::Insert { at: 7 }, main),
            Landing::Insert { at: 7 }
        );
    }

    #[test]
    fn the_open_file_is_the_editor_version() {
        // 目录树里点的正是打开着的那个文件：走编辑器，别去读写盘
        let path = PathBuf::from("D:/w/正文.typ");
        assert_eq!(
            landing(&AiScope::File(path.clone()), &path),
            Landing::WholeDocument
        );
    }

    #[test]
    fn another_file_is_written_where_it_lives() {
        let other = PathBuf::from("D:/w/附录.typ");
        assert_eq!(
            landing(&AiScope::File(other.clone()), Path::new("D:/w/正文.typ")),
            Landing::File(other)
        );
    }

    #[test]
    fn scope_buttons_are_labelled() {
        assert_eq!(ScopeChoice::Selection.label(), "选区");
        assert_eq!(ScopeChoice::Paragraph.label(), "光标段落");
        assert_eq!(ScopeChoice::Document.label(), "全文");
        assert_eq!(ScopeChoice::Insert.label(), "插入");
    }
}
