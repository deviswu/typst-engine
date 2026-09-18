//! 补全候选的**来源**（UI 无关）。
//!
//! 这里只回答「有哪些名字可以补」；怎么弹下拉、按什么规则过滤、插入时
//! 替换哪一段，都是外壳的事（`crates/app/src/completion.rs`）。
//!
//! # 名字分两类
//!
//! 1. **内置**：`Library::default()` 里就有官方那份 scope（全局 + 数学模式），
//!    再加上 `KEYWORDS` 里那几个语法词。**刻意不手抄一份内置函数表** ——
//!    那种表一定会随着 typst 升级而过期，而这份 scope 是编译器自己用的。
//! 2. **本文档定义的**：`#let x = …` / `#import "…": a, b` 里的名字。
//!    补全必须能补出「刚写的那个函数」，否则它就是摆设。
//!
//! 两类都靠引擎里那棵**增量维护的语法树**（`typst::World::source`），
//! 所以每次敲键只花一次树遍历，不重新解析。

use std::sync::OnceLock;

use typst::syntax::{LinkedNode, Source, SyntaxKind};
use typst::{Library, LibraryExt};

/// 语法关键字：它们不在 library 的 scope 里（不是函数），但敲 `#` 之后
/// 最想补的恰恰是这些。
pub const KEYWORDS: &[&str] = &[
    "let", "set", "show", "import", "include", "if", "else", "for", "while", "in", "not", "and",
    "or", "context", "return", "break", "continue", "none", "auto", "true", "false",
];

/// 常用的那几个：`#` 之后还没敲字母时，下拉里排最前面的应该是它们，
/// 而不是一堆按字母序挤在前面的名字（详见外壳里的排序注释）。
pub const COMMON_NAMES: &[&str] = &[
    "table",
    "figure",
    "image",
    "grid",
    "stack",
    "box",
    "block",
    "rect",
    "circle",
    "line",
    "path",
    "page",
    "text",
    "par",
    "heading",
    "list",
    "enum",
    "terms",
    "place",
    "align",
    "pad",
    "columns",
    "link",
    "footnote",
    "cite",
    "ref",
    "raw",
    "quote",
    "strong",
    "emph",
    "underline",
    "strike",
    "highlight",
    "sub",
    "super",
    "math",
    "set",
    "show",
    "let",
    "import",
    "include",
    "rect",
];

/// Typst 内置名字（全局 + 数学模式 + 关键字），已排序去重。
///
/// 建一遍 std 库有开销（几十毫秒量级），所以结果缓存起来 —— 进程里只做一次。
pub fn builtin_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();

    NAMES.get_or_init(|| {
        let library = Library::default();
        let mut names: Vec<String> = Vec::new();

        for scope in [library.global.scope(), library.math.scope()] {
            for (name, _) in scope.iter() {
                let name = name.as_str();
                // 下划线开头的是内部记号（`_`、`_std` 之类），不是给人敲的
                if name.starts_with('_') {
                    continue;
                }
                names.push(name.to_owned());
            }
        }

        names.extend(KEYWORDS.iter().map(|k| (*k).to_owned()));
        names.sort();
        names.dedup();
        names
    })
}

/// 本文档里定义出来的名字（`#let` / `#import`），已排序去重。
///
/// 消费方应当把引擎里那棵增量维护的 `Source` 传进来。
pub fn definitions(source: &Source) -> Vec<String> {
    let text = source.text();
    let mut out = Vec::new();
    visit(LinkedNode::new(source.root()), text, &mut out);
    out.sort();
    out.dedup();
    out
}

