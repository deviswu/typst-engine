p = 'crates/app/src/main.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, f"匹配 {s.count(old)} 次：{old[:70]!r}"
    s = s.replace(old, new)


# ① 动作 + 字段
rep("""        AiToggleHunk,
    ]
);""", """        AiToggleHunk,
        ToggleShell,
    ]
);""")

rep("""use std::path::{Path, PathBuf};""", """use std::path::{Path, PathBuf};
use std::rc::Rc;""")

rep("""use image_view::ImageView;""", """use image_view::ImageView;
use term_colors::TerminalPalette;""")

rep("""    /// 多轮对话历史（`(是否用户, 文本)`）—— 连续按 Ctrl+K 时模型能记住上文
    ai_history: Vec<(bool, String)>,""",
    """    /// 多轮对话历史（`(是否用户, 文本)`）—— 连续按 Ctrl+K 时模型能记住上文
    ai_history: Vec<(bool, String)>,
    /// 交互式终端（alacritty + pty）。整个生命周期只在 UI 线程用，
    /// PTY 的读写线程由 alacritty 的 EventLoop 自己持有。
    shell_terminal: Option<Rc<terminal::Terminal>>,
    /// 终端自己的焦点：它得能拿到键盘输入
    terminal_focus: FocusHandle,
    /// 终端的输入法状态（中文输入时的预编辑文本）
    terminal_ime: Entity<terminal_view::ImeState>,
    /// 终端面板是否可见（Ctrl+4 切换）
    shell_visible: bool,""")

# ② 构造：起终端 + 事件泵
rep("""        // AI 编辑的要求输入框（与快速打开同一个套路：常驻，开关只切可见性）""",
    """        // 交互式终端：Windows 上用 PowerShell，其它平台按 $SHELL 找。
        let terminal_focus = cx.focus_handle();
        let terminal_ime = cx.new(|_| terminal_view::ImeState { marked_text: None });
        let shell_terminal = {
            let shell = default_shell();
            let start_dir = root.clone();
            match terminal::Terminal::new(&shell, Some(start_dir), 100, 30) {
                Ok(term) => {
                    println!("[typst-live] 终端已启动：{shell}");
                    Some(Rc::new(term))
                }
                Err(err) => {
                    // 终端起不来不该影响编辑器：记一条，继续跑
                    println!("[typst-live] 终端启动失败：{err}");
                    None
                }
            }
        };

        // AI 编辑的要求输入框（与快速打开同一个套路：常驻，开关只切可见性）""")

rep("""            ai: AiEdit::default(),
            ai_history: Vec::new(),""",
    """            ai: AiEdit::default(),
            ai_history: Vec::new(),
            shell_terminal,
            terminal_focus,
            terminal_ime,
            // 终端默认收起：它是「按需叫出来」的东西，一开窗就占半屏反而吵
            shell_visible: false,""")

# ③ 事件泵（自适应轮询，与 wu 同一套）
rep("""    // ── AI 编辑（Ctrl+K）─────────────────────────────────""",
    '''    // ── 终端 ──────────────────────────────────────────

    /// 终端事件泵：**自适应轮询**。
    ///
    /// 有输出时 50ms（回显跟手），连续空闲 2 秒后降到 120ms ——
    /// `wu` 特意从固定 50ms 改过来的：固定间隔在空闲时也是 20Hz 常量唤醒，
    /// 白烧 CPU；120ms 上限又保证空闲后第一次敲键的回显延迟肉眼不可感。
    fn spawn_terminal_pump(&self, term: Rc<terminal::Terminal>, cx: &mut Context<Self>) {
        let view = cx.entity();
        cx.spawn(async move |_weak, cx| {
            let mut idle_rounds: u32 = 0;
            loop {
                let interval = if idle_rounds > 40 { 120 } else { 50 };
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;

                let events = term.drain_events();
                if events.is_empty() {
                    idle_rounds = idle_rounds.saturating_add(1);
                    continue;
                }
                idle_rounds = 0;

                let has_wakeup = events
                    .iter()
                    .any(|event| matches!(event, terminal::TermEvent::Wakeup));
                let exited = events.iter().find_map(|event| match event {
                    terminal::TermEvent::ChildExit(code) => Some(*code),
                    _ => None,
                });

                _ = view.update(cx, |this, cx| {
                    if let Some(code) = exited {
                        this.message = Some(format!("终端已退出（退出码 {code:?}）"));
                    }
                    if has_wakeup {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// 终端面板：标题栏 + 终端本体。与 `wu` 同一种摆法。
    fn render_shell(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let body: AnyElement = match &self.shell_terminal {
            Some(term) => {
                // 终端不跟随应用主题：它是「程序输出」，用固定深色底 + ANSI 调色板
                // （浅色底上 ANSI 经典黄/亮黄基本看不清，`term_colors` 里有说明）
                let fg = gpui::white();
                let bg = gpui::rgb(0x1e1e1e).into();
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
                        .palette(TerminalPalette::dark())
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
                    .child("终端")
                    .child(
                        div()
                            .ml_auto()
                            .child(
                                Button::new("shell-close")
                                    .ghost()
                                    .xsmall()
                                    .label("关闭")
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.shell_visible = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    // ── AI 编辑（Ctrl+K）─────────────────────────────────''')

