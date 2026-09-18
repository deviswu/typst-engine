//! 预览的**出图与视口**：光栅化、纹理的按视口装卸、缩放与翻页。
//!
//! 与排版分开：缩放只重做这一层（状态栏上「排版 N 次 / 光栅化 M 次」两个
//! 计数就是这条分界线的证据）。

use crate::*;

impl Previewer {
    /// 从已排版的 `doc` 重新出图。**不碰编译器**。
    /// 排版或缩放变了 —— 重建页尺寸并丢掉全部纹理，等下一帧按视口重新出图。
    ///
    /// **不再一次性光栅化全部页**。原因：A4 单页在 100% 下就占 7.7 MiB 纹理，
    /// 100 页就是 770 MiB；400% 时每页 54 MiB，直接爆。现在只出可见的几页。
    pub(crate) fn rerasterize(&mut self) {
        let Some(doc) = self.doc.clone() else {
            self.bitmaps.clear();
            self.page_sizes.clear();
            self.texture_bytes = 0;
            self.live_range = None;
            return;
        };

        // 页的**逻辑**尺寸（布局用）。它等于 pt × 96/72 × zoom ——
        // 屏幕缩放系数在这里恰好抵消，因为逻辑尺寸本就不该依赖显示器。
        let base = typst_engine::export::BASE_PIXEL_PER_PT * self.zoom;
        self.page_sizes = doc
            .pages()
            .iter()
            .map(|p| {
                let s = p.frame.size();
                (s.x.to_pt() as f32 * base, s.y.to_pt() as f32 * base)
            })
            .collect();

        // 全部丢成「未光栅化」占位。布局靠 page_sizes，不靠纹理，
        // 所以占位不会让页面尺寸变化 —— 滚到底、翻页、跳大纲都不受影响。
        self.bitmaps = vec![None; self.page_sizes.len()];
        self.texture_bytes = 0;
        self.live_range = None;
    }
    /// 按当前视口把该出图的页出了，把不该留的丢掉。
    ///
    /// 只在**可见页范围真的变了**的时候动手 —— 滚动过程中每帧都重算
    /// 会让滚动发涩。
    pub(crate) fn sync_visible_pages(&mut self) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let count = doc.pages().len();
        if count == 0 || self.bitmaps.len() != count {
            return;
        }

        // `top_item`/`bottom_item` 是 gpui 给的「当前滚进视口的子项下标」——
        // 我们的子项恰好就是页。还没布局时它们安全地返回 0。
        let first = self.scroll.top_item().min(count - 1);
        let last = self.scroll.bottom_item().min(count - 1);
        let lo = first.saturating_sub(PAGE_PREFETCH);
        let hi = (last + PAGE_PREFETCH).min(count - 1);

        self.current_page = first;
        if self.live_range == Some((lo, hi)) {
            return;
        }
        self.live_range = Some((lo, hi));

        let ppp = pixel_per_pt_for_zoom(self.zoom) * self.scale_factor;
        let t = Instant::now();
        let mut rasterized = 0usize;
        let mut dropped = 0usize;

        // ① 卸载：范围外的纹理丢掉（这是显存真正被释放的地方）
        for (i, slot) in self.bitmaps.iter_mut().enumerate() {
            if (i < lo || i > hi) && slot.is_some() {
                *slot = None;
                dropped += 1;
            }
        }

        // ② 补缺：范围内还没出图的页
        for i in lo..=hi {
            if self.bitmaps[i].is_some() {
                continue;
            }
            let texture = to_texture(rasterize_page(&doc.pages()[i], ppp));
            self.bitmaps[i] = texture;
            rasterized += 1;
        }

        if rasterized > 0 || dropped > 0 {
            self.status.raster_ms = t.elapsed().as_secs_f64() * 1000.0;
            self.status.rasters += 1;
        }
        self.texture_bytes = self
            .bitmaps
            .iter()
            .flatten()
            .map(|b| {
                let s = b.size(0);
                s.width.0 as usize * s.height.0 as usize * 4
            })
            .sum();

