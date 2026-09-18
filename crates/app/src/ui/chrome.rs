//! 窗框上的东西：菜单条、工具栏所在的那一层之外的固定条目 ——
//! 状态栏、标签页、终端面板。

use crate::*;

impl Previewer {
    /// 顶部菜单条（文件 / 视图）。
    ///
    /// 用 gpui-component 的 `dropdown_menu` —— 与 `wu` 同一个路子。
    pub(crate) fn render_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let view = cx.entity();

        // 菜单勾选要反映当前状态，但**不能**在菜单闭包里 `view.read(cx)` ——
        // 那读的是正在渲染的这个视图，等于双重租借（`open_dialog` 那次崩溃
        // 就是这个姿势）。在闭包外取值、move 进去：菜单条每次 render 都会重建
        // 闭包，所以这些值不会过期。
        // 主题列表也在这里抓好（菜单闭包里没有 `cx` 可以查注册表）
        let theme_names: Vec<SharedString> = themes::names(cx);
        let current_theme: SharedString = self.theme_name.clone().unwrap_or_default().into();
        let recent_dirs: Vec<PathBuf> = self.recent_dirs.clone();

        let file_menu =
            {
                let view = view.clone();
                Button::new("menu-file")
                    .flex_shrink_0()
                    .ghost()
                    .compact()
                    .label("文件")
                    .dropdown_menu(move |menu, window, _cx| {
                        let view = view.clone();

                        // 最近文件夹：`PopupMenuItem` 是菜单项不是元素，既不能
                        // `.children(...)` 也不能 `.into_any_element()`，只能 for 循环塞。
                        // 建在这里是因为 `window` 只在闭包里有。
                        let mut recent_items: Vec<PopupMenuItem> =
                            vec![PopupMenuItem::new("（还没有）").disabled(true)];
                        if !recent_dirs.is_empty() {
                            recent_items = recent_dirs
                                .iter()
                                .enumerate()
                                .map(|(index, dir)| {
                                    let label = short_path(&dir.to_string_lossy());
                                    PopupMenuItem::new(label).on_click(window.listener_for(
                                        &view,
                                        move |this, _, _window, cx| {
                                            this.use_recent_dir(index, cx);
                                        },
                                    ))
                                })
                                .collect();
                        }

                        let mut menu =
                            menu.min_w(220.)
                                .item(PopupMenuItem::new("保存（Ctrl+S）").on_click(
                                    window.listener_for(&view, |this, _, _window, cx| {
                                        this.save_file(cx);
                                    }),
                                ))
                                .item(PopupMenuItem::new("重新编译（Ctrl+B）").on_click(
                                    window.listener_for(&view, |this, _, _window, cx| {
                                        this.recompile_now(cx);
                                    }),
                                ))
                                .item(PopupMenuItem::new("导出 PDF（Ctrl+E）").on_click(
                                    window.listener_for(&view, |this, _, _window, cx| {
                                        this.export_pdf(cx);
                                    }),
                                ))
                                .separator()
                                .item(PopupMenuItem::new("打开文件夹…").on_click(
                                    window.listener_for(&view, |this, _, _window, cx| {
                                        this.open_folder_picker(cx);
                                    }),
                                ))
                                .item(PopupMenuItem::new("最近文件夹").disabled(true));
                        for item in recent_items {
                            menu = menu.item(item);
                        }
                        menu = menu
                            .separator()
                            .item(PopupMenuItem::new("AI 编辑…（Ctrl+K）").on_click(
                                window.listener_for(&view, |this, _, window, cx| {
                                    this.open_ai(window, cx);
                                }),
                            ))
                            .separator()
                            .item(PopupMenuItem::new("退出").on_click(window.listener_for(
                                &view,
                                |this, _, _window, cx| {
                                    // 录着的时候点退出：先把录像收干净（同步收尾），
                                    // 不能把用户刚录的丢在分段文件里。
                                    this.finish_recording_on_close();
                                    cx.quit();
                                },
                            )));
                        menu
                    })
            };

