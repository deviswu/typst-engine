//! 三块主区：左栏（目录树 / 大纲）、右栏（预览 / Markdown / 图片）、
//! 以及编译错误面板。

use crate::ui::util::*;
use crate::*;

impl Previewer {
    pub(crate) fn render_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .w_full()
            .h_full()
            .min_w_0()
            .bg(theme.sidebar)
            .child(self.render_tree_body(cx))
    }

    /// 目录树本体：点文件 → 按类型显示到右侧。
    pub(crate) fn render_tree_body(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        if self.tree_root.is_none() {
            // 旧文案是「点右上角的刷新」—— 那个按钮随左栏标题行一起被撤掉了（L21），
            // 于是这句提示指向一个不存在的东西；更要命的是 `refresh_tree` 全项目只剩
            // `set_root_dir` 一个调用点，**目录树再也没有任何入口能扫一次**。
            //
            // 热区挂在**容器**上，不挂在里面的按钮上 —— 这是实测出来的，不是选择：
            // 同样一个点击坐标，落在容器上会触发（日志有「扫描目录树」），落在那个
            // `Button::on_click` 上什么都不发生（而同屏工具栏、状态栏的按钮都正常，
            // 左栏的树条目也正常，所以不是整块面板收不到事件）。原因未查明；
            // 反正空面板里没有别的能点，大热区不会误伤，也不会再“点了没反应”。
            return v_flex()
                .id("tree-empty")
                .flex_1()
                .p_3()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|s| s.bg(theme.muted_foreground.opacity(0.08)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| this.refresh_tree(cx)),
                )
                .child("还没有扫描目录")
                .child(
                    Button::new("tree-scan")
                        .small()
                        .ghost()
                        .label(format!("扫描 {}", short_path(&self.root.to_string_lossy()))),
                )
                .child("或者从「文件 → 打开文件夹…」选一个工作区")
                .into_any_element();
        }

        if self.tree_scanning {
            return div()
                .flex_1()
                .p_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("正在扫描目录…")
                .into_any_element();
        }

        // 扫出来空空如也时**说清楚扫的是哪个目录** —— 否则用户只看到一个空面板，
        // 会以为「目录树坏了」（用内置演示文档时曾经就是这样：根目录取的是
        // 主文件所在的 `%TEMP%`，点开「目录」看着像空的）。
        if self.tree_entries == 0 {
            let root = self
                .tree_root
                .as_ref()
                .map(|path| short_path(&path.to_string_lossy()))
                .unwrap_or_default();
            return v_flex()
                .flex_1()
                .p_3()
                .gap_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("这个目录里没有可打开的文件")
                .child(div().text_color(theme.primary).child(root))
                .child("用「文件 → 打开…」换一个文件，树根会跟着它走")
                .into_any_element();
        }

        let view = cx.entity();
        // 右键菜单那个闭包也要一份（`view` 被下面的 render_item 搬走了）
        let menu_view = view.clone();
        Tree::new(
            &self.tree_state,
            move |ix, entry, _selected, _window, cx| {
                // `render_item` 拿到的是 `&mut App`，要 `Context<Self>` 才能挂点击回调；
                // 借 `view.update` 换一个上下文出来（gpui-component 自己的 story 也这么写）。
                view.update(cx, |_, cx| {
                    let icon: AnyElement = if entry.is_folder() {
                        let name = if entry.is_expanded() {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        };
                        Icon::from(name)
                            .text_color(cx.theme().primary)
                            .into_any_element()
                    } else {
                        tree::file_type_icon(Path::new(entry.item().id.as_ref()))
                    };

                    let path = PathBuf::from(entry.item().id.as_ref());
                    ListItem::new(ix)
                        .w_full()
                        .rounded(cx.theme().radius)
                        .px_2()
                        .pl(px(12.) * entry.depth() + px(6.))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(icon)
                                .child(entry.item().label.clone()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // 目录的展开/收起由 Tree 自己做了（它在外层包了一个
                            // mouse_down 调 toggle），这里只管打开文件
                            if path.is_dir() {
                                return;
                            }
                            this.open_path(path.clone(), window, cx);
                        }))
                })
            },
        )
        .flex_1()
        // 右键一个文件 →「AI 处理此文件…」。
        //
        // 这是「对选中的文件做处理」的主入口：右键**不会**打开文件，所以可以让
        // AI 去改一个**当前没打开**的文件（一边改着正文、一边过一遍附录），
        // 不会被「有未保存改动就不让换文件」拦住（见 `open_in_editor`）。
        .context_menu(move |_ix, entry, menu, _window, cx| {
            if entry.is_folder() {
                return menu;
            }
            let path = PathBuf::from(entry.item().id.as_ref());
            let view = menu_view.clone();
            view.update(cx, |_, cx| {
                menu.item(
                    PopupMenuItem::new(format!("AI 处理「{}」…", short_label(&path))).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.open_ai_for_file(path.clone(), window, cx);
                        }),
                    ),
                )
            })
        })
        .into_any_element()
    }
    pub(crate) fn render_errors(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_1()
            .bg(theme.danger.opacity(0.08))
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.danger)
            // 每一条都能点：点了跳到出错那一行（以前这里是一列死文本 ——
            // 「看到错了却得自己去找」是用户提的第 2 条）。
            .children(self.diags.iter().take(MAX_ERROR_ROWS).map(|d| {
                let line = d.line_col.as_ref().map(|pos| pos.line);
                let where_ = d
                    .line_col
                    .as_ref()
                    .map(|pos| format!("第 {} 行", pos.line + 1))
                    .unwrap_or_else(|| d.severity_label().to_string());

                let row = h_flex()
                    .w_full()
                    .items_start()
                    .gap_2()
                    .px_1()
                    .rounded_sm()
                    .child(div().flex_shrink_0().child(where_))
                    .child(div().flex_1().min_w_0().child(d.message.clone()));

                match line {
                    Some(line) => row
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.danger.opacity(0.14)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                this.jump_to_line(line, window, cx);
                            }),
                        )
                        .into_any_element(),
                    // 映射不出行的（比如「文档没排成功」）就不给点击
                    None => row.into_any_element(),
                }
            }))
    }
    /// 右侧主区：**按内容自动选视图**，没有切换按钮。
    ///
    /// 与 `wu` 一致：打开 `.typ` 就看排版预览、打开 `.md` 就看 Markdown、
    /// 打开图片就看图片 —— 视图由「现在在看什么文件」决定，不由用户点页签决定。
    /// 所以这里只按 `self.right`（由 `open_path` / `open_in_editor` 设置）画内容。
    ///
    /// **没有标题行**（用户要求：顶部那一行只留给标签页）：以前这里要写
    /// 「排版预览 / Markdown · xxx」再配一行只读页码，现在全撤 ——
    /// 在看什么文件看标签页，页码与缩放看状态栏。
    pub(crate) fn render_right_pane(
        &self,
        preview: AnyElement,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();

        let body: AnyElement = match self.right {
            RightPane::Preview => preview,
            RightPane::Markdown => {
                if self.markdown.read(cx).is_empty() {
                    placeholder("在左边目录树里点一个 .md 文件", theme)
                } else {
                    div()
                        .size_full()
                        .child(self.markdown.clone())
                        .into_any_element()
                }
            }
            RightPane::Image => match &self.image {
                Some(view) => div().size_full().child(view.clone()).into_any_element(),
                None => placeholder(
                    "在左边目录树里点一张图片（png/jpg/webp/gif/bmp/svg）",
                    theme,
                ),
            },
        };

        div().flex_1().h_full().min_w_0().child(body)
    }
}
