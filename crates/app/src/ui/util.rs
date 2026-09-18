//! 渲染用的小工具：路径缩写、占位块、错误波浪线的端点换算。

use crate::*;

/// 波浪线的终点。
///
/// 至少覆盖一个字符；若诊断带着可用的字节范围、且终点**在同一行**，
/// 就盖住整个病灶而不是戳一个孤零零的点（跨行的话用一个字符收尾，
/// 免得算出一段横跨多行的矩形）。
pub(crate) fn squiggle_end(
    source: &typst::syntax::Source,
    d: &lang::Diagnostic,
    start: lang::LineCol,
) -> lang::LineCol {
    let one_char = lang::LineCol {
        line: start.line,
        col: start.col + 1,
    };

    let Some(range) = d.range.as_ref() else {
        return one_char;
    };
    let end = lang::line_col(source, range.end);

    if end.line == start.line && end.col > start.col {
        end
    } else {
        one_char
    }
}
/// 路径太长时只留末两段（右栏标题用）。
pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0 => path.to_string(),
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}
/// 面板/日志里显示的文件名（只要文件名，不要整条路径）。
pub(crate) fn short_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| short_path(&path.to_string_lossy()))
}
/// 右栏的「还没内容」占位（纯文本，无按钮）。
pub(crate) fn placeholder(text: &'static str, theme: &gpui_component::Theme) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(text)
        .into_any_element()
}
/// 颜色名 → 菜单上的中文（与 `wu` 的叫法一致）。
pub(crate) fn color_label(name: &str) -> &'static str {
    match name {
        "red" => "红",
        "orange" => "橙",
        "yellow" => "黄",
        "green" => "绿",
        "aqua" => "青",
        "blue" => "蓝",
        "purple" => "紫",
        "gray" => "灰",
        "black" => "黑",
        _ => "颜色",
    }
}
/// 主题那行日志的正文。
///
/// 带上背景色的 **hsl 数字**：光看「切成了深色」是自我报告，
/// 数字才能证明真的换了（浅色主题的 l 接近 1，深色接近 0）。
pub(crate) fn describe_theme(dark: bool, cx: &App) -> String {
    let bg = cx.theme().background;
    format!(
        "{}（背景 hsl {:.3} {:.3} {:.3}）",
        if dark { "深色" } else { "浅色" },
        bg.h,
        bg.s,
        bg.l
    )
}
