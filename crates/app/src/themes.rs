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
use gpui::{App, SharedString, Window};
use gpui_component::select::SelectItem;
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

    let theme = Theme::global_mut(cx);
    theme.mode = config.mode;
    theme.apply_config(&config);

    window.refresh();
    Some(dark)
}

/// 下拉列表里的一项。
#[derive(Clone, PartialEq)]
pub struct ThemeItem {
    name: SharedString,
    active: bool,
}

impl ThemeItem {
    /// 全部主题；`active` 是当前那一个（列表里给它打个勾）。
    pub fn all(active: &str, cx: &App) -> Vec<Self> {
        names(cx)
            .into_iter()
            .map(|name| Self {
                active: name.as_ref() == active,
                name,
            })
            .collect()
    }

    /// 当前那一个在列表里的下标（给下拉框定位用）。
    pub fn index_of(items: &[Self]) -> Option<usize> {
        items.iter().position(|item| item.active)
    }
}

impl SelectItem for ThemeItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_of_finds_the_active_one() {
        let items = vec![
            ThemeItem {
                name: "A".into(),
                active: false,
            },
            ThemeItem {
                name: "B".into(),
                active: true,
            },
        ];

        assert_eq!(ThemeItem::index_of(&items), Some(1));
    }

    #[test]
    fn index_of_gives_none_when_nothing_is_active() {
        let items = vec![ThemeItem {
            name: "A".into(),
            active: false,
        }];

        assert_eq!(ThemeItem::index_of(&items), None);
    }
}
