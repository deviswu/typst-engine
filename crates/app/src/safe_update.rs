//! 在**常驻任务**里更新视图时的借用竞态兜底。
//!
//! # 为什么需要这个
//!
//! gpui 的 Windows 后端会在**外层已经持有 `App` 借用**的情况下，把「就绪的前台任务」
//! 派发到主线程执行（`WindowsDispatcher` → `handle_gpui_events` → `handle_msg`）。
//! 只要某个任务的 `weak.update(...)` 正好在这一刻被 poll，就会撞上
//! `RefCell already borrowed` —— 而 panic 发生在窗口过程（wndproc）里，展开不了，
//! 进程直接 fastfail 退出（0xc0000409）。中文输入法会开嵌套消息循环，命中概率极高，
//! 于是症状是「一打字就没了」。
//!
//! 这是平台竞态，不是我们代码里的逻辑错误：**同一次更新重试一下就好了**。
//! 于是所有「任务里更新实体」的地方都走这里：
//!
//! - `Busy` → 调用方等下一拍重试（循环里的 `continue`、延时任务里的重新计时）
//! - `Gone` → 实体已释放，循环该退出了
//! - 其它 panic **原样抛出**（不掩盖真 bug）

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;

use gpui::{App, AppContext, Context, Entity, WeakEntity};

use crate::diag::SUPPRESS_BORROW_PANIC;

/// 一次「任务里更新视图」的结果。
pub(crate) enum UpdateOutcome<T> {
    /// 更新跑完了。
    Done(T),
    /// 撞上借用竞态：**等一会儿重试**，不是错误。
    Busy,
    /// 实体已经释放：循环/任务该收工了。
    Gone,
}

/// 更新弱引用指向的实体，把「借用竞态」变成 `Busy` 而不是崩溃。
pub(crate) fn safe_task_update<T: 'static, C: AppContext, R>(
    weak: &WeakEntity<T>,
    cx: &mut C,
    f: impl FnOnce(&mut T, &mut Context<T>) -> R,
) -> UpdateOutcome<R> {
    SUPPRESS_BORROW_PANIC.store(true, Ordering::SeqCst);
    let caught = std::panic::catch_unwind(AssertUnwindSafe(|| weak.update(cx, f)));
    SUPPRESS_BORROW_PANIC.store(false, Ordering::SeqCst);

    match caught {
        Ok(Ok(value)) => UpdateOutcome::Done(value),
        Ok(Err(_)) => UpdateOutcome::Gone,
        Err(payload) => {
            if is_borrow_conflict(payload.as_ref()) {
                UpdateOutcome::Busy
            } else {
                // 不是竞态就是真 bug：原样抛，别在这里吞掉。
                std::panic::resume_unwind(payload)
            }
        }
    }
}

/// 读一眼弱引用指向的实体（不改它）。
///
/// 与 [`safe_task_update`] 同一件事：常驻轮询任务每一拍都要先问一句状态
/// （「现在开着哪个文件」「本地有改动吗」），而直接 `read_with` 撞上那个平台竞态
/// 同样是 0xc0000409 —— 读取也得走这条路。
pub(crate) fn safe_task_read<T: 'static, C: AppContext, R>(
    weak: &WeakEntity<T>,
    cx: &mut C,
    f: impl FnOnce(&T, &App) -> R,
) -> UpdateOutcome<R> {
    SUPPRESS_BORROW_PANIC.store(true, Ordering::SeqCst);
    let caught = std::panic::catch_unwind(AssertUnwindSafe(|| weak.read_with(cx, f)));
    SUPPRESS_BORROW_PANIC.store(false, Ordering::SeqCst);

    match caught {
        Ok(Ok(value)) => UpdateOutcome::Done(value),
        Ok(Err(_)) => UpdateOutcome::Gone,
        Err(payload) => {
            if is_borrow_conflict(payload.as_ref()) {
                UpdateOutcome::Busy
            } else {
                std::panic::resume_unwind(payload)
            }
        }
    }
}

/// 强引用版（`Entity` 也能直接喂进来）。
pub(crate) fn safe_entity_update<T: 'static, C: AppContext, R>(
    entity: &Entity<T>,
    cx: &mut C,
    f: impl FnOnce(&mut T, &mut Context<T>) -> R,
) -> UpdateOutcome<R> {
    safe_task_update(&entity.downgrade(), cx, f)
}

/// 这条 panic 载荷是不是「借用冲突」。
///
/// **只认这一句**：`already borrowed` 是 gpui/`RefCell` 的竞态措辞。
/// 别放宽成「包含 borrow 就算」—— 真 bug 也会提到 borrow，那就会被一起吞掉。
fn is_borrow_conflict(payload: &(dyn std::any::Any + Send)) -> bool {
    payload
        .downcast_ref::<String>()
        .is_some_and(|text| text.contains("already borrowed"))
        || payload
            .downcast_ref::<&str>()
            .is_some_and(|text| text.contains("already borrowed"))
}
