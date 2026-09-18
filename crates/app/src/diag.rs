//! 诊断输出与崩溃留痕。
//!
//! # 为什么不是 `println!`
//!
//! 发布版是 **GUI 子系统**（`main.rs` 顶部的 `windows_subsystem`）——
//! 没有控制台，`println!` 出来的字一个地方都看不到。而这个应用里有
//! 「首帧多久」「首次排版多久」「取包多久」「为什么排版失败」这类
//! 只有真跑起来才知道的数字，丢掉它们等于每次排查都从零开始。
//!
//! 所以：**所有诊断都同时落进日志文件**（`%APPDATA%\typst-live\typst-live.log`，
//! 与 `settings.conf` 同目录），debug 构建额外打到 stdout（开发时在终端里照样看得见）。
//!
//! # 崩溃也要留痕
//!
//! `install_panic_hook` 在进程启动时装一个 panic 钩子，把「在哪崩的、崩在什么消息上」
//! 追加进同一个日志。GUI 子系统下默认钩子的输出没人接得住 —— 没有这个文件，
//! 用户报「它自己没了」时手上一点线索都没有。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// 「这里发生的 panic 是**预期内**的」标志。
///
/// `safe_update` 在 `catch_unwind` 期间把它置位：那条路径上的 `already borrowed`
/// 是 gpui 在 Windows 上的已知竞态（可重试，见 `safe_update` 模块说明），
/// 不该被记成崩溃 —— 否则日志里全是假警报，真崩的那条反而没人看。
pub(crate) static SUPPRESS_BORROW_PANIC: AtomicBool = AtomicBool::new(false);

/// 日志文件路径。
///
/// 与设置文件同目录：一个应用的东西放一起，`%APPDATA%\typst-live\` 下就两份。
fn log_path() -> PathBuf {
    crate::settings::path().with_file_name("typst-live.log")
}

/// 追加一行诊断。**永远不返回错误** —— 日志写不进去不该影响应用运行。
pub(crate) fn log_line(line: &str) {
    #[cfg(debug_assertions)]
    println!("{line}");

    append_log(line);
}

/// 只进文件，不碰 stdout（panic 钩子用：那时候 stdout 已经不可信了）。
pub(crate) fn append_log(line: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// 装 panic 钩子：崩溃必须留下「在哪、为什么」。
///
/// 借用竞态期间（`SUPPRESS_BORROW_PANIC`）直接返回 —— 那种 panic 会被
/// `safe_update` 捕获并重试，写进日志只会淹掉真问题。
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        if SUPPRESS_BORROW_PANIC.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }

        let site = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "位置未知".to_string());
        append_log(&format!(
            "[panic] {site} —— {}",
            payload_text(info.payload())
        ));

        // 原来那个钩子照旧跑：debug 下终端里还是原来那条 backtrace。
        previous(info);
    }));
}

/// panic 载荷 → 人话（`panic!("…")` 是 `&str`，`format!` 出来的是 `String`）。
fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    "（非字符串载荷）".to_string()
}
