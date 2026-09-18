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
    term::{Config, RenderableContent, Term, TermMode},
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

    /// 终端是不是处于「括号粘贴」模式（shell/编辑器发的 `\e[?2004h`）。
    ///
    /// 开着的时候粘贴要包一层 `\e[200~ … \e[201~`：不然多行文本会被
    /// shell 逐行当成「回车」执行（粘一段命令进去就变成连跑好几条）。
    pub fn bracketed_paste(&self) -> bool {
        self.term.lock().mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// 把一段文本粘贴进终端。
    pub fn paste(&self, text: &str) {
        self.write_input(&paste_bytes(text, self.bracketed_paste()));
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
    ///
    /// **关窗时必须调它**（见 `Previewer::shutdown_terminal`）：`Msg::Shutdown` 会让
    /// alacritty 的 `EventLoop` 从 `process_events` 返回 false 而退出，它持有的
    /// ConPTY 随之析构 —— 关掉伪控制台，附着在上面的 PowerShell 才会结束。
    /// 不调的话，子进程就只能指望「进程退出」帮忙收尸（崩溃/被强杀时连这个都没有）。
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

/// 粘贴文本 → 送进 PTY 的字节。
///
/// 抽成自由函数是为了**能测** —— 两个坑都在这里：
///
/// 1. **换行必须是 `\r`**：终端里的「回车」是 CR。只送 `\n` 到 PowerShell 提示符上
///    只是把光标移到下一行、**不执行** —— 用起来就是「粘贴没反应」。
/// 2. **括号粘贴模式**（shell/编辑器发的 `\e[?2004h`）开着时要包一层
///    `\e[200~ … \e[201~`，否则粘多行会被逐行当成回车执行。
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{normalized}\x1b[201~").into_bytes()
    } else {
        normalized.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::paste_bytes;

    /// LF 与 CRLF 都要变成 CR —— 不然 PowerShell 提示符上「粘了不执行」。
    #[test]
    fn paste_normalizes_newlines_to_cr() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
        assert_eq!(paste_bytes("a\r\nb", false), b"a\rb".to_vec());
        assert_eq!(
            paste_bytes("echo hi\necho bye", false),
            b"echo hi\recho bye".to_vec()
        );
    }

    /// 括号粘贴模式：多行要包起来，shell 才不会逐行执行。
    #[test]
    fn paste_wraps_when_bracketed() {
        assert_eq!(
            paste_bytes("a\nb", true),
            b"\x1b[200~a\rb\x1b[201~".to_vec()
        );
    }

    /// 普通单行粘贴：原样，一个字节都不多。
    #[test]
    fn paste_keeps_plain_text_intact() {
        assert_eq!(paste_bytes("git status", false), b"git status".to_vec());
    }
}
