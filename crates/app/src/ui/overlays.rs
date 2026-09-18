//! 浮层：AI 编辑、快速打开（Ctrl+P）、关窗前的未保存提示。
//!
//! 三者都是「绝对定位盖在最上面」的同一套摆法，放在一起是为了让
//! 「新加一个浮层该照着谁写」一眼可见。

use crate::*;

impl Previewer {
    /// 「AI 设置」浮层：端点 / 模型 / Key。
    ///
    /// 壳与关窗提示同一套（绝对定位 + 背景遮罩 + Esc 关）。Enter 存盘由三个
    /// 输入框自己的订阅负责（填完顺手一敲），这里只管画与按钮。
    pub(crate) fn render_ai_settings(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let field = |label: &'static str, hint: &'static str, input: &Entity<InputState>| {
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(Input::new(input).w_full())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(hint),
                )
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(gpui::black().opacity(0.35))
            .occlude()
            .child(
                v_flex()
                    // Esc 走**按键上下文**而不是「把焦点抢到浮层上」：
                    // 三个输入框得能拿到键盘（焦点抢过来的话就一个字都打不进去）。
                    // 与 AI 浮层同一个套路，见 `main.rs` 那几行 KeyBinding。
                    .key_context("AiSettings")
                    .w(px(560.))
                    .p_4()
                    .gap_3()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("AI 设置"),
                    )
                    .child(field(
                        "端点（默认 https://api.deepseek.com/chat/completions）",
                        "任何 OpenAI 兼容的 /chat/completions 都行；本地模型填 127.0.0.1 之类",
                        &self.ai_settings_form.base_url,
                    ))
                    .child(field(
                        "模型（默认 deepseek-flash）",
                        "端点认什么名字就填什么",
                        &self.ai_settings_form.model,
                    ))
                    .child(field(
                        "API Key（留空 = 不配）",
                        "存进 settings.conf（明文，与多数命令行工具一样）；环境变量 AI_API_KEY 优先",
                        &self.ai_settings_form.key,
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("ai-settings-cancel").label("取消").on_click(
                                cx.listener(|this, _, _window, cx| this.close_ai_settings(cx)),
                            ))
                            .child(
                                Button::new("ai-settings-save")
                                    .primary()
                                    .label("保存（Enter）")
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.save_ai_settings(cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// AI 编辑浮层：输入要求 → 生成中 → 逐块确认。
    ///
    /// 三个阶段共用一层壳（标题 + 内容 + 底部提示），只在中间换内容。
    pub(crate) fn render_ai(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let (title, hint) = match self.ai.stage {
            AiStage::Closed => return div().into_any_element(),
            AiStage::Ask => ("AI 编辑", "Enter 发送 · 上面几个小按钮换上下文 · Esc 取消"),
            AiStage::Running => ("AI 编辑 · 生成中", "Esc 取消"),
            AiStage::Review => match ai_scope::landing(&self.ai.scope, &self.main_path) {
                // 插入与改写是两种不同的落地方式，提示要说准 —— 一处说「插入」、
                // 一处说「写回文件」、其它说「应用」。
                ai_scope::Landing::Insert { .. } => {
                    ("AI 编辑 · 插入到光标", "Enter 插入 · Esc 放弃")
                }
                ai_scope::Landing::File(_) => (
                    "AI 编辑 · 确认改动",
                    "Enter 写回文件 · Tab/空格 切换本块 · ↑↓ 选块 · Esc 放弃",
                ),
                _ => (
                    "AI 编辑 · 确认改动",
                    "Enter 应用 · Tab/空格 切换本块 · ↑↓ 选块 · Esc 放弃",
                ),
            },
        };

        let inner: AnyElement =
            match self.ai.stage {
                AiStage::Closed => div().into_any_element(),

                AiStage::Ask | AiStage::Running => {
                    let mut column = v_flex().w_full().gap_2();

                    // 上下文范围：点一下就换（照 `wu` 的对话框：一排小按钮，当前那个高亮）。
                    //
                    // 只在 Ask 阶段给：一旦发出去，范围就定死了；打开着的文件范围也不给 ——
                    // 那个入口在目录树右键上，切过去没意义。
                    if self.ai.stage == AiStage::Ask && !matches!(self.ai.scope, AiScope::File(_)) {
                        let active = self.ai_scope_choice();
                        let mut row = h_flex().w_full().gap_1().items_center().child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("上下文"),
                        );
                        for choice in self.ai_scope_choices(cx) {
                            let button = Button::new(format!("ai-scope-{}", choice.label()))
                                .xsmall()
                                .label(choice.label());
                            let button = if Some(choice) == active {
                                button.primary()
                            } else {
                                button.ghost()
                            };
                            row = row.child(button.on_click(cx.listener(
                                move |this, _, _window, cx| this.set_ai_scope(choice, cx),
                            )));
                        }
                        column = column.child(row);
                    }

                    if self.ai.stage == AiStage::Running {
                        let generated = self.ai.progress.load(Ordering::Relaxed);
                        column = column.child(
                            div()
                                .text_sm()
                                .text_color(theme.primary)
                                .child(format!("已生成 {generated} 字…")),
                        );
                    }
                    column = column.child(Input::new(&self.ai_input).w_full());
                    column.into_any_element()
                }

                AiStage::Review => {
                    // 插入模式：没有 diff 可看（回复本来就是**新**内容，不是对上下文的
                    // 改写），把回复整段摆出来就行。
                    if matches!(self.ai.scope, AiScope::Insert { .. }) {
                        v_flex()
                            .id("ai-insert")
                            .w_full()
                            .gap_1()
                            .max_h(px(380.))
                            .overflow_y_scroll()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                format!(
                                    "将插入 {} 字到光标处（原文一个字不动）",
                                    self.ai.result.chars().count()
                                ),
                            ))
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_0p5()
                                    // 逐行画：一整段带 `\n` 的文本丢给一个 div，换行
                                    // 不一定按预期断行（hunk 那边也是逐行画的）。
                                    .children(
                                        self.ai
                                            .result
                                            .lines()
                                            .map(|line| {
                                                div()
                                                    .text_xs()
                                                    .font_family(theme.mono_font_family.clone())
                                                    .child(line.to_string())
                                            })
                                            .collect::<Vec<_>>(),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        let mut list = v_flex()
                            .id("ai-hunks")
                            .w_full()
                            .gap_1()
                            .max_h(px(380.))
                            .overflow_y_scroll();
                        for (index, hunk) in self.ai.hunks.iter().enumerate() {
                            let accepted = self.ai.accepted.get(index).copied().unwrap_or(true);
                            let selected = index == self.ai.selected;

                            let mut block = v_flex()
                                .w_full()
                                .rounded(theme.radius)
                                .px_2()
                                .py_1()
                                .gap_0p5()
                                .bg(if selected {
                                    theme.accent.opacity(0.18)
                                } else {
                                    theme.secondary
                                })
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .child(div().text_color(theme.primary).child(format!(
                                            "第 {} 块  +{}  -{}",
                                            index + 1,
                                            hunk.added(),
                                            hunk.removed()
                                        )))
                                        .child(
                                            div()
                                                .text_color(if accepted {
                                                    theme.success
                                                } else {
                                                    theme.muted_foreground
                                                })
                                                .child(if accepted { "接受" } else { "拒绝" }),
                                        ),
                                );

                            for line in &hunk.lines {
                                let (sign, color) = match line.kind {
                                    diff::DiffKind::Add => ("+", theme.success),
                                    diff::DiffKind::Del => ("-", theme.danger),
                                };
                                block = block.child(
                                    div()
                                        .text_xs()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(color)
                                        .child(format!("{sign} {}", line.text)),
                                );
                            }

                            list = list.child(block);
                        }

                        if self.ai.hunks.is_empty() {
                            list = list.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("模型返回的内容与原文本一致（没有可应用的改动）"),
                            );
                        }

                        list.into_any_element()
                    }
                }
            };

        // 错误行不分阶段——生成中、确认改动那一屏都要看得到
        // （否则「没配 Key」这类错在 Review 阶段就消失了）。
        let body: AnyElement = v_flex()
            .w_full()
            .gap_2()
            .child(inner)
            .children(self.ai.error.as_ref().map(|error| {
                div()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(format!("失败：{error}"))
            }))
            .into_any_element();

        let panel = v_flex()
            .key_context("AiEdit")
            .track_focus(&self.ai_focus)
            .w(px(700.))
            .max_h(px(560.))
            .gap_2()
            .p_3()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child(title))
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.ai.scope_label.clone()),
                    ),
            )
            .child(body)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(hint),
            );

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(90.))
            .bg(gpui::black().opacity(0.25))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.cancel_ai(cx)),
            )
            .child(panel)
            .into_any_element()
    }
    /// 关窗提示浮层：有未保存的改动，先问你一句。
    ///
    /// 与快速打开同一套摆法（根链最后的绝对定位层）。两处不同：
    /// ① 点空白**不关** —— 这是要在三个选项里选一个的地方，误点不该等于「放弃」；
    /// ② 自己 `track_focus`，Esc 才收得到（否则键盘焦点还在编辑器里）。
    /// 「打开文件夹…」的浮层：一层层往下点，确认就把这个目录变成工作区目录。
    ///
    /// 不用系统文件对话框是有意的 —— 原生对话框会在 gpui 里开嵌套消息循环，
    /// 本项目被那个咬过（借用竞态 → 进程退）。见 `FolderPicker` 的注释。
    pub(crate) fn render_folder_picker(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(picker) = self.folder_picker.as_ref() else {
            return div().into_any_element();
        };

        let cwd = picker.cwd.clone();
        let dirs = picker.dirs.clone();
        let roots = tree::roots();

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(gpui::black().opacity(0.35))
            .occlude()
            .track_focus(&self.folder_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                if ev.keystroke.key == "escape" {
                    this.folder_picker_cancel(cx);
                }
            }))
            .child(
                v_flex()
                    .w(px(560.))
                    .max_h(px(560.))
                    .p_4()
                    .gap_3()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("选择工作区文件夹"),
                    )
                    // 现在停在哪。长路径让它换行显示，不截断
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(cwd.to_string_lossy().to_string()),
                    )
                    .child(
                        v_flex()
                            .id("folder-list")
                            .flex_1()
                            .min_h(px(140.))
                            .overflow_y_scroll()
                            .gap_0p5()
                            .child(self.folder_row("..", "..（上一层）", cx, |this, cx| {
                                this.folder_picker_up(cx)
                            }))
                            .children(roots.into_iter().map(|root| {
                                let label = root.to_string_lossy().to_string();
                                self.folder_row(
                                    &format!("root-{label}"),
                                    &label,
                                    cx,
                                    move |this, cx| this.folder_picker_into(root.clone(), cx),
                                )
                            }))
                            .children(dirs.into_iter().map(|dir| {
                                let name = dir
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                let label = format!("· {name}");
                                self.folder_row(
                                    &format!("dir-{}", dir.display()),
                                    &label,
                                    cx,
                                    move |this, cx| this.folder_picker_into(dir.clone(), cx),
                                )
                            })),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("folder-cancel").label("取消").on_click(
                                cx.listener(|this, _, _window, cx| this.folder_picker_cancel(cx)),
                            ))
                            .child(
                                Button::new("folder-use")
                                    .primary()
                                    .label("用这个文件夹")
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.folder_picker_confirm(cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// 选择浮层里的一行（点一下进这个目录）。
    ///
    /// `on_click` 要求元素有 `id`（gpui 只有有状态的元素才收点击），
    /// 所以这一行必须带 `id` —— 少写它编译期就报「no method named `on_click`」。
    fn folder_row(
        &self,
        id: &str,
        label: &str,
        cx: &Context<Self>,
        action: impl Fn(&mut Previewer, &mut Context<Previewer>) + 'static,
    ) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .id(id.to_owned())
            .w_full()
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .text_sm()
            .hover(|style| style.bg(theme.accent.opacity(0.12)))
            .child(label.to_owned())
            .on_click(cx.listener(move |this, _, _window, cx| action(this, cx)))
            .into_any_element()
    }

    pub(crate) fn render_close_prompt(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let name = short_label(&self.main_path);

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(gpui::black().opacity(0.35))
            .occlude()
            .track_focus(&self.close_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_cancel(cx);
                }
            }))
            .child(
                v_flex()
                    .w(px(460.))
                    .p_4()
                    .gap_3()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("有未保存的改动"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!("{name} 改过但还没写进文件。现在关掉，改动就没了。")),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("close-cancel").label("取消").on_click(
                                cx.listener(|this, _, _window, cx| this.close_cancel(cx)),
                            ))
                            .child(Button::new("close-discard").label("放弃改动").on_click(
                                cx.listener(|this, _, window, cx| this.close_discard(window, cx)),
                            ))
                            .child(
                                Button::new("close-save")
                                    .primary()
                                    .label("保存并关闭")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_save(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