        let live = self.bitmaps.iter().filter(|b| b.is_some()).count();
        logln!(
            "[typst-live] 视口 {}–{} 页 / 共 {count}：新出图 {rasterized}，卸载 {dropped}；\
             常驻纹理 {live} 页 {:.1} MiB，用时 {:.1} ms",
            lo + 1,
            hi + 1,
            self.texture_bytes as f64 / (1024.0 * 1024.0),
            self.status.raster_ms,
        );
    }
    /// 预览「适应宽度」：让页宽 = 展示区宽 - 两侧边距。
    ///
    /// 只要开着 `zoom_fit`，每次渲染都按当前展示区宽度重算缩放 —— 所以
    /// **拖动分区、改窗口大小，页面都会跟着充满**（这正是用户要的）。
    /// 手动缩放（Ctrl+=/-）会关掉它，Ctrl+0 再回来。
    ///
    /// 为什么在 `render` 里做：展示区宽度只有布局完才知道，用上一帧的
    /// `ScrollHandle::bounds()` 就够了（差一帧，肉眼看不出来）。
    pub(crate) fn fit_zoom_to_viewport(&mut self) {
        if !self.zoom_fit {
            return;
        }
        let Some(page) = self.doc.as_ref().and_then(|doc| doc.pages().first()) else {
            return;
        };
        let width = self.scroll.bounds().size.width.as_f32();
        if width <= 1.0 {
            return; // 还没布局
        }

        let page_pt = page.frame.size().x.to_pt() as f32;
        let usable = (width - PREVIEW_MARGIN * 2.0).max(64.0);
        let fit = usable / (page_pt * typst_engine::export::BASE_PIXEL_PER_PT);

        // 只在真的变了才重建纹理（拖动时每帧都会算，但相等就不动手）
        let fit = fit.clamp(ZOOM_MIN, ZOOM_MAX);
        if (fit - self.zoom).abs() > 0.005 {
            logln!(
                "[typst-live] 适应宽度：展示区 {width:.0}px - 边距 {:.0}px → 缩放 {fit:.3}（页宽 {page_pt:.1}pt）",
                PREVIEW_MARGIN * 2.0
            );
            self.zoom = fit;
            self.rerasterize();
        }
    }
    /// 开机把布局数字报一次（**永久保留的诊断**）。
    ///
    /// 起因：「编辑区太窄」那次，分区面板是 875px 而内容只有 117px ——
    /// 面板只给位置与尺寸，内容不加 `w_full()` 就缩成内容宽。
    /// 有了这行数字，下次同类问题一眼就能定位是谁占走了宽度。
    pub(crate) fn report_layout_once(&mut self, window: &Window, cx: &Context<Self>) {
        if self.layout_reported {
            return;
        }
        let sizes = self.split_state.read(cx).sizes().clone();
        if sizes.len() < 3 {
            return; // 还没布局出来
        }
        self.layout_reported = true;

        let nums: Vec<String> = sizes.iter().map(|p| format!("{:.0}", p.as_f32())).collect();
        logln!(
            "[typst-live] 布局：窗口 {:.0}px → 侧栏 {} / 编辑 {} / 展示 {}",
            window.bounds().size.width.as_f32(),
            nums[0],
            nums[1],
            nums[2]
        );
    }
    /// 用户手动缩放（Ctrl+= / Ctrl+- / 滚轮）：退出「适应宽度」。
    pub(crate) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom_fit = false;
        self.touch_settings(cx);
        self.apply_zoom(zoom, cx);
    }
    /// 回到「适应宽度」（Ctrl+0）。
    pub(crate) fn fit_zoom(&mut self, cx: &mut Context<Self>) {
        self.zoom_fit = true;
        self.fit_zoom_to_viewport();
        self.touch_settings(cx);
        cx.notify();
    }
    /// 真正改缩放：不动「手动/适应」开关，也不写设置（拖动分区时会频繁触发）。
    pub(crate) fn apply_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;
        // 只重做出图，不重新排版 —— 这就是缩放能任意清晰的原因。
        self.rerasterize();
        // 缩放是要记住的（下次开窗应该还是这个大小）
        self.touch_settings(cx);
        cx.notify();
    }
    /// 缩放并把当前页重新锚回预览顶部。
    pub(crate) fn set_zoom_anchored(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let before = self.zoom;
        self.set_zoom(zoom, cx);
        if (self.zoom - before).abs() > f32::EPSILON {
            self.scroll.scroll_to_top_of_item(self.current_page);
        }
    }
    /// 把第 `page` 页（0 起）滚到预览顶部。越界会被夹到合法范围。
    pub(crate) fn go_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.bitmaps.is_empty() {
            return;
        }
        let page = page.min(self.bitmaps.len() - 1);
        self.current_page = page;
        self.scroll.scroll_to_top_of_item(page);
        self.message = Some(format!("第 {} / {} 页", page + 1, self.bitmaps.len()));
        cx.notify();
    }
}

/// 引擎的 `RasterPage` → GPUI 的 GPU 纹理。
///
/// 这里**同步**构造 `RenderImage`，刻意不走 `Image::from_bytes` 那条路 ——
/// 后者要过 asset 系统做异步解码（`use_asset`），第一次必然拿不到结果。
/// 而且 `Image::from_bytes` 的缓存键是内容哈希，同一份内容永远复用同一张
/// 纹理，缩放就拿不到更高分辨率。自己构造就没有这两个问题。
pub(crate) fn to_texture(page: RasterPage) -> Option<Arc<RenderImage>> {
    let (width, height) = (page.width, page.height);
    let mut rgba = page.rgba;

    // tiny-skia 给的是 RGBA（alpha 已预乘），GPU 纹理要 BGRA。
    for pixel in rgba.as_chunks_mut::<4>().0 {
        swap_rgba_pa_to_bgra(pixel);
    }

    let buffer: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(width, height, rgba)?;

    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}