        // 「主题」菜单：**只放主题列表**。
        //
        // 这里原来还塞着大纲/目录树/终端/缩放/代码折叠/跟随光标/预览背景/性能指标 ——
        // 全撤了（用户要求）：终端有状态栏按钮与 `Ctrl+4`，缩放 `Ctrl+=/-/0`、
        // 折叠 `Ctrl+Alt+F`、跟随 `Ctrl+Alt+L`、预览背景 `Ctrl+Alt+G`、
        // 性能指标 `Ctrl+Alt+M`，菜单里只留最常换的主题。
        let theme_menu = {
            let view = view.clone();
            Button::new("menu-theme")
                .flex_shrink_0()
                .ghost()
                .compact()
                .label("主题")
                .dropdown_menu(move |menu, window, _cx| {
                    let view = view.clone();
                    let mut menu = menu.min_w(180.);
                    for name in theme_names.iter().cloned() {
                        let active = name == current_theme;
                        let picked = name.clone();
                        menu = menu.item(
                            PopupMenuItem::new(name.to_string())
                                .checked(active)
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| {
                                        this.use_theme(picked.to_string(), window, cx);
                                    },
                                )),
                        );
                    }
                    menu
                })
        };

        let ai_menu = {
            let view = view.clone();
            Button::new("menu-ai")
                .flex_shrink_0()
                .ghost()
                .compact()
                .label("AI")
                .dropdown_menu(move |menu, window, cx| {
                    let view = view.clone();
                    let mut menu = menu.min_w(260.);

                    // 现在真正在用的是哪一组（端点 · 模型 · 有没有 Key）。
                    // 「Ctrl+K 出来的 AI 不能用」最该先看的就是这一行 ——
                    // 别人无法从「点了没反应」里推出「原来是没配 Key」。
                    let summary = view.read(cx).ai_cfg.summary();
                    menu = menu
                        .item(PopupMenuItem::new(summary).disabled(true))
                        .separator()
                        .item(PopupMenuItem::new("AI 编辑…（Ctrl+K）").on_click(
                            window.listener_for(&view, |this, _, window, cx| {
                                this.open_ai(window, cx)
                            }),
                        ));

                    // 目录树里选中的文件（点一下就选中了）。与当前文档同一个就不列了 ——
                    // 那一条就是上面的「AI 编辑…」，多一行只会让人犹豫点哪个。
                    if let Some(path) = view.read(cx).ai_menu_file(cx) {
                        menu = menu.item(
                            PopupMenuItem::new(format!("AI 处理「{}」…", short_label(&path)))
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| {
                                        this.open_ai_for_file(path.clone(), window, cx);
                                    },
                                )),
                        );
                    }

                    menu = menu.separator();
                    for (label, task) in [
                        ("语法检查并修复", AiTask::SyntaxFix),
                        ("中文校对", AiTask::Proofread),
                        ("术语一致性", AiTask::Terminology),
                        ("中译英", AiTask::TranslateToEnglish),
                        ("英译中", AiTask::TranslateToChinese),
                    ] {
                        menu = menu.item(PopupMenuItem::new(label).on_click(window.listener_for(
                            &view,
                            move |this, _, window, cx| {
                                // 固定任务的作用范围与 Ctrl+K 同一条规矩：
                                // 有选中就只改那段，没选就是整篇
                                let scope = this.current_ai_scope(cx);
                                this.run_ai_task(task, scope, window, cx);
                            },
                        )));
                    }
                    menu.separator()
                        .item(PopupMenuItem::new("AI 设置…").on_click(
                            window.listener_for(&view, |this, _, window, cx| {
                                this.open_ai_settings(window, cx)
                            }),
                        ))
                })
        };

        // 「视图」菜单：面板/栏的显隐 + 录屏。
        //
        // 它是为录屏回来的（L20 那次按用户要求删掉了）：录出来的画面**就是窗口本身**，
        // 所以「要录干净画面」只能靠把不想入镜的那些关掉。菜单条本身留着不关 ——
        // 全关掉之后总得有地方能开回来。
        let view_menu = {
            let view = view.clone();
            let (tree, editor, preview, toolbar, statusbar) = (
                self.show_tree,
                self.show_editor,
                self.show_preview,
                self.show_toolbar,
                self.show_statusbar,
            );
            let (mic, cam) = (self.rec_mic, self.rec_cam);
            let recording = self.rec.is_some();
            let paused = matches!(
                self.rec.as_ref().map(|r| r.state()),
                Some(record::State::Paused)
            );
            let finalizing = self.rec_finalizing;

            Button::new("menu-view")
                .flex_shrink_0()
                .ghost()
                .compact()
                .label("视图")
                .dropdown_menu(move |menu, window, _cx| {
                    let view = view.clone();
                    let mut menu = menu.min_w(260.);

                    // 五个显隐。`.checked()` 抓住的是「打开菜单那一刻的值」——
                    // 菜单条每次 render 都会重建这批闭包，所以不会过期。
                    menu = menu
                        .item(
                            PopupMenuItem::new("显示目录（左栏）")
                                .checked(tree)
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, _window, cx| {
                                        this.set_pane_visible(PaneToggle::Tree, !tree, cx);
                                    },
                                )),
                        )
                        .item(PopupMenuItem::new("显示编辑区").checked(editor).on_click(
                            window.listener_for(&view, move |this, _, _window, cx| {
                                this.set_pane_visible(PaneToggle::Editor, !editor, cx);
                            }),
                        ))
                        .item(PopupMenuItem::new("显示展示区").checked(preview).on_click(
                            window.listener_for(&view, move |this, _, _window, cx| {
                                this.set_pane_visible(PaneToggle::Preview, !preview, cx);
                            }),
                        ))
                        .item(PopupMenuItem::new("显示工具栏").checked(toolbar).on_click(
                            window.listener_for(&view, move |this, _, _window, cx| {
                                this.set_pane_visible(PaneToggle::Toolbar, !toolbar, cx);
                            }),
                        ))
                        .item(
                            PopupMenuItem::new("显示状态栏")
                                .checked(statusbar)
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, _window, cx| {
                                        this.set_pane_visible(
                                            PaneToggle::Statusbar,
                                            !statusbar,
                                            cx,
                                        );
                                    },
                                )),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(if recording {
                                "停止录屏（Ctrl+Alt+R）"
                            } else {
                                "开始录屏（Ctrl+Alt+R）"
                            })
                            .on_click(window.listener_for(
                                &view,
                                |this, _, window, cx| {
                                    this.toggle_recording(window, cx);
                                },
                            )),
                        );

                    if recording {
                        menu = menu.item(
                            PopupMenuItem::new(if paused {
                                "继续录（Ctrl+Alt+P）"
                            } else {
                                "暂停录（Ctrl+Alt+P）"
                            })
                            .on_click(window.listener_for(
                                &view,
                                |this, _, _window, cx| {
                                    this.toggle_record_pause(cx);
                                },
                            )),
                        );
                    }
                    if finalizing {
                        menu =
                            menu.item(PopupMenuItem::new("收尾中…（拼接 / 合成）").disabled(true));
                    }

                    menu.item(PopupMenuItem::new("录麦克风").checked(mic).on_click(
                        window.listener_for(&view, move |this, _, _window, cx| {
                            this.set_record_mic(!mic, cx)
                        }),
                    ))
                    .item(
                        PopupMenuItem::new("录摄像头画中画").checked(cam).on_click(
                            window.listener_for(&view, move |this, _, _window, cx| {
                                this.set_record_cam(!cam, cx)
                            }),
                        ),
                    )
                })
        };

        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_2()
            .py_0p5()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(file_menu)
            .child(theme_menu)
            .child(ai_menu)
            .child(view_menu)
            .child(
                div()
                    .ml_auto()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.main_path.display().to_string()),
            )
    }
    /// 工具栏上的录屏按钮：空闲是「录屏」，录制中变红带计时，收尾中变字。
    ///
    /// 这个按钮**本身也会被录进画面** —— 所以它得一眼看得出「正在录」，
    /// 不能只靠状态栏那行小字。
    pub(crate) fn render_record_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let recording = self.rec.is_some();
        let paused = matches!(
            self.rec.as_ref().map(|r| r.state()),
            Some(record::State::Paused)
        );
        let (label, hint) = if self.rec_finalizing {
            (
                "收尾中…".to_string(),
                "正在拼接 / 合成，完了状态栏会说存到哪",
            )
        } else if recording {
            (
                format!(
                    "⏺ {} 停止",
                    record::elapsed_label(self.rec_elapsed.as_secs())
                ),
                "停止录屏（Ctrl+Alt+R）",
            )
        } else {
            (
                "录屏".to_string(),
                "录屏（Ctrl+Alt+R）：只录本软件窗口那一块 + 麦克风",
            )
        };

        let button = if recording {
            Button::new("tb-record")
                .flex_shrink_0()
                .danger()
                .xsmall()
                .label(label)
        } else {
            Button::new("tb-record")
                .flex_shrink_0()
                .ghost()
                .xsmall()
                .label(label)
        }
        .tooltip(hint)
        .on_click(cx.listener(|this, _, window, cx| this.toggle_recording(window, cx)));

        let mut row = h_flex()
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .child(button);
        // 暂停/继续只在录制中出现（平时不占工具栏）
        if recording {
            row = row.child(
                Button::new("tb-record-pause")
                    .flex_shrink_0()
                    .ghost()
                    .xsmall()
                    .label(if paused { "▶ 继续" } else { "⏸ 暂停" })
                    .tooltip("暂停 / 继续（Ctrl+Alt+P）：暂停是把这一段收干净，继续时接着录")
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_record_pause(cx))),
            );
        }
        row
    }

    /// 中间那三块：按「视图」菜单里的显隐装进可拖动分区。
    ///
    /// 隐藏 = **不装进去**（而不是把宽度设 0）：这样分隔条也不会留下，只留一块时
    /// 它自己占满 —— `ResizableState::sync_panels_count` 本来就有「面板数量变了」
    /// 的分支，会按容器重新分配尺寸。
    pub(crate) fn render_main_area(
        &self,
        sidebar: impl IntoElement,
        editor: impl IntoElement,
        preview: impl IntoElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();

        // 三块都关了：给一句话，不然一片空白看起来就是卡死了
        if !self.show_tree && !self.show_editor && !self.show_preview {
            return v_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .bg(theme.secondary)
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("三块面板都隐藏了 —— 从「视图」菜单里把它们打开")
                .into_any_element();
        }

        let mut panels: Vec<gpui_component::resizable::ResizablePanel> = Vec::new();
        if self.show_tree {
            // 侧栏给个尺寸范围：拖到过窄会把文字挤爆
            panels.push(
                resizable_panel()
                    .size(px(180.))
                    .size_range(px(150.)..px(420.))
                    .child(sidebar),
            );
        }
        if self.show_editor {
            // 编辑区：**下限定得比内容最小宽度大**，否则拖到很窄时内容（Input）
            // 会溢出到左边的侧栏上，看起来就是「覆盖目录」。
            panels.push(
                resizable_panel()
                    .size(px(700.))
                    .size_range(px(400.)..px(2600.))
                    .child(editor),
            );
        }
        if self.show_preview {
            panels.push(
                resizable_panel()
                    .size(px(700.))
                    .size_range(px(300.)..px(2600.))
                    .child(self.render_right_pane(preview.into_any_element(), cx)),
            );
        }

        // 注意：`ResizablePanelGroup` 自己的 render 里已经 `size_full + flex_1 +
        // min_h_0/min_w_0`，所以这里**不要**再调 flex_1（它没有实现 Styled，
        // 调了也编译不过），直接当 flex 子项放就行。
        h_resizable("main-split")
            .with_state(&self.split_state)
            .children(panels)
            .into_any_element()
    }

    /// 底部状态栏：**一行**装下引擎指标 + 文档统计 + 保存/错误 + 一次性消息 + 主题。
    ///
    /// 放在窗口最底部（终端面板之下）—— 与大多数编辑器一致：
    /// 上面是「内容」，最下面一条是「状态」。
    ///
    /// 指标一多就必须想清楚**哪部分可以牺牲**：左侧指标放一个 `flex_1` +
    /// `overflow_hidden` 的容器里（窄窗口下宁可裁掉几个数字），
    /// 右侧的消息与主题下拉是**固定**的，永远看得见。
    /// 状态栏那一格写什么（四种状态的定义与测试在 `autosave::status_label`）。
    fn save_label(&self) -> &'static str {
        autosave::status_label(self.dirty, self.autosave)
    }

    pub(crate) fn render_statusbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        // 三组、组间一条竖线。以前是十几段文字平铺在一行里，
        // 「性能」「当前视图」「文档状态」的边界完全看不出来（用户提的第 5 条）。
        //
        // 每个指标都 `flex_shrink_0`：状态栏一窄就会把文字压到互相重叠
        // （与「页框被 flex 压扁」是同一个坑），要裁就整体裁掉右边几个。
        let divider = || {
            div()
                .w(px(1.))
                .h(px(12.))
                .flex_shrink_0()
                .bg(theme.muted_foreground.opacity(0.3))
        };

        // ① 性能：这次排版/光栅化花了多久、重解析了多少、各自跑了几次。
        //    **默认不显示**（状态栏保持精简），从「视图 → 显示性能指标」打开。
        let performance: Option<AnyElement> = self.show_metrics.then(|| {
            let mut performance = h_flex()
                .flex_shrink_0()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme.primary)
                        .child(format!("排版 {:.1}ms", self.status.compile_ms)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .child(format!("光栅化 {:.1}ms", self.status.raster_ms)),
                );

            if self.index_builds > 0 {
                performance = performance.child(
                    div()
                        .flex_shrink_0()
                        .child(format!("索引 {:.1}ms", self.index_ms)),
                );
            }

            performance = performance
                .child(div().flex_shrink_0().child(format!(
                    "重解析 {}B/{}B",
                    self.status.reparsed, self.status.text_bytes
                )))
                .child(div().flex_shrink_0().child(format!(
                    "排版{}·光栅{}·索引{} 次",
                    self.status.compiles, self.status.rasters, self.index_builds
                )));
            performance.into_any_element()
        });

        // ② 当前视图：第几页 / 缩放 / 纹理占了多少显存
        let view_group = h_flex()
            .flex_shrink_0()
            .gap_2()
            .child(div().flex_shrink_0().child(if self.status.pages == 0 {
                "0 页".to_string()
            } else {
                format!("{}/{} 页", self.current_page + 1, self.status.pages)
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.primary)
                    .child(if self.zoom_fit {
                        format!("充满 {:.0}%", self.zoom * 100.0)
                    } else {
                        format!("{:.0}%", self.zoom * 100.0)
                    }),
            )
            .child(div().flex_shrink_0().child(format!(
                "{:.1}MiB",
                self.texture_bytes as f64 / (1024.0 * 1024.0)
            )));

        // ③ 文档状态：字数统计 / 标题数 / 存没存 / 有没有错
        let first_error_line = self
            .diags
            .iter()
            .filter(|d| d.is_error)
            .find_map(|d| d.line_col.as_ref().map(|pos| pos.line));

        let error_badge: AnyElement = if self.error_count > 0 {
            let badge = div()
                .flex_shrink_0()
                .px_1()
                .rounded_sm()
                .bg(theme.danger.opacity(0.14))
                .text_color(theme.danger)
                .child(format!("✗ {} 错误", self.error_count));

            match first_error_line {
                // 点一下跳到第一条错误 —— 状态栏是「看到红字」的地方，
                // 那就该能直接去修
                Some(line) => badge
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.danger.opacity(0.24)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            this.jump_to_line(line, window, cx);
                        }),
                    )
                    .into_any_element(),
                None => badge.into_any_element(),
            }
        } else {
            div()
                .flex_shrink_0()
                .text_color(theme.success)
                .child("✓ 无错误")
                .into_any_element()
        };

        let doc_group = h_flex()
            .flex_shrink_0()
            .gap_2()
            .child(div().flex_shrink_0().child(self.stats.summary()))
            .child(
                div()
                    .flex_shrink_0()
                    .child(format!("{} 标题", self.outline.len())),
            )
            // 保存状态那一格：**可点**，点它就是自动保存的开关。
            // 放这里是因为它本来就在回答「存了没」—— 想知道「为什么没存」的人
            // 眼睛已经在这一格上了，不必再教他去按 Ctrl+Alt+S。
            .child(
                div()
                    .id("save-cell")
                    .flex_shrink_0()
                    .cursor_pointer()
                    .rounded_sm()
                    .px_1()
                    .hover(|s| s.bg(theme.muted_foreground.opacity(0.14)))
                    .child(self.save_label())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            let on = !this.autosave;
                            this.set_autosave(on, cx);
                        }),
                    ),
            )
            .child(error_badge);

        let mut bar = h_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_3()
            .items_center()
            // 贴着窗口底边，所以边框在上（原先是顶栏，边框在下）
            .border_t_1()
            .border_color(theme.border)
            .text_sm()
            .child({
                let mut left = h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .items_center()
                    .gap_2()
                    .text_xs();
                if let Some(performance) = performance {
                    left = left.child(performance).child(divider());
                }
                left.child(view_group).child(divider()).child(doc_group)
            });

        if let Some(msg) = &self.message {
            bar = bar.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.primary)
                    .child(msg.clone()),
            );
        }

        // 主题下拉**搬进「视图」菜单**了（状态栏只留真正常用的东西）。
        // 这里放终端按钮：它是「按需叫出来」的面板，给个一眼能看见的入口。
        bar.child(
            Button::new("status-terminal")
                .flex_shrink_0()
                .ghost()
                .xsmall()
                .icon(IconName::SquareTerminal)
                .label("终端")
                .tooltip("打开 / 收起终端（Ctrl+4）")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_shell(window, cx))),
        )
    }
    /// 终端面板：标题栏 + 终端本体。与 `wu` 同一种摆法。
    pub(crate) fn render_shell(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let body: AnyElement = match &self.shell_terminal {
            Some(term) => {
                // 终端跟随应用主题的亮/暗（这与「用固定深色终端」是个取舍）：
                // 浅色主题下若还配深色终端，界面会像贴了块补丁；
                // 而浅色底必须配 `TerminalPalette::light()` —— ANSI 经典黄/亮黄
                // 在近白底上对比度不到 1.5:1，基本看不清（`term_colors` 里有说明）。
                let (fg, bg, palette) = if theme.is_dark() {
                    (
                        gpui::white(),
                        gpui::rgb(0x1e1e1e).into(),
                        TerminalPalette::dark(),
                    )
                } else {
                    (
                        gpui::rgb(0x24292e).into(),
                        gpui::rgb(0xfbfbfb).into(),
                        TerminalPalette::light(),
                    )
                };
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .child(
                        terminal_view::TerminalElement::new(
                            term.clone(),
                            self.terminal_focus.clone(),
                            self.terminal_ime.clone(),
                        )
                        .colors(fg, bg)
                        .palette(palette)
                        .min_contrast(4.5)
                        .track_focus(&self.terminal_focus),
                    )
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("终端没起来（启动时失败，看终端日志）")
                .into_any_element(),
        };

        v_flex()
            .w_full()
            .h(px(260.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .h(px(24.))
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .text_xs()
                    // 面板标题只写「终端」：**没有关闭按钮**了 ——
                    // 收起面板用状态栏那个「终端」按钮或 `Ctrl+4`，
                    // 面板上再放一个「关闭」只会让人以为「关了就没了」。
                    .child("终端"),
            )
            .child(body)
            .into_any_element()
    }
}
