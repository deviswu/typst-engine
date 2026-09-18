//! 主题：把注册表里的一个主题名应用到界面上。
//!
//! # gpui-component 的主题模型（看过源码之后才敢写的三条）
//!
//! 1. `ThemeRegistry` 里存着一堆 `ThemeConfig`，每个自带 `mode`（亮/暗）
//! 2. `Theme::apply_config(&config)` **会把这份配置放进对应那一侧**
//!    （`light_theme` 或 `dark_theme`），并上色
//! 3. `Theme::theme_name()` 报的就是当前那一侧的名字
//!
//! 所以「切主题」只需要两步：把 `mode` 对齐、把配置 `apply_config` 进去。
//! **不需要**单独存「亮还是暗」—— 名字本身带着模式，存两份迟早不一致。
//!
//! 想自定义主题：往 gpui-component 的 `themes/` 目录放 JSON 就行
//! （`ThemeRegistry::watch_dir` 是它们的机制，本应用不额外管）。

// 只引要用的几个类型，**不要** `use gpui::*`：那会把 gpui 自己的
// `#[gpui::test]` 宏（名字就叫 `test`）带进来，于是本文件里的 `#[test]`
// 会解析到它头上，报「recursion limit reached while expanding #[test]」。
use std::sync::Arc;

use gpui::{App, Hsla, SharedString, Window, hsla};
use gpui_component::highlighter::{HighlightTheme, ThemeStyle};
use gpui_component::{Theme, ThemeRegistry};

/// 注册表里所有主题的名字（默认主题优先、亮色在前、其余按名字序）。
pub fn names(cx: &App) -> Vec<SharedString> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .map(|config| config.name.clone())
        .collect()
}

/// 把某个主题应用到界面上。名字不在注册表里就什么都不做，返回 `None`。
///
/// 返回该主题是不是深色 —— 只给日志用（设置里不存它，见模块头）。
pub fn apply(name: &str, window: &mut Window, cx: &mut App) -> Option<bool> {
    let config = ThemeRegistry::global(cx).themes().get(name).cloned()?;
    let dark = config.mode.is_dark();

    {
        let theme = Theme::global_mut(cx);
        theme.mode = config.mode;
        theme.apply_config(&config);
    }

    // 括号的颜色自己定，不跟着主题的某一路颜色走：主题里哪一路是「青」并不统一，
    // 而这一个颜色要保证在两种底色上都显眼 —— 暗色用亮青、亮色用深青。
    let brackets = if dark {
        hsla(0.52, 0.85, 0.72, 1.0)
    } else {
        hsla(0.55, 0.78, 0.42, 1.0)
    };
    tint_brackets(brackets, cx);

    window.refresh();
    Some(dark)
}

/// 把代码里的括号染成指定颜色。
///
/// 为什么需要这一步：主题文件里 `punctuation.bracket` 基本都是**没写**的，
/// 于是括号跟正文一个颜色 —— 一屏代码里 `( ) [ ] { }` 全沉在字里行间，
/// 配对读起来很费眼。这里在应用主题之后补一条自己的样式。
///
/// `ThemeStyle` 的字段是**私有的、也没有构造器**，唯一的构造途径是反序列化
/// （`Hsla` 的 serde 表示就是 `Rgba { r, g, b, a }`，所以给个颜色就行）。
/// 反序列化失败就当没这回事 —— 括号少一层颜色，不该把界面搞崩。
fn tint_brackets(color: Hsla, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let mut highlight: HighlightTheme = (*theme.highlight_theme).clone();

    let Ok(style) = serde_json::from_value::<ThemeStyle>(serde_json::json!({ "color": color }))
    else {
        return;
    };

    highlight.style.syntax.punctuation_bracket = Some(style);
    theme.highlight_theme = Arc::new(highlight);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ThemeStyle` 只能从 JSON 造 —— 这条测试盯着那条路别断。
    #[test]
    fn a_bracket_style_can_be_built_from_a_color() {
        let style = serde_json::from_value::<ThemeStyle>(
            serde_json::json!({ "color": gpui::hsla(0.5, 0.6, 0.6, 1.0) }),
        );

        assert!(style.is_ok(), "括号的样式没造出来：{style:?}");
    }
}
