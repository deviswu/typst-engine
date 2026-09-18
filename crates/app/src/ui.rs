//! `Previewer` 的**渲染层**。
//!
//! 这里只画东西：状态与逻辑在 `main.rs`，光栅化/纹理在 `preview.rs`，
//! 跳转在 `jump_glue.rs`。拆开是因为 `main.rs` 一度到了 4000 行 ——
//! 一个文件里同时有「状态机」「排版流程」「26 个按钮怎么排」之后，
//! 改动靠搜索而不是靠阅读，那是维护成本的开始。

use self::util::*;
use crate::*;

pub(crate) mod chrome;
pub(crate) mod overlays;
pub(crate) mod panes;
pub(crate) mod util;

impl Render for Previewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // AI 评审阶段把焦点拿到浮层上：gpui 的动作派发**从聚焦节点开始**，
        // 没有焦点就没有起点（NEXT.md 里那个坑：所有快捷键静默失效）。
        if self.ai.stage == AiStage::Review && !self.ai_focus.is_focused(window) {
            self.ai_focus.focus(window, cx);
        }

        // 关窗提示出来了就把焦点收到浮层上：Esc 取消得先收得到键盘 ——
        // 否则焦点还在编辑器里，那一按只是往文档里写了个转义符。
        if self.close_prompt && !self.close_focus.is_focused(window) {
            self.close_focus.focus(window, cx);
        }

        // 首次排版**推迟到这一帧画完之后**再跑：先让窗口出现（带一句
        // 「首次排版中…」），别让用户盯着一个还没画出来的窗口等 200+ ms。
        if self.first_compile {
            self.first_compile = false;
            logln!("[typst-live] 首帧 @{:.0} ms（窗口已画）", since_start_ms());
            cx.spawn(async move |this, cx| {
                // 让出一轮执行器，确保这一帧真的画出去了再排
                cx.background_executor().timer(Duration::ZERO).await;
                let _ = safe_task_update(&this, cx, |this, cx| {
                    logln!("[typst-live] 首次排版 @{:.0} ms", since_start_ms());
                    this.recompile(cx);
                    logln!(
                        "[typst-live] 首次排版完成：{:.1} ms（{} 页）@{:.0} ms",
                        this.status.compile_ms,
                        this.status.pages,
                        since_start_ms()
                    );
                });
            })
            .detach();
        }

        // 窗口尺寸/位置变了就记下来（拖动时会变很多次，所以写盘是防抖的）。
        //
        // ★ 用 `window_bounds()` 而不是 `bounds()`：前者就是「下次开窗该用哪份
        // 几何」，与我们交给 `WindowOptions` 的是同一个坐标系；`bounds()` 报的是
        // **客户区**，比请求值差一个非客户区高度（本机实测 **+11px**）——
        // 拿它存下来，窗口每开一次就在屏幕上往下爬 11px。
        if let WindowBounds::Windowed(rect) = window.window_bounds() {
            let boxed = (
                rect.origin.x.as_f32() as i32,
                rect.origin.y.as_f32() as i32,
                rect.size.width.as_f32() as i32,
                rect.size.height.as_f32() as i32,
            );
            if self.pending_window != Some(boxed) {
                self.pending_window = Some(boxed);
                self.touch_settings(cx);
            }
        }

        // 下面几项都要 `&mut cx`，而 `cx.theme()` 是不可变借用 ——
        // 所以它们必须放在拿 theme 之前（这个坑在 NEXT.md 里记着）。
        //
        // 开机报一次布局：窗口多宽、三块分区各多宽
        self.report_layout_once(window, cx);

        // 「适应宽度」：按当前展示区宽度重算缩放（拖动分区/改窗口都会走到这）
        self.fit_zoom_to_viewport();

        // 按当前视口补出/卸载纹理。内部只在可见范围真的变了才动手，
        // 所以滚动过程中每帧调它是安全的（就是两次二分查找）。
        self.sync_visible_pages();

        // 双击编辑区后的前向跳转。放在 render 里而不是鼠标回调里，
        // 是因为此刻编辑器已经把光标放到点击处了 —— 读到的位置才准。
        if self.pending_forward {
            self.pending_forward = false;
            self.jump_to_preview(cx);
        }

        // 磁盘上的新版本（轮询任务提在 `pending_reload` 上，见 `spawn_disk_watch`）。
        // **推迟到这一帧画完**再装：`set_value` 要 `&mut Window`，而装完还要重排一次
        // （几十毫秒）—— 不该压在这一帧的渲染里（与 `first_compile` 同一个道理）。
        if let Some((text, stamp)) = self.pending_reload.take() {
            let weak = cx.entity().downgrade();
            window.defer(cx, move |window, cx| {
                let _ = safe_task_update(&weak, cx, |this, cx| {
                    this.apply_disk_reload(text, stamp, window, cx)
                });
            });
        }

        // 外部改动的轮询：首帧之后才起（那之前连窗口都还没画出来）。
        if !self.disk_polling {
            self.spawn_disk_watch(cx);
        }

        let theme = cx.theme();
        let has_errors = !self.compile_errors.is_empty();

        let sidebar_pane = self.render_sidebar(cx);

        // 工具栏：一键插入，分组画分隔条 —— 条目与分组都对着 `wu` 的工具栏。
        // 工具栏**全宽**（放在菜单条与主区之间）—— 26 个按钮塞进 520px 的编辑区
        // 会横向溢出，看起来就是「按钮重叠」。`wu` 也是全宽一条。
        let toolbar = {
            let mut bar = h_flex()
                .id("toolbar")
                .w_full()
                .flex_shrink_0()
                // 40px 而不是 36：12px 的字在 36px 的条里上下只剩 6px，
                // 看起来像被挤扁了（用户提的第一条就是这个）。
                .h(px(40.))
                .px_3()
                // 按钮之间 8px：4px 时相邻按钮几乎粘在一起，分组边界看不出来
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.sidebar)
                // 窄窗口里横向滚，别把按钮挤没
                .overflow_x_scroll();

            for (index, group) in Markup::GROUPS.iter().enumerate() {
                if index > 0 {
                    // 分隔条画得比按钮高一点、深一点 —— 1px 浅灰几乎看不见，
                    // 等于没分组（这条也是用户提的）。
                    bar = bar.child(
                        div()
                            .w(px(1.))
                            .h(px(22.))
                            .flex_shrink_0()
                            .bg(theme.muted_foreground.opacity(0.35)),
                    );
                }

                match index {
                    // 颜色：九个颜色的下拉（wu 也是下拉）
                    5 => {
                        let view = cx.entity();
                        bar = bar.child(
                            Button::new("tb-color")
                                .flex_shrink_0()
                                .ghost()
                                .xsmall()
                                .label("颜色")
                                .tooltip("包住选中：换这个颜色")
                                .dropdown_menu(move |menu, window, _cx| {
                                    let view = view.clone();
                                    let mut menu = menu.min_w(120.);
                                    for name in markup::COLORS {
                                        menu = menu.item(
                                            PopupMenuItem::new(color_label(name)).on_click(
                                                window.listener_for(
                                                    &view,
                                                    move |this, _, window, cx| {
                                                        this.apply_markup(
                                                            Markup::TextColor(name),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            ),
                                        );
                                    }
                                    menu
                                }),
                        );
                    }

                    // AI 那一组**不再单独画一个入口**：顶上的「AI」菜单是同一个
                    // 下拉（内容一模一样），两个入口只会让人犹豫点哪个。
                    // 这里留空 = 只是不画按钮（分组本身还留在 `Markup::GROUPS` 里，
                    // 组间的分隔条还在，工具栏的视觉分组没变）。
                    6 => {}

                    _ => {
                        for kind in group.iter().copied() {
                            bar = bar.child(
                                // id 加前缀：工具栏的「图片」与右侧页签的「图片」会撞名
                                Button::new(format!("tb:{}", kind.label()))
                                    .flex_shrink_0()
                                    .ghost()
                                    .xsmall()
                                    .label(kind.label())
                                    .tooltip(kind.hint())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.apply_markup(kind, window, cx)
                                    })),
                            );
                        }
                    }
                }
            }

            bar
        };

        // 编辑区不要圆角也不要边线：只靠**底色**与展示区区分。
        // `Input` 自带的边框/圆角/焦点描边都要关掉（appearance 管底色，保留它）。
        let editor_pane = v_flex()
            // ★ 必须 `w_full`：分区只负责给位置与尺寸，内容自己不撑起来的话
            // 会缩成「内容宽度」（实测 875px 的面板里内容只有 117px）
            .w_full()
            .h_full()
            .min_w_0()
            // 内容不许溢出：拖窄时该裁剪，而不是盖到左边的目录/大纲上
            .overflow_hidden()
            .bg(theme.background)
            // 双击编辑区 → 显示区。
            //
            // **不阻止事件**：编辑器自己的「双击选词」照常发生，
            // 两件事互不干扰（选中的词也会一并被前向跳转命中）。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    if ev.click_count < 2 {
                        return;
                    }
                    // 不当场跳：等这一轮事件跑完，编辑器把光标挪到点击处了，
                    // 那时读到的光标位置才是准的（在 render 里读）。
                    this.pending_forward = true;
                    cx.notify();
                }),
            )
            .child(
                Input::new(&self.editor)
                    .bordered(false)
                    .focus_bordered(false)
                    .flex_1(),
            );

        // 布局尺寸一律用 `page_sizes`（按 pt 算出来的），**不用纹理的像素尺寸**。
        // 两者能对上（纹理 = pt × ppp，逻辑 = 纹理 / 屏缩放 = pt × 96/72 × zoom），
        // 但用前者才能保证「某一页还没出图」时页面尺寸不变 —— 否则
        // 滚动条会在滚到新页的瞬间跳一下。
        let page_rows: Vec<(f32, f32, Option<Arc<RenderImage>>)> = (0..self.bitmaps.len())
            .map(|i| {
                let (w, h) = self.page_sizes.get(i).copied().unwrap_or((0.0, 0.0));
                (w, h, self.bitmaps[i].clone())
            })
            .collect();

        // 高亮与缩放系数先收成自有数据，免得在子元素闭包里借用 `self`。
        let base = pixel_per_pt_for_zoom(self.zoom);
        let flash = self.flash.as_ref().map(|f| (f.page, f.rect, f.seq));

        // 预览区背景：纯色 / 网格。网格画在滚动层**底下**的一层画布上，
        // 所以它不跟着页面滚 —— 页框挪动时能当尺子用（「页面对齐了没」一眼看得出）。
        let viewport_bg = theme.secondary;
        let grid = self.preview_bg == PreviewBg::Grid;
        let grid_color = theme.muted_foreground.opacity(0.22);

        let pages_layer = v_flex()
            .id("pages")
            .flex_1()
            .h_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            // 网格模式下自己透明，让底下的画布透出来
            .bg(if grid {
                gpui::transparent_black()
            } else {
                viewport_bg
            })
            .gap_5()
            .pb_5()
            .items_center()
            .children(
                page_rows
                    .into_iter()
                    .enumerate()
                    .map(|(i, (w, h, texture))| {
                        let inner = match texture {
                            // 纹理是**物理像素**，而布局用的是**逻辑像素** —— 除回缩放系数
                            // 就是 1:1 映射，一个物理像素对一个物理像素，字才不发虚。
                            Some(texture) => img(ImageSource::Render(texture))
                                .w(px(w))
                                .h(px(h))
                                .into_any_element(),
                            // 占位：尺寸一样，所以不牵动布局；只是还没出图
                            None => div()
                                .w(px(w))
                                .h(px(h))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(gpui::black().opacity(0.18))
                                .child(format!("第 {} 页", i + 1))
                                .into_any_element(),
                        };
                        div()
                            // 高亮要绝对定位在页内，所以页框得是定位上下文
                            .relative()
                            .bg(gpui::white())
                            .shadow_md()
                            .overflow_hidden()
                            .w(px(w))
                            .h(px(h))
                            // ★ 必须禁掉收缩：flex 列容器默认 `flex-shrink: 1`，
                            // 会把超出视口高度的页全压扁塞进来 —— 结果是
                            // **容器永远不会溢出，滚轮因此没有任何东西可滚**，
                            // 而且视口光栅化会误以为「全部页都可见」而把纹理全出出来。
                            .flex_shrink_0()
                            // 双击这一页 → 光标跳到对应的源码处；
                            // Ctrl+单击 → 打开链接
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                    let Some((x, y)) = this.window_to_page_pt(ev.position, i)
                                    else {
                                        return;
                                    };
                                    if ev.click_count == 1 && ev.modifiers.control {
                                        this.open_link_at(i, x, y, cx);
                                        return;
                                    }
                                    if ev.click_count < 2 {
                                        return;
                                    }
                                    this.jump_to_source(i, x, y, window, cx);
                                }),
                            )
                            .child(inner)
                            .children(flash.and_then(|(page, rect, seq)| {
                                (page == i).then(|| {
                                    div()
                                        .absolute()
                                        .left(px(rect[0] * base))
                                        .top(px(rect[1] * base))
                                        .w(px((rect[2] - rect[0]).max(1.0) * base))
                                        .h(px((rect[3] - rect[1]).max(1.0) * base))
                                        .bg(theme.primary.opacity(0.28))
                                        // 一次性反馈，所以淡出。序号当 id：换了序号
                                        // 才是新元素、才会重新播一遍。
                                        .with_animation(
                                            ("jump-flash", seq),
                                            Animation::new(FLASH),
                                            |el, delta| el.opacity(1.0 - delta),
                                        )
                                        .into_any_element()
                                })
                            }))
                            .into_any_element()
                    }),
            );

        // 网格模式下套一层「底色 + 网格画布」，纯色模式直接用滚动层本身。
        let pages: AnyElement = if grid {
            div()
                .relative()
                .flex_1()
                .h_full()
                .bg(viewport_bg)
                .child(
                    canvas(
                        |bounds, _window, _cx| bounds,
                        move |_prepainted, bounds, window, _cx| {
                            paint_grid(bounds, grid_color, window);
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(pages_layer)
                .into_any_element()
        } else {
            pages_layer.into_any_element()
        };

        let preview_pane = if self.doc.is_none() {
            // 还没有排版结果（首次排版还在路上）—— 别给一片空白，
            // 让人知道它在干活
            v_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .bg(theme.secondary)
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("首次排版中…（大文档冷编译要几百毫秒）")
                .into_any_element()
        } else if has_errors {
            v_flex()
                .flex_1()
                .h_full()
                .child(pages)
                .child(self.render_errors(cx))
                .into_any_element()
        } else {
            pages.into_any_element()
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_menu(cx))
            .child(toolbar)
            // 中间三块用可拖动分区：编辑区与展示区的分界**能左右拖**
            // （拖的时候预览会自动按新宽度重排 —— 见 `fit_zoom_to_viewport`）
            .child(
                // 注意：`ResizablePanelGroup` 自己的 render 里已经 `size_full +
                // flex_1 + min_h_0/min_w_0`，所以这里**不要**再调 flex_1（它没有
                // 实现 Styled，调了也编译不过），直接当 flex 子项放就行。
                h_resizable("main-split")
                    .with_state(&self.split_state)
                    .child(
                        // 侧栏给个尺寸范围：拖到过窄会把文字挤爆
                        resizable_panel()
                            .size(px(180.))
                            .size_range(px(150.)..px(420.))
                            .child(sidebar_pane),
                    )
                    .child(
                        // 编辑区：**下限定得比内容最小宽度大**，否则拖到很窄时
                        // 内容（Input）会溢出到左边的侧栏上，看起来就是「覆盖目录」
                        // 编辑区 : 展示区 = **1 : 1**（用户要求）：两边等宽，
                        // 拖过分隔条之后按用户拖的比例走（`ResizableState` 不落盘，
                        // 所以下次打开又回到 1:1）。
                        resizable_panel()
                            .size(px(700.))
                            .size_range(px(400.)..px(2600.))
                            .child(editor_pane),
                    )
                    .child(
                        // 展示区与编辑区等宽。预览是「适应宽度」，自己会缩；
                        // 真嫌小，往左拖分隔条就行。
                        resizable_panel()
                            .size(px(700.))
                            .size_range(px(300.)..px(2600.))
                            .child(self.render_right_pane(preview_pane, cx)),
                    ),
            )
            .children(self.shell_visible.then(|| self.render_shell(cx)))
            // 状态栏贴窗口最底：上面是内容，最下面一条是状态
            .child(self.render_statusbar(cx))
            // 浮层放在最后，才能盖在内容之上
            .children((self.ai.stage != AiStage::Closed).then(|| self.render_ai(cx)))
            .children(self.ai_settings_open.then(|| self.render_ai_settings(cx)))
            // 关窗提示在最上面：它是「你不处理完就走不了」的那一层
            .children(
                self.folder_picker
                    .is_some()
                    .then(|| self.render_folder_picker(cx)),
            )
            .children(self.close_prompt.then(|| self.render_close_prompt(cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _window, cx| {
                let next = this.zoom * ZOOM_STEP;
                this.set_zoom(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _window, cx| {
                let next = this.zoom / ZOOM_STEP;
                this.set_zoom(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, _window, cx| {
                this.fit_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &SaveFile, _window, cx| {
                this.save_file(cx);
            }))
            .on_action(cx.listener(|this, _: &RecompileNow, _window, cx| {
                this.recompile_now(cx);
            }))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| {
                this.format_document(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExportPdf, _window, cx| {
                this.export_pdf(cx);
            }))
            .on_action(cx.listener(|this, _: &NextPage, _window, cx| {
                this.go_to_page(this.current_page + 1, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevPage, _window, cx| {
                let prev = this.current_page.saturating_sub(1);
                this.go_to_page(prev, cx);
            }))
            .on_action(cx.listener(|this, _: &FirstPage, _window, cx| {
                this.go_to_page(0, cx);
            }))
            .on_action(cx.listener(|this, _: &LastPage, _window, cx| {
                let last = this.bitmaps.len().saturating_sub(1);
                this.go_to_page(last, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncToPreview, _window, cx| {
                this.jump_to_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &AiEditOpen, window, cx| {
                this.open_ai(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AiSettingsCancel, _window, cx| {
                this.close_ai_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &AiSubmit, window, cx| {
                if this.ai.stage == AiStage::Review {
                    this.apply_ai(window, cx);
                } else {
                    this.start_ai(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &AiCancel, _window, cx| {
                this.cancel_ai(cx);
            }))
            .on_action(cx.listener(|this, _: &AiNextHunk, _window, cx| {
                this.ai_move(1, cx);
            }))
            .on_action(cx.listener(|this, _: &AiPrevHunk, _window, cx| {
                this.ai_move(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &AiToggleHunk, _window, cx| {
                this.ai_toggle_hunk(cx);
            }))
            .on_action(
                cx.listener(|this, _: &ToggleShell, window, cx| this.toggle_shell(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleFolding, window, cx| {
                let on = !this.folding;
                this.set_folding(on, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFollow, _window, cx| {
                let on = !this.follow_cursor;
                this.set_follow_cursor(on, cx);
            }))
            // 菜单里不放了（用户要求菜单只留主题）——这两项各给一个快捷键入口
            .on_action(cx.listener(|this, _: &TogglePreviewBg, _window, cx| {
                let next = if this.preview_bg == PreviewBg::Grid {
                    PreviewBg::Solid
                } else {
                    PreviewBg::Grid
                };
                this.set_preview_bg(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleMetrics, _window, cx| {
                let on = !this.show_metrics;
                this.set_show_metrics(on, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleAutosave, _window, cx| {
                let on = !this.autosave;
                this.set_autosave(on, cx);
            }))
            // Ctrl + 滚轮缩放。不阻止容器自身的滚动（gpui 没有 preventDefault），
            // 所以缩完之后把当前页重新锚回顶部 —— 一举两得：既抵消了误滚，
            // 又让“缩放不跳位置”成为确定行为。
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                if !ev.modifiers.control {
                    // 用户在自己滚预览 → 记下时间，「跟随光标」在静默期内不抢他的位置
                    if this.follow_cursor {
                        this.follow_pause = Some(Instant::now());
                    }
                    return;
                }
                let dy = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => p.y.as_f32() / 40.0,
                };
                if dy == 0.0 {
                    return;
                }
                let next = if dy > 0.0 {
                    this.zoom * ZOOM_STEP
                } else {
                    this.zoom / ZOOM_STEP
                };
                this.set_zoom_anchored(next, cx);
            }))
    }
}
