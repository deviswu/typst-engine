//! 文档大纲。

use std::ops::Range;

use typst::syntax::{LinkedNode, Source, SyntaxKind, ast};

/// 大纲里的一条标题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    /// 层级：`=` 的个数，从 1 起。
    pub depth: usize,
    /// 标题文字（已去掉前导 `=` 与首尾空白）。
    pub title: String,
    /// 标题在源码里的字节范围（**含**前导 `=`）。
    pub range: Range<usize>,
    /// 0 起的行号。给「点大纲跳到源码」用。
    pub line: usize,
}

/// 按文档顺序取出所有标题。
///
/// 消费方应当把**引擎里那棵增量维护的 `Source`** 传进来（
/// `typst::World::source(&world, main)`），这样每次敲键只花一次树遍历，
/// 不重新解析。若自己 `Source::detached(text)`，那就是每次全量 parse。
pub fn outline(source: &Source) -> Vec<OutlineItem> {
    // 行号索引**建一次**，而不是每个标题各自从文本开头数一遍换行。
    //
    // 这里踩过一次坑：初版 `line_of()` 每个标题都扫全文，于是
    // 「751 个标题 × 51 KB」退化成 O(n²)，实测 **106 ms**，远超一帧。
    // `tests/perf.rs` 把它揪了出来。改成一次性建索引后是 O(n + k·log n)。
    let text = source.text();
    let line_starts = line_starts(text);

    let mut out = Vec::new();
    visit(LinkedNode::new(source.root()), text, &line_starts, &mut out);
    out
}

/// 每一行行首的字节偏移。第一个元素恒为 0。
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        text.as_bytes()
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// 字节偏移 → 0 起的行号。
fn line_of(line_starts: &[usize], byte: usize) -> usize {
    line_starts
        .partition_point(|&s| s <= byte)
        .saturating_sub(1)
}

fn visit(node: LinkedNode<'_>, text: &str, line_starts: &[usize], out: &mut Vec<OutlineItem>) {
    if node.kind() == SyntaxKind::Heading
        && let Some(item) = as_outline_item(&node, text, line_starts)
    {
        out.push(item);
    }

    for child in node.children() {
        visit(child, text, line_starts, out);
    }
}

fn as_outline_item(
    node: &LinkedNode<'_>,
    text: &str,
    line_starts: &[usize],
) -> Option<OutlineItem> {
    // `cast` 要求 `&'a SyntaxNode`，所以走 `get()` 而不是 Deref
    // —— 后者拿到的是更短的借用期。
    let heading = node.get().cast::<ast::Heading>()?;

    let range = node.range();
    let raw = text.get(range.clone())?;
    let title = raw.trim_start_matches('=').trim().to_owned();

    Some(OutlineItem {
        depth: heading.depth().get(),
        title,
        line: line_of(line_starts, range.start),
        range,
    })
}

#[cfg(test)]
mod tests {
    use typst::syntax::Source;

    use super::*;

    fn outline_of(text: &str) -> Vec<OutlineItem> {
        outline(&Source::detached(text))
    }

    #[test]
    fn finds_headings_in_document_order() {
        let items = outline_of("= One\n\nBody.\n\n== Two\n\nMore.\n\n= Three\n");

        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["One", "Two", "Three"]);
    }

    #[test]
    fn depth_comes_from_the_number_of_equals() {
        let items = outline_of("= A\n\n== B\n\n=== C\n\n==== D\n");

        let depths: Vec<_> = items.iter().map(|i| i.depth).collect();
        assert_eq!(depths, [1, 2, 3, 4]);
    }

    #[test]
    fn line_numbers_are_zero_based() {
        // 第 0 行是标题，第 1 行空，第 2 行是第二个标题
        let items = outline_of("= First\n\n== Second\n");

        assert_eq!(items[0].line, 0);
        assert_eq!(items[1].line, 2);
    }

    #[test]
    fn the_range_covers_the_equals_signs() {
        let source = Source::detached("= Title\n");
        let items = outline(&source);

        assert_eq!(&source.text()[items[0].range.clone()], "= Title");
    }

    #[test]
    fn title_is_trimmed() {
        let items = outline_of("=    Spaced   \n");

        assert_eq!(items[0].title, "Spaced");
    }

    /// 标题在代码块里不该被当成正文标题…… 但 Typst 里 `= ` 在代码块里
    /// 本来就是代码，不是标题。这条确认我们没把它误当标题。
    #[test]
    fn a_hash_inside_a_code_block_is_not_a_heading() {
        let items = outline_of("```\n= not a heading\n```\n\n= Real\n");

        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Real"]);
    }

    /// 没标题时给空表，不 panic。
    #[test]
    fn a_document_without_headings_gives_an_empty_outline() {
        assert!(outline_of("Just a paragraph.\n").is_empty());
        assert!(outline_of("").is_empty());
    }

    /// 未闭合的语法不该让大纲 panic —— 我们在编辑器里边打边算。
    #[test]
    fn broken_syntax_does_not_panic() {
        for broken in [
            "= Heading (unclosed",
            "= \n== \n===",
            "#let x = (1 +",
            "= 中文标题\n\n=== 更深\n",
        ] {
            let _ = outline_of(broken);
        }
    }
}