fn visit(node: LinkedNode<'_>, text: &str, out: &mut Vec<String>) {
    match node.kind() {
        // `#let 名字 = …` / `#let 名字(参数) = …`：树序里**第一个** Ident 就是绑定名
        // （`let` 是关键字符号不是 Ident，形参在名字之后）。所以取一个就够，
        // 不会把形参 `x` 一起收进来。
        SyntaxKind::LetBinding => {
            if let Some(name) = idents_in(&node, text).first() {
                out.push(name.clone());
            }
        }
        // `#import "x.typ": a, b` 绑的是 a、b；`#import "x.typ" as c` 绑的是 c。
        // 两种情况下的 Ident 都是「本文档可用的名字」（路径是字符串，不是 Ident）。
        SyntaxKind::ModuleImport => out.extend(idents_in(&node, text)),
        _ => {}
    }

    for child in node.children() {
        visit(child, text, out);
    }
}

/// 子树里所有 `Ident` 节点的文本（按树序）。
fn idents_in(node: &LinkedNode<'_>, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_idents(node, text, &mut out);
    out
}

fn collect_idents(node: &LinkedNode<'_>, text: &str, out: &mut Vec<String>) {
    if node.kind() == SyntaxKind::Ident
        && let Some(raw) = text.get(node.range())
    {
        out.push(raw.to_owned());
    }
    for child in node.children() {
        collect_idents(&child, text, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_cover_the_usual_suspects() {
        let names = builtin_names();

        for expected in [
            "set", "show", "table", "figure", "text", "page", "image", "grid",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "内置里该有 {expected}"
            );
        }
        // 关键字也在里面（它们不是库函数，但敲 `#` 之后最想补的就是它们）
        for expected in ["let", "import", "if", "for"] {
            assert!(
                names.contains(&expected.to_string()),
                "关键字里该有 {expected}"
            );
        }
    }

    #[test]
    fn builtins_are_sorted_and_unique() {
        let names = builtin_names();

        assert!(
            names.windows(2).all(|w| w[0] < w[1]),
            "该是严格递增的（排好序且去重）"
        );
        assert!(
            names.iter().all(|n| !n.starts_with('_')),
            "内部记号不该出现"
        );
        assert!(names.len() > 50, "std 的名字不该这么少：{}", names.len());
    }

    /// 缓存只建一次：两次调用拿到的必须是同一份。
    #[test]
    fn builtins_are_cached() {
        assert!(std::ptr::eq(builtin_names(), builtin_names()));
    }

    /// 「常用」那一档必须都是**真的存在**的名字 —— 手写的表最容易写错，
    /// 写错了不会报错，只会让下拉里多一个永远补不出来的候选。
    #[test]
    fn common_names_all_exist() {
        let builtins = builtin_names();

        for name in COMMON_NAMES {
            assert!(
                builtins.contains(&name.to_string()),
                "常用表里的 {name:?} 不是 Typst 内置名字（拼错了？）"
            );
        }
    }

    #[test]
    fn finds_let_and_import_names() {
        let source = Source::detached(
            "#let alpha = 1\n\
             #let beta(x) = x\n\
             #import \"u.typ\": gamma\n\
             #import \"v.typ\" as delta\n",
        );

        let names = definitions(&source);

        for expected in ["alpha", "beta", "gamma", "delta"] {
            assert!(
                names.contains(&expected.to_string()),
                "该收进 {expected}：{names:?}"
            );
        }
        assert!(
            !names.contains(&"x".to_string()),
            "形参不是文档定义：{names:?}"
        );
    }

    /// `#set` / `#show` 也是 LetBinding（kind 不同），但它们**不绑定新名字** ——
    /// 别把 `#set text(size: 11pt)` 里的 `text` 当成用户定义收进来。
    #[test]
    fn set_and_show_rules_do_not_define_names() {
        let source = Source::detached(
            "#set text(size: 11pt)\n#show heading: it => it.body\n#let mine = 1\n",
        );

        let names = definitions(&source);

        assert_eq!(names, vec!["mine".to_string()], "只该有真正 let 出来的名字");
    }

    #[test]
    fn broken_syntax_does_not_panic() {
        for broken in ["#let ", "#let x = ", "#import \"", "#let 中文名 = 1"] {
            let _ = definitions(&Source::detached(broken));
        }
    }
}
