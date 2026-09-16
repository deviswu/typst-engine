//! Markdown 渲染：直接用 gpui-component 内置的 markdown 组件。
//!
//! 移植自参考项目 `wu` 的 `src/markdown_view.rs`。
//!
//! 内置组件基于 `markdown` crate（Zed 同款引擎，完整 GFM），白拿三样东西：
//!
//! - 表格 / 任务列表 / 删除线 / 脚注等完整 GFM
//! - 代码块 tree-sitter 语法高亮
//! - `.selectable(true)` 原生选中 + 复制 —— **显示区终于能选文字了**
//!   （排版预览是位图纹理，选不了；markdown 这条走的是真正的文本层）
//!
//! 所以本文件只有「持有源码」这一件事，渲染全交给内置组件。

use gpui::{Context, IntoElement, Render, Styled as _, Window};
use gpui_component::text::markdown;

/// Markdown 视图：持有源码，`render` 时交给内置组件。
pub struct MarkdownView {
    source: String,
    /// 当前显示的文件路径（标题上显示用）。
    path: Option<String>,
}

impl MarkdownView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            source: String::new(),
            path: None,
        }
    }

    /// 换一份 Markdown 源码。
    pub fn set_source(&mut self, path: String, source: String, cx: &mut Context<Self>) {
        self.path = Some(path);
        self.source = source;
        cx.notify();
    }

    pub fn is_empty(&self) -> bool {
        self.source.trim().is_empty()
    }
}

impl Render for MarkdownView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        markdown(self.source.clone())
            .scrollable(true)
            .selectable(true)
            .p_4()
            .size_full()
    }
}
