//! 页面坐标换算：窗口像素 ⇄ 页内 pt。
//!
//! 为什么值得单独一个文件：这是全项目里**唯一一处容易写错、而且错了看不出来**
//! 的算术。
//!
//! 两条容易踩的事实：
//!
//! 1. `ScrollHandle::bounds_for_item(i)` 给的是**内容坐标**，不是窗口坐标 ——
//!    `ScrollHandle::top_item` 用的就是同一套。要**加回滚动偏移**才是
//!    「这一页现在画在窗口的哪里」。
//! 2. 100% 缩放 = 96 dpi，而 typst 的点是 1/72 英寸 —— 所以 1pt = `96/72`
//!    个逻辑像素（`pixel_per_pt_for_zoom`）。
//!
//! 把偏移漏掉时，**预览没滚动的话一切正常**，一滚过就整页偏。第一版正是
//! 这么错的，靠临时自检里一条独立算式才揪出来。所以这里是纯函数 + 单测：
//! 让这个错不可能再犯回去。

use typst_engine::export::pixel_per_pt_for_zoom;

/// 逻辑像素坐标 `(x, y)`。
pub type Px = (f32, f32);

/// 页内 pt 坐标 `(x, y)`：页左上角为原点，y 向下。
pub type PagePt = (f32, f32);

/// 窗口坐标 → 页内 pt。
///
/// - `position`：鼠标事件的窗口坐标
/// - `page_origin`：`bounds_for_item(page).origin`（内容坐标）
/// - `scroll`：`scroll.offset()`（向右下滚时为负）
pub fn page_pt_from_window(position: Px, page_origin: Px, scroll: Px, zoom: f32) -> PagePt {
    let base = pixel_per_pt_for_zoom(zoom);
    (
        (position.0 - page_origin.0 - scroll.0) / base,
        (position.1 - page_origin.1 - scroll.1) / base,
    )
}

/// 页内 pt → 窗口坐标（[`page_pt_from_window`] 的反函数）。
pub fn window_from_page_pt(pt: PagePt, page_origin: Px, scroll: Px, zoom: f32) -> Px {
    let base = pixel_per_pt_for_zoom(zoom);
    (
        page_origin.0 + pt.0 * base + scroll.0,
        page_origin.1 + pt.1 * base + scroll.1,
    )
}

#[cfg(test)]
mod tests {
    use typst_engine::export::BASE_PIXEL_PER_PT;

    use super::*;

    /// 某页左上角在内容坐标里的位置，随便取一个非零值。
    const ORIGIN: Px = (100.0, 50.0);
    const NO_SCROLL: Px = (0.0, 0.0);

    #[test]
    fn without_scrolling_it_is_just_an_origin_offset() {
        let at = window_from_page_pt((10.0, 20.0), ORIGIN, NO_SCROLL, 1.0);

        assert!((at.0 - (100.0 + 10.0 * BASE_PIXEL_PER_PT)).abs() < 0.01);
        assert!((at.1 - (50.0 + 20.0 * BASE_PIXEL_PER_PT)).abs() < 0.01);
    }

    /// ★ 这条就是那个 bug 的回归测试。
    ///
    /// 预览已经滚到很下面时，`bounds_for_item` 给的还是内容坐标：
    /// 漏掉 `scroll` 的话，双击位置会偏出**整页**（这里量级是 900pt）。
    #[test]
    fn a_scrolled_preview_needs_the_offset_added_back() {
        let scroll = (0.0, -1200.0);
        let target: PagePt = (10.0, 20.0);

        // 正算：目标现在画在窗口的哪里
        let at = window_from_page_pt(target, ORIGIN, scroll, 1.0);
        assert_eq!(at.1, 50.0 + 20.0 * BASE_PIXEL_PER_PT - 1200.0);

        // 反算必须回到原处（f32 往返，比到 0.01pt 就够）
        let back = page_pt_from_window(at, ORIGIN, scroll, 1.0);
        assert!(
            (back.0 - target.0).abs() < 0.01 && (back.1 - target.1).abs() < 0.01,
            "反算偏了：{target:?} → {at:?} → {back:?}"
        );

        // 如果忘了加偏移，会偏多少 —— 明确写出来，免得以后有人「顺手简化」
        let wrong = page_pt_from_window(at, ORIGIN, NO_SCROLL, 1.0);
        let off_by = (wrong.1 - target.1).abs();
        assert!(
            off_by > 800.0,
            "漏掉滚动偏移该偏出整页（实测偏 {off_by:.0}pt），实际只偏了一点 —— 说明测试本身写错了"
        );
    }

    #[test]
    fn zoom_scales_pixels_but_not_the_page_pt() {
        for zoom in [0.25_f32, 1.0, 2.0, 4.0] {
            let target: PagePt = (30.0, 40.0);
            let at = window_from_page_pt(target, ORIGIN, NO_SCROLL, zoom);
            let back = page_pt_from_window(at, ORIGIN, NO_SCROLL, zoom);

            assert!((back.0 - target.0).abs() < 0.001, "zoom={zoom} 往返偏了");
            assert!((back.1 - target.1).abs() < 0.001, "zoom={zoom} 往返偏了");

            // 缩放只改像素，不改 pt
            assert!(
                (at.1 - ORIGIN.1 - 40.0 * pixel_per_pt_for_zoom(zoom)).abs() < 0.001,
                "zoom={zoom} 像素尺度不对"
            );
        }
    }

    #[test]
    fn the_two_directions_are_inverses_in_general() {
        let scrolls = [NO_SCROLL, (0.0, -1200.0), (-350.0, -40.5)];
        let zooms = [0.25_f32, 1.0, 2.5];

        for scroll in scrolls {
            for zoom in zooms {
                for pt in [(0.0_f32, 0.0_f32), (123.5, 456.25), (-3.0, -8.0)] {
                    let at = window_from_page_pt(pt, ORIGIN, scroll, zoom);
                    let back = page_pt_from_window(at, ORIGIN, scroll, zoom);

                    assert!(
                        (back.0 - pt.0).abs() < 0.01 && (back.1 - pt.1).abs() < 0.01,
                        "scroll={scroll:?} zoom={zoom} pt={pt:?} → {at:?} → {back:?}"
                    );
                }
            }
        }
    }
}
