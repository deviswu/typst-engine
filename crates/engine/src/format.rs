//! Typst 代码格式化 —— **进程内**。
//!
//! 对比 `wu`：它调用外部 `typstyle` 可执行文件（走临时文件 + 子进程），
//! 所以用户得先 `cargo install typstyle` 或下载 release，否则功能不可用。
//! 这里直接链 `typstyle-core`：无子进程、无临时文件、无安装前置，
//! 也不会因为 PATH 里没有 typstyle 就静默失败。
//!
//! 功能对齐，底层实现不同 —— 这是我们和 `wu` 的关系。

use typstyle_core::{Config, Typstyle};

/// 用默认配置格式化。
///
/// 返回格式化后的文本；语法烂到无法解析时返回错误信息（**不** panic，
/// 因为用户很可能就是在半写完的状态下按的快捷键）。
pub fn format(text: &str) -> Result<String, String> {
    Typstyle::new(Config::default())
        .format_text(text.to_owned())
        .render()
        .map_err(|err| format!("{err:?}"))
}

/// 用指定配置格式化。
pub fn format_with(text: &str, config: Config) -> Result<String, String> {
    Typstyle::new(config)
        .format_text(text.to_owned())
        .render()
        .map_err(|err| format!("{err:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_spacing_around_equals() {
        // 注意：`=Title`（无空格）在 Typst 里**不是标题**，只是一段 markup 文本，
        // 所以 typstyle 不会给它补空格（实测确认）。
        // 这是“格式化”与“自动修错”的边界：前者不改语义。
        let messy = "= Title\n\n#let x=1\n#let y  =  2\n";
        let tidy = format(messy).expect("该能格式化");

        assert!(tidy.contains("#let x = 1"), "等号两侧该补空格：{tidy:?}");
        assert!(tidy.contains("#let y = 2"), "多余空格该收掉：{tidy:?}");
    }

    /// 实测记录下来的一条行为：`=Title` 不被当作标题，所以不会被改。
    /// 写下来是为了以后有人觉得「这里应该补个空格」时能看到它是有意为之。
    #[test]
    fn a_hash_without_a_space_is_not_a_heading_so_it_is_left_alone() {
        let out = format("=Title\n").expect("该能格式化");

        assert_eq!(out, "=Title\n");
    }

    /// 格式化必须是**幂等**的 —— 第二次不该再改动。
    /// 否则用户连按两次快捷键会看到文本一直变。
    #[test]
    fn formatting_is_idempotent() {
        let messy = "=Title\n\n#let x=1\n";
        let once = format(messy).expect("第一次");
        let twice = format(&once).expect("第二次");

        assert_eq!(once, twice, "第二次格式化还改了东西");
    }

    /// 已经干净的文本不该被改动（实测：`#let x=1` 这种才会被改）。
    #[test]
    fn already_clean_text_is_returned_unchanged() {
        let clean = "= Title\n\n#let x = 1\n\nBody.\n";
        let out = format(clean).expect("该能格式化");

        assert_eq!(out, clean);
    }

    /// 中文内容不该被破坏。
    #[test]
    fn cjk_content_survives() {
        let text = "= 中文标题\n\n这是一段中文正文，不该被改动。\n";
        let out = format(text).expect("该能格式化");

        assert!(out.contains("这是一段中文正文，不该被改动。"));
    }

    /// 半写完的语法不该 panic —— 用户很可能正在打字中间按了快捷键。
    #[test]
    fn broken_syntax_does_not_panic() {
        for broken in [
            "#let x = (1 +",
            "= 标题\n\n#table(",
            "#for i in {",
            "= 未闭合的 [",
        ] {
            // 要么成功、要么给错误字符串；都不许崩
            let _ = format(broken);
        }
    }

    #[test]
    fn indentation_can_be_configured() {
        let text = "#let x = 1\n#for i in (1, 2) [\n#i\n]\n";

        let two = format_with(text, Config::default());
        assert!(two.is_ok());

        let four = format_with(
            text,
            Config {
                tab_spaces: 4,
                ..Config::default()
            },
        );
        assert!(four.is_ok());
    }

    /// 实测：空文本进去出来是一个换行（typstyle 保证以换行结尾）。
    /// 断言用 `trim` 而不是直接比 `""`，免得把无关的行为差异当 bug。
    #[test]
    fn empty_text_is_fine() {
        let out = format("").expect("空文本该能格式化");

        assert!(out.trim().is_empty(), "空文本给出了 {out:?}");
    }
}
