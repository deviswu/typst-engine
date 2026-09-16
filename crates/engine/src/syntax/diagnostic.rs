//! 诊断：把语法错误与编译错误统一成同一种形状。
//!
//! 关键事实：`SyntaxDiagnostic.span` 与 `SourceDiagnostic.span`
//! **是同一个类型 `DiagSpan`**，所以两者的字节范围映射能共用一份代码。
//! 这对消费方意味着：画波浪线的地方只需要写一遍。

use std::ops::Range;

use typst::diag::SourceDiagnostic;
use typst::syntax::{DiagSpan, DiagSpanKind, Source};

/// 0 起的行号与列号。
///
/// 列按**字符**数（不是字节）—— 与 LSP 的 `Position.character` 一致，
/// 中文不会因为一个字符占 3 字节而把光标算偏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

/// 一条诊断。语法错误与编译错误都会变成这个形状。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub is_error: bool,
    pub message: String,
    /// 字节范围。映射不出来时是 `None`。
    pub range: Option<Range<usize>>,
    /// 0 起的行列。映射不出来时是 `None`。
    pub line_col: Option<LineCol>,
}

impl Diagnostic {
    /// 严重程度的中文名，省得每个消费方各写一遍。
    pub fn severity_label(&self) -> &'static str {
        if self.is_error { "错误" } else { "警告" }
    }
}

/// **不编译**就能拿到的语法错误与警告。
///
/// 错误排在警告前面。两者都来自解析器，所以它比编译快得多 ——
/// 适合每次敲键都跑一遍，好让用户立刻看到「括号没闭」，
/// 而不用等排版。
pub fn syntax_diagnostics(source: &Source) -> Vec<Diagnostic> {
    let (errors, warnings) = source.root().errors_and_warnings();

    errors
        .into_iter()
        .chain(warnings)
        .map(|d| convert(source, d.is_error, d.message.as_str(), &d.span))
        .collect()
}

/// 把编译产出的诊断映射成同样的形状。
///
/// 有了它，画波浪线的代码只需要认 [`Diagnostic`] 一种输入，
/// 不必区分「来自解析器」还是「来自编译器」。
pub fn compile_diagnostics(source: &Source, diags: &[SourceDiagnostic]) -> Vec<Diagnostic> {
    diags
        .iter()
        .map(|d| {
            convert(
                source,
                d.severity == typst::diag::Severity::Error,
                d.message.as_str(),
                &d.span,
            )
        })
        .collect()
}

fn convert(source: &Source, is_error: bool, message: &str, span: &DiagSpan) -> Diagnostic {
    let range = range_of_diag_span(source, span);
    Diagnostic {
        is_error,
        message: message.to_owned(),
        line_col: range.clone().map(|r| line_col(source, r.start)),
        range,
    }
}

/// `DiagSpan` → 源码字节范围。
///
/// 只在指向**本文件**时才有结果；指向别的文件或根本 detached 时给 `None`。
pub fn range_of_diag_span(source: &Source, span: &DiagSpan) -> Option<Range<usize>> {
    match span.get() {
        // 外部文件直接带着字节范围
        DiagSpanKind::Range { id, range } if id == source.id() => Some(range),
        // 内部 Span 编号，需要问 Source 要（它内部是二分/树查找）
        DiagSpanKind::Number { id, num, sub_range } if id == source.id() => {
            source.range(num, sub_range)
        }
        _ => None,
    }
}

/// 字节偏移 → 行列。列按字符计。
pub fn line_col(source: &Source, byte: usize) -> LineCol {
    let text = source.text();
    let end = byte.min(text.len());

    let line = text.as_bytes()[..end]
        .iter()
        .filter(|b| **b == b'\n')
        .count();
    let line_start = text[..end].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = text[line_start..end].chars().count();

    LineCol { line, col }
}

#[cfg(test)]
mod tests {
    use typst::syntax::Source;