# ④ 布局：面板挂在主区下方
rep("""            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(sidebar_pane)
                    .child(editor_pane)
                    .child(self.render_right_pane(preview_pane, cx)),
            )""",
    """            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(sidebar_pane)
                    .child(editor_pane)
                    .child(self.render_right_pane(preview_pane, cx)),
            )
            .children(self.shell_visible.then(|| self.render_shell(cx)))""")

# ⑤ 动作 + 快捷键 + 菜单
rep("""            .on_action(cx.listener(|this, _: &AiToggleHunk, _window, cx| {
                this.ai_toggle_hunk(cx);
            }))""",
    """            .on_action(cx.listener(|this, _: &AiToggleHunk, _window, cx| {
                this.ai_toggle_hunk(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleShell, window, cx| {
                this.shell_visible = !this.shell_visible;
                // 显示时把焦点交给终端，否则得先点一下才能打字
                if this.shell_visible {
                    this.terminal_focus.focus(window, cx);
                }
                cx.notify();
            }))""")

rep("""            KeyBinding::new("ctrl-k", AiEditOpen, None),""",
    """            KeyBinding::new("ctrl-k", AiEditOpen, None),
            // 终端显隐（与 wu 同一个键位）
            KeyBinding::new("ctrl-4", ToggleShell, None),""")

rep("""                        .separator()
                        .item(PopupMenuItem::new("排版预览").on_click(window.listener_for(""",
    """                        .item(PopupMenuItem::new("终端（Ctrl+4）").on_click(
                            window.listener_for(&view, |this, _, window, cx| {
                                this.shell_visible = !this.shell_visible;
                                if this.shell_visible {
                                    this.terminal_focus.focus(window, cx);
                                }
                                cx.notify();
                            }),
                        ))
                        .separator()
                        .item(PopupMenuItem::new("排版预览").on_click(window.listener_for(""")

# ⑥ 起泵：构造末尾（首次排版之后，终端已经有了）
rep("""        // **不在这里编译**：45 页冷编译 200+ ms，同步做的话窗口要等它排完
        // 才出现。推迟到第一帧画完（见 `render` 里的 `first_compile`）。
        this""",
    """        // 终端事件泵：终端在建世界时就起来了，泵要等实体建好再挂
        if let Some(term) = this.shell_terminal.clone() {
            this.spawn_terminal_pump(term, cx);
        }

        // **不在这里编译**：45 页冷编译 200+ ms，同步做的话窗口要等它排完
        // 才出现。推迟到第一帧画完（见 `render` 里的 `first_compile`）。
        this""")

# ⑦ shell 的选择
rep("""/// 主题那行日志的正文。""",
    """/// 默认 shell：Windows 上 PowerShell，其它平台看 `$SHELL`。
fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// 主题那行日志的正文。""")

open(p, 'w', encoding='utf-8').write(s)
print("终端接线写好")
