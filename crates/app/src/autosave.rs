//! 自动保存：**什么时候该往磁盘上写**。
//!
//! 就一条规矩，但仍然单独成模块而不是塞进 `main.rs`：判错的后果是**丢数据**
//! （该写没写）或者**动用户的文件**（不该写却写了），所以它得是个纯函数、
//! 三种「不该写」各有单测钉住。见 `docs/NEXT.md` 的 L24。
//!
//! 触发时机是**空闲防抖**：打字停下来之后由计时器问这里一次
//! （见 `Previewer::touch_autosave`）。「用户还在打字吗」不在这里判 ——
//! 那是计时器的事，这里只看「此刻该不该写」。

use std::time::Duration;

/// 打字停下来多久算「停下来了」。
///
/// 比设置项的 `SETTINGS_DEBOUNCE`（400 ms）长：那个写的是几十行的配置文件，
/// 写坏了也无所谓；这个写的是用户正文，宁可比手指慢一点。
///
/// 也刻意不更长：写盘是**同步**的（`std::fs::write`，见 `write_to_disk`），
/// 主线程上几十 KB 是微秒级；但如果哪天文档到了 MB 级，这个数字要跟着复核 ——
/// 那时该改成后台线程写，而不是把间隔拉长（拉长只是把掉帧变成「更容易丢一秒」）。
pub const DELAY: Duration = Duration::from_secs(1);

/// 该写盘吗？
///
/// 三条守卫**缺一不可**：
///
/// - `autosave`：用户关掉了就别写（`Ctrl+Alt+S` 或状态栏那一格）。
/// - `explicit_file`：内置示例文档的路径在临时目录里，它从来不是用户的文件，
///   自动往那儿写等于凭空造出一个文件来。这条**与开关无关**，永远生效。
/// - `changed`：文本与磁盘上那一份真的不一样才写。判据是**文本比较**，
///   不是「敲过键」：打了字又撤回去（Ctrl+Z）不该白写一次盘。
pub fn should_save(autosave: bool, explicit_file: bool, changed: bool) -> bool {
    autosave && explicit_file && changed
}

/// 状态栏那一格写什么。
///
/// 四个状态要能分清，否则自动保存就是个「看不见的功能」：「关了且改了」
/// 与「开着但还在等一下」都写「● 未保存」的话，用户没法从界面上看出到底开没开。
/// 放在这个模块（而不是 `ui/chrome.rs`）是因为它说的就是自动保存的状态，
/// 而且**这么放能测** —— 它是个纯函数，四行各一个断言。
pub fn status_label(dirty: bool, autosave: bool) -> &'static str {
    match (dirty, autosave) {
        (true, true) => "● 待自动保存",
        (true, false) => "● 未保存",
        (false, true) => "已自动保存",
        (false, false) => "已保存",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_guards_open_means_write() {
        assert!(should_save(true, true, true));
    }

    #[test]
    fn switched_off_never_writes() {
        assert!(!should_save(false, true, true));
    }

    /// 内置示例文档：就算用户开着自动保存、也确实改了字，也不许写 ——
    /// 它的路径是 `%TEMP%\typst-live-demo.typ`，不是用户的东西。
    #[test]
    fn the_built_in_demo_is_never_written() {
        assert!(!should_save(true, false, true));
    }

    /// 文本与磁盘上那份一样（打字又撤回去了）：没必要写。
    #[test]
    fn unchanged_text_is_not_written_again() {
        assert!(!should_save(true, true, false));
    }

    /// 状态栏四态各一个，一个都不能撞名 —— 撞名就等于「看不出开关状态」。
    #[test]
    fn the_four_status_labels_are_distinct() {
        let all = [
            status_label(true, true),
            status_label(true, false),
            status_label(false, true),
            status_label(false, false),
        ];
        assert_eq!(status_label(true, true), "● 待自动保存");
        assert_eq!(status_label(true, false), "● 未保存");
        assert_eq!(status_label(false, true), "已自动保存");
        assert_eq!(status_label(false, false), "已保存");

        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "两种状态不该写同一句话：{a}");
            }
        }
    }
}