    use super::*;

    fn source_with(text: &str) -> Source {
        Source::detached(text)
    }

    #[test]
    fn line_col_is_zero_based() {
        let s = source_with("abc\ndef\n");

        assert_eq!(line_col(&s, 0), LineCol { line: 0, col: 0 });
        assert_eq!(line_col(&s, 2), LineCol { line: 0, col: 2 });
        assert_eq!(line_col(&s, 4), LineCol { line: 1, col: 0 });
        assert_eq!(line_col(&s, 6), LineCol { line: 1, col: 2 });
    }

    /// 列按字符算，不按字节 —— 中文一个字符 3 字节。
    #[test]
    fn columns_count_characters_not_bytes() {
        let s = source_with("中文\nx");

        assert_eq!(line_col(&s, 0), LineCol { line: 0, col: 0 });
        assert_eq!(line_col(&s, 3), LineCol { line: 0, col: 1 }, "第 2 个汉字");
        assert_eq!(line_col(&s, 6), LineCol { line: 0, col: 2 });
        assert_eq!(line_col(&s, 7), LineCol { line: 1, col: 0 });
    }

    #[test]
    fn a_byte_past_the_end_is_clamped_not_a_panic() {
        let s = source_with("abc");

        assert_eq!(line_col(&s, 999), LineCol { line: 0, col: 3 });
    }

    /// 未闭合的括号必须被解析器报出来 —— 这是「不等编译就能提示」的基础。
    #[test]
    fn an_unclosed_paren_is_reported_by_the_parser() {
        let s = source_with("#let x = (1 + 2\n");
        let diags = syntax_diagnostics(&s);

        assert!(!diags.is_empty(), "解析器应该报出未闭合");
        assert!(diags.iter().any(|d| d.is_error), "应该有错误而不是只有警告");
    }

    #[test]
    fn a_clean_document_has_no_error_diagnostics() {
        let s = source_with("#set page(width: 5cm)\n\n= Title\n\nHello.\n");
        let diags = syntax_diagnostics(&s);

        assert!(
            diags.iter().all(|d| !d.is_error),
            "干净的文档不该有语法错误：{diags:?}"
        );
    }

    /// 有病灶时行列必须落在正文里，而不是 `None`。
    #[test]
    fn a_reported_error_carries_a_usable_position() {
        let s = source_with("#let x = (1 + 2\n");
        let diags = syntax_diagnostics(&s);
        let with_pos = diags.iter().find(|d| d.line_col.is_some());

        let Some(d) = with_pos else {
            panic!("一条诊断都没定位上：{diags:?}");
        };
        let pos = d.line_col.unwrap();
        assert_eq!(pos.line, 0, "错误在第 0 行");
        assert!(pos.col <= 16, "列号不该跑出这一行：{pos:?}");
        assert!(d.range.is_some(), "字节范围也该给出来");
    }

    #[test]
    fn severity_label_is_readable() {
        let err = Diagnostic {
            is_error: true,
            message: String::new(),
            range: None,
            line_col: None,
        };
        assert_eq!(err.severity_label(), "错误");
    }

    /// `compile_diagnostics` 与 `syntax_diagnostics` 必须产出同一种形状 ——
    /// 这正是消费方只需要认一种输入的原因。
    #[test]
    fn both_sources_of_diagnostics_share_one_shape() {
        let s = source_with("#let x = (1 + 2\n");

        let from_parser = syntax_diagnostics(&s);
        // 编译诊断这里传空切片，只验证签名与形状能对上
        let from_compiler = compile_diagnostics(&s, &[]);

        assert!(!from_parser.is_empty());
        assert!(from_compiler.is_empty());
        // 两者都是 Vec<Diagnostic>，可以拼在一起交给同一个渲染器
        let all: Vec<Diagnostic> = from_parser.into_iter().chain(from_compiler).collect();
        assert!(!all.is_empty());
    }
}
