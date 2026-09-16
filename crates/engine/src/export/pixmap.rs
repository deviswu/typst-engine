//! 位图导出。
//!
//! 与 [`super::svg`] 的分工：
//! - **SVG** 适合导出与将来的页内 diff 推送（矢量、体积小、可字符串比较）
//! - **位图** 适合屏幕预览（跳过 SVG 解析，且 **DPI 完全由我们决定**）
//!
//! 预览为什么改用位图 —— 实测一页 15cm 的文档：SVG 字符串 493 KiB，
//! 而 gpui 光栅化 SVG 时固定按自然尺寸 ×2（`SMOOTH_SVG_SCALE_FACTOR`），
//! 且 `Image::from_bytes` 的纹理缓存键是**内容哈希**：同一份 SVG 永远复用
//! 同一张纹理，改显示尺寸拿不到更高分辨率。用 `typst-render` 自己出图，
//! 缩放时按需重新光栅化，分辨率与显存完全可控。

use typst::utils::Scalar;
use typst_layout::{Page, PagedDocument};
use typst_render::RenderOptions;

/// 一页的光栅结果。
///
/// `rgba` 是 **RGBA 且 alpha 已预乘**（tiny-skia 的原生格式）。要当 GPU
/// 纹理用（BGRA）还得换序，那是上层的事 —— 本 crate 不依赖任何 UI 框架。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RasterPage {
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// 期望的字节数（宽 × 高 × 4）。
    pub fn byte_len(&self) -> usize {
        self.pixel_count() * 4
    }

    /// 数据长度是否与尺寸自洽。
    pub fn is_consistent(&self) -> bool {
        self.rgba.len() == self.byte_len()
    }
}

/// 缩放 100% 对应的 DPI —— 96 是 CSS 标准像素的定义值。
pub const BASE_DPI: f32 = 96.0;

/// 一英寸 = 72 点（Typst 的点是 1/72 英寸）。
pub const PT_PER_INCH: f32 = 72.0;

/// 缩放 100% 时，每点对应多少像素。
pub const BASE_PIXEL_PER_PT: f32 = BASE_DPI / PT_PER_INCH;

/// 缩放倍率 → 每点像素数。
///
/// 1.0 倍 = 屏幕上「实际大小」（约等于 96 dpi 的物理尺寸）。
pub fn pixel_per_pt_for_zoom(zoom: f32) -> f32 {
    BASE_PIXEL_PER_PT * zoom
}

/// 把一页光栅化。
pub fn rasterize_page(page: &Page, pixel_per_pt: f32) -> RasterPage {
    // 刻意不写 `tiny_skia::Pixmap` 这个类型名 —— 那会迫使我们直接依赖
    // `tiny-skia` 并把它与 `typst-render` 的版本锁死。让编译器推导就好。
    let pixmap = typst_render::render(page, &render_options(pixel_per_pt));
    RasterPage {
        width: pixmap.width(),
        height: pixmap.height(),
        rgba: pixmap.take(),
    }
}

/// 把每一页光栅化，顺序与 `pages()` 一致。
pub fn rasterize(document: &PagedDocument, pixel_per_pt: f32) -> Vec<RasterPage> {
    document
        .pages()
        .iter()
        .map(|page| rasterize_page(page, pixel_per_pt))
        .collect()
}

fn render_options(pixel_per_pt: f32) -> RenderOptions {
    // `Scalar::new` 把 NaN 归零。0 或负数会导致 pixmap 尺寸为 0 而失败，
    // 所以在入口兜住下界 —— 调用方传了荒谬的缩放也不该炸掉编译流程。
    let ppp = pixel_per_pt.max(MIN_PIXEL_PER_PT) as f64;
    RenderOptions {
        pixel_per_pt: Scalar::new(ppp),
        render_bleed: false,
    }
}

/// 每点最少 1/16 像素。够小以保证不误伤，够大以保证 pixmap 建得出来。
const MIN_PIXEL_PER_PT: f32 = 0.0625;

#[cfg(test)]
mod tests {
    use crate::world::{EngineWorld, EntryState, embedded_and_system_fonts};

    use super::*;

    fn compile(source: &str) -> PagedDocument {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("main.typ");
        std::fs::write(&main, source).expect("write");

        let world = EngineWorld::new(
            embedded_and_system_fonts(),
            EntryState::new(dir.path(), &main),
        );

        typst::compile::<PagedDocument>(&world)
            .output
            .unwrap_or_else(|e| panic!("编译失败：{e:?}"))
    }

    /// 固定 100×200 点、黑字白底的页面，好让尺寸与内容都可断言。
    fn fixed_page() -> &'static str {
        "#set page(width: 100pt, height: 200pt, margin: 5pt)\n\nHello\n"
    }

    #[test]
    fn zoom_maps_to_pixel_per_pt() {
        assert!((pixel_per_pt_for_zoom(1.0) - 4.0 / 3.0).abs() < 1e-6);
        assert!((pixel_per_pt_for_zoom(2.0) - 8.0 / 3.0).abs() < 1e-6);
        assert!((pixel_per_pt_for_zoom(0.5) - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn pixel_dimensions_follow_pixel_per_pt() {
        let doc = compile(fixed_page());
        let page = &doc.pages()[0];

        let one = rasterize_page(page, 1.0);
        assert_eq!((one.width, one.height), (100, 200), "1 px/pt 应得到原尺寸");

        let two = rasterize_page(page, 2.0);
        assert_eq!((two.width, two.height), (200, 400), "DPI 翻倍，像素翻倍");

        assert!(one.is_consistent());
        assert!(two.is_consistent());
        assert_eq!(two.rgba.len(), one.rgba.len() * 4);
    }

    #[test]
    fn rasterizing_gives_one_bitmap_per_page() {
        let doc =
            compile("#set page(height: 40pt)\n\nA\n\n#pagebreak()\n\nB\n\n#pagebreak()\n\nC\n");
        assert_eq!(doc.pages().len(), 3);

        let pages = rasterize(&doc, 1.0);

        assert_eq!(pages.len(), 3);
        assert!(pages.iter().all(|p| p.is_consistent()));
    }

    /// 光栅化必须真的画出东西 —— 只断言尺寸的话，全白图也能过。
    #[test]
    fn the_page_is_not_blank() {
        let doc = compile(fixed_page());
        let page = rasterize_page(&doc.pages()[0], 1.0);

        let dark = page
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] < 128 && px[3] > 0)
            .count();

        assert!(
            dark > 0,
            "整页没有一个暗像素，像是根本没渲染出字形（{} 像素）",
            page.pixel_count()
        );
    }

    /// 荒谬的缩放不该 panic，也不该产出 0 尺寸的图。
    #[test]
    fn a_degenerate_zoom_still_produces_a_bitmap() {
        let doc = compile(fixed_page());
        let page = &doc.pages()[0];

        for zoom in [0.0, -1.0, f32::NAN, 1e-9] {
            let r = rasterize_page(page, zoom);
            assert!(r.width >= 1 && r.height >= 1, "zoom={zoom} 给出了空图");
            assert!(r.is_consistent(), "zoom={zoom} 的数据长度不对");
        }
    }

    #[test]
    fn an_empty_document_still_rasterizes() {
        let doc = compile("");
        let pages = rasterize(&doc, 1.0);

        assert!(!pages.is_empty(), "空文档也有 1 页");
        assert!(pages[0].is_consistent());
    }
}
