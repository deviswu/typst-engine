//! 终端封装：基于 alacritty_terminal（Zed 同款终端核心），
//! Windows 上用 ConPTY 提供真交互式终端（支持 agent、颜色、光标控制）。
//! 本模块只负责「终端后端」，不含任何 gpui 渲染，便于独立验证。

use alacritty_terminal::{
    event::{Event as AlacEvent, EventListener, Notify as _, WindowSize},
    event_loop::{EventLoop, Msg, Notifier},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, RenderableContent, Term},
    tty,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

/// 终端尺寸（列 × 行），实现 alacritty 的 `Dimensions` trait 以便创建/缩放终端。
#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    pub columns: usize,
    pub lines: usize,
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// 应用层关心的终端事件。
#[derive(Clone, Debug)]
pub enum TermEvent {
    /// 有新内容，需要重绘。
    Wakeup,
    /// 终端铃声。
    Bell,
    /// 子进程退出（Some(code)=退出码，None=未知）。
    ChildExit(Option<i32>),
}

/// 把 alacritty 事件转成应用层事件。
struct Listener(std::sync::mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        let ev = match event {
            AlacEvent::Wakeup => Some(TermEvent::Wakeup),
            AlacEvent::Bell => Some(TermEvent::Bell),
            AlacEvent::ChildExit(status) => Some(TermEvent::ChildExit(status.code())),
            AlacEvent::Exit => Some(TermEvent::ChildExit(None)),
            _ => None,
        };
        if let Some(ev) = ev {
            let _ = self.0.send(ev);
        }
    }
}

/// 一个交互式终端实例：持有终端状态机 + PTY 事件循环。
pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    event_rx: std::sync::mpsc::Receiver<TermEvent>,
    /// 手动触发 Wakeup 的发送端（滚动改变 display_offset 时 alacritty 不发 Wakeup，需手动补发）。
    wakeup_tx: std::sync::mpsc::Sender<TermEvent>,
}

impl Terminal {
    /// 启动一个 shell。
    ///
    /// - `shell`：shell 程序名（如 `powershell` / `cmd`）。
    /// - `cwd`：初始工作目录（None=继承当前进程目录）。
    /// - `columns` / `lines`：初始字符网格尺寸。
    pub fn new(
        shell: &str,
        cwd: Option<PathBuf>,
        columns: usize,
        lines: usize,
    ) -> Result<Self, String> {
        let size = TerminalSize { columns, lines };
        let window_size = WindowSize {
            num_lines: lines as u16,
            num_cols: columns as u16,
            cell_width: 8,
            cell_height: 16,
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let wakeup_tx = tx.clone();

        let options = tty::Options {
            shell: Some(tty::Shell::new(shell.to_string(), Vec::new())),
            working_directory: cwd,
            drain_on_exit: true,
            env: HashMap::new(),
            #[cfg(windows)]
            escape_args: false,
        };

        let pty = tty::new(&options, window_size, 0).map_err(|e| e.to_string())?;

        let config = Config {
            scrolling_history: 10000,
            ..Config::default()
        };
        let term = Term::new(config, &size, Listener(tx.clone()));
        let term = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(term.clone(), Listener(tx), pty, true, false)
            .map_err(|e| e.to_string())?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        Ok(Terminal {
            term,
            notifier,
            event_rx: rx,
            wakeup_tx,
        })
    }

    /// 向 shell 写入字节（键盘输入 / 粘贴）。
    pub fn write_input(&self, bytes: &[u8]) {
        self.notifier.notify(bytes.to_vec());
    }

    /// 滚动显示（查看历史输出）。
    ///
    /// `delta_lines`：正数向上滚（看更早历史），负数向下滚（回最新）。
    /// alacritty 滚动后不发 Wakeup，这里手动补发一个以触发界面重绘。
    pub fn scroll(&self, delta_lines: i32) {
        {
            let mut term = self.term.lock();
            term.scroll_display(Scroll::Delta(delta_lines));
        }
        let _ = self.wakeup_tx.send(TermEvent::Wakeup);
    }

    /// 调整终端字符网格尺寸（同时调整 Term 网格与 PTY 窗口）。
    pub fn resize(&self, columns: usize, lines: usize) {
        // 1. 调整 Term 的字符网格
        {
            let mut term = self.term.lock();
            term.resize(TerminalSize { columns, lines });
        }
        // 2. 调整 PTY 窗口尺寸（通知子进程）
        let ws = WindowSize {
            num_lines: lines as u16,
            num_cols: columns as u16,
            cell_width: 8,
            cell_height: 16,
        };
        let _ = self.notifier.0.send(Msg::Resize(ws));
    }

    /// 关闭终端（发送 Shutdown，结束事件循环与子进程）。
    /// 主动收掉这个终端。
    ///
    /// 目前**没有调用方**：gpui 这个 rev 没有可靠的「窗口即将关闭」钩子，
    /// 而 `Rc<Terminal>` 一落地，alacritty 的 EventLoop 会跟着收掉 pty。
    /// 留着是因为它属于这个封装该有的能力（真出问题时得能手动关），
    /// 不是「写了没用」的代码 —— 但也不许再悄悄进来第二个这样的。
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }

    /// 取出所有待处理事件（非阻塞）。
    pub fn drain_events(&self) -> Vec<TermEvent> {
        let mut events = Vec::new();
        while let Ok(e) = self.event_rx.try_recv() {
            events.push(e);
        }
        events
    }

    /// 在锁内遍历可见内容，回调结束后释放锁。
    ///
    /// 渲染数据（字符网格）通过闭包取出，避免在锁外持有借用。
    pub fn render(&self, f: impl FnOnce(RenderableContent)) {
        let term = self.term.lock();
        let content = term.renderable_content();
        f(content);
    }

    // ---- 鼠标选择 ----

    /// 开始一次鼠标选择（坐标为视口内 0-based 行 / 列）。
    pub fn start_selection(&self, viewport_line: i32, column: usize) {
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(viewport_line - offset), Column(column));
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// 拖拽中更新选择终点（坐标为视口内 0-based 行 / 列）。
    pub fn update_selection(&self, viewport_line: i32, column: usize) {
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(viewport_line - offset), Column(column));
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, Side::Right);
        }
    }

    /// 清空选择。
    pub fn clear_selection(&self) {
        let mut term = self.term.lock();
        term.selection = None;
    }

    /// 取当前选中的文本（无选择或空选择返回 None）。
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock();
        term.selection_to_string().filter(|s| !s.is_empty())
    }
}
