//! 行级 diff：AI 改稿在「应用之前」的变更预览。
//!
//! 移植自参考项目 `wu` 的 `src/diff.rs`。三件事：
//!
//! 1. `hunks()` 把 diff 切成**最大连续变更块** —— AI 的修改往往散落在几处，
//!    用户该能一处一处地接受/拒绝，而不是「全要或全不要」
//! 2. `apply_hunks()` 按接受标记重建文本
//! 3. 都是纯函数，所以能直接单测（下面那几条）
//!
//! 两条硬要求（第一版都错了，靠自检抓出来的）：
//!
//! - **行尾风格要跟着原文走**：编辑器里的文本是 CRLF，重建时若写死换行符，
//!   一次 AI 改稿就会把整篇的行尾换掉（git 上看起来就是「全文改了」）
//! - **「全部拒绝」必须逐字节等于原文**：包括结尾有没有换行。
//!   不然用户点了拒绝还会看到文件被改（只差一个换行也算改了）

use similar::{ChangeTag, TextDiff};

/// 一行 diff 的类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    /// 新增行
    Add,
    /// 删除行
    Del,
}

/// 一行 diff。
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// 一个变更块：最大连续的新增/删除行（不含未变的上下文）。
#[derive(Clone, Debug)]
pub struct Hunk {
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// 该块新增了几行。
    pub fn added(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.kind == DiffKind::Add)
            .count()
    }

    /// 该块删掉了几行。
    pub fn removed(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.kind == DiffKind::Del)
            .count()
    }
}

/// 取一行的内容（丢掉行尾换行符）。
fn line_of(change: &similar::Change<&str>) -> String {
    change.value().trim_end_matches(['\n', '\r']).to_string()
}

/// 按「最大连续变更块」切分 diff，用于逐块接受/拒绝。
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(old, new);
    let mut out: Vec<Hunk> = Vec::new();
    let mut current: Vec<DiffLine> = Vec::new();

    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => None,
            ChangeTag::Insert => Some(DiffKind::Add),
            ChangeTag::Delete => Some(DiffKind::Del),
        };

        match kind {
            Some(kind) => current.push(DiffLine {
                kind,
                text: line_of(&change),
            }),
            None => {
                if !current.is_empty() {
                    out.push(Hunk {
                        lines: std::mem::take(&mut current),
                    });
                }
            }
        }
    }

    if !current.is_empty() {
        out.push(Hunk { lines: current });
    }
    out
}

/// 按接受标记重建文本：`accepted[i] == false` 的块保留旧内容。
///
/// 标记不够长时按「接受」处理（多出来的块默认全要 —— 这是 AI 改稿的默认意图）。
///
/// 行尾跟着 `old` 走（CRLF 就还 CRLF），结尾换行也跟 `old` 一致 ——
/// 这样「全部拒绝」能保证逐字节等于原文。
pub fn apply_hunks(old: &str, new: &str, accepted: &[bool]) -> String {
    let eol = if old.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing = old.ends_with('\n');

    let diff = TextDiff::from_lines(old, new);
    let mut lines: Vec<String> = Vec::new();
    let mut hunk_index = 0usize;
    let mut in_hunk = false;

    for change in diff.iter_all_changes() {
        let text = line_of(&change);
        let this_hunk = hunk_index;

        match change.tag() {
            ChangeTag::Equal => {
                if in_hunk {
                    in_hunk = false;
                    hunk_index += 1;
                }
                lines.push(text);
            }
            ChangeTag::Delete => {
                in_hunk = true;
                if !accepted.get(this_hunk).copied().unwrap_or(true) {
                    lines.push(text);
                }
            }
            ChangeTag::Insert => {
                in_hunk = true;
                if accepted.get(this_hunk).copied().unwrap_or(true) {
                    lines.push(text);
                }
            }
        }
    }

    // `from_lines` 会把末尾换行变成「最后一行是空的」，这里统一收掉再按需要补回
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    let mut out = lines.join(eol);
    if trailing && !out.is_empty() {
        out.push_str(eol);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "1\n2\n3\n4\n5\n";
    const NEW: &str = "1\nX\n3\n4\nY\n";

    #[test]
    fn hunks_split_scattered_changes() {
        let found = hunks(OLD, NEW);

        assert_eq!(found.len(), 2, "两处改动该切成两块：{found:?}");
        assert_eq!((found[0].added(), found[0].removed()), (1, 1));
        assert_eq!((found[1].added(), found[1].removed()), (1, 1));
    }

    #[test]
    fn no_change_means_no_hunks() {
        assert!(hunks("a\n", "a\n").is_empty());
    }

    #[test]
    fn apply_hunks_accept_and_reject_each_block() {
        assert_eq!(apply_hunks(OLD, NEW, &[true, true]), NEW, "全接受 = 新文本");
        assert_eq!(
            apply_hunks(OLD, NEW, &[false, false]),
            OLD,
            "全拒绝 = 旧文本"
        );
        assert_eq!(apply_hunks(OLD, NEW, &[true, false]), "1\nX\n3\n4\n5\n");
        assert_eq!(apply_hunks(OLD, NEW, &[false, true]), "1\n2\n3\n4\nY\n");
    }

    /// 标记给少了：多出来的块按「接受」算（AI 改稿的默认意图是改）。
    #[test]
    fn missing_marks_default_to_accepted() {
        assert_eq!(apply_hunks(OLD, NEW, &[]), NEW);
        assert_eq!(apply_hunks(OLD, NEW, &[false]), "1\n2\n3\n4\nY\n");
    }

    /// 纯新增（没有删除）也能算出一块。
    #[test]
    fn a_pure_insertion_is_one_hunk() {
        let found = hunks("a\n", "a\nb\n");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].added(), 1);
        assert_eq!(found[0].removed(), 0);
    }

    /// ★「全部拒绝」必须**逐字节**等于原文 —— 包括行尾与结尾换行。
    ///
    /// 第一版重建时写死换行符又无条件补一个结尾换行，于是两条都错：
    /// 编辑器里的 CRLF 会被整篇换成 LF，而「拒绝」也会改到文件。
    #[test]
    fn rejecting_everything_reproduces_the_original_byte_for_byte() {
        let crlf = "1\r\n2\r\n3\r\n";
        let changed = "1\r\nX\r\n3\r\n";
        assert_eq!(
            apply_hunks(crlf, changed, &[false]),
            crlf,
            "CRLF 要原样还回来"
        );

        let no_trailing = "1\n2\n3";
        assert_eq!(
            apply_hunks(no_trailing, "1\nX\n3", &[false]),
            no_trailing,
            "原文结尾没有换行，就别自己补一个"
        );

        let crlf_no_trailing = "1\r\n2\r\n3";
        assert_eq!(
            apply_hunks(crlf_no_trailing, "1\r\nX\r\n3", &[false]),
            crlf_no_trailing
        );
    }

    /// 接受修改时行尾风格也要跟着原文，否则一次 AI 改稿会把整篇 CRLF 换掉。
    #[test]
    fn accepting_keeps_the_original_line_ending() {
        let out = apply_hunks("1\r\n2\r\n", "1\r\nX\r\n", &[true]);

        assert_eq!(out, "1\r\nX\r\n");
    }

    /// 中文与空行都不该把块切错。
    #[test]
    fn cjk_and_blank_lines_are_handled() {
        let old = "第一段\n\n第二段\n";
        let new = "第一段改过\n\n第二段\n\n第三段\n";

        let found = hunks(old, new);
        assert!(!found.is_empty(), "该有改动");
        assert_eq!(apply_hunks(old, new, &[true; 4]), new);
    }
}
