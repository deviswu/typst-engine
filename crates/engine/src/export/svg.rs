//! SVG 导出。
//!
//! 选 SVG 而不是位图做预览，理由：
//! - **矢量**，任意缩放都清晰，不用按 DPI 重渲染
//! - **体积小**，一页通常几十 KB，更新时传输便宜
//! - **可 diff**，将来做页内增量推送时能直接比字符串
//! - 文字是真实轮廓，选中/复制语义由上层决定

use typst_layout::{Page, PagedDocument};
use typst_svg::{SvgOptions, svg};

/// 把每一页渲染成一个 SVG 字符串（顺序与 `pages()` 一致）。
pub fn page_svgs(document: &PagedDocument) -> Vec<String> {
    let opts = SvgOptions::default();
    document.pages().iter().map(|p| svg(p, &opts)).collect()
}

/// 只渲染一页。
pub fn page_svg(page: &Page) -> String {
    svg(page, &SvgOptions::default())
}

#[cfg(test)]
mod tests {
    use crate::world::{EngineWorld, EntryState, embedded_and_system_fonts};

    use super::*;

    /// 在临时目录里编译一段源码，拿回排版结果。
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

    fn two_page_source() -> &'static str {
        "= One\n\nBody.\n\n#pagebreak()\n\n= Two\n\nMore.\n"
    }

    #[test]
    fn every_page_becomes_a_valid_svg() {
        let doc = compile(two_page_source());
        let svgs = page_svgs(&doc);

        assert_eq!(svgs.len(), 2, "源码里有 1 个 pagebreak");
        assert_eq!(svgs.len(), doc.pages().len(), "页数要一一对应");
        for (i, s) in svgs.iter().enumerate() {
            assert!(s.contains("<svg"), "第 {i} 页缺 <svg> 根元素");
            assert!(s.len() > 500, "第 {i} 页只有 {} 字节，像是空的", s.len());
        }
    }

    #[test]
    fn a_single_page_can_be_rendered_alone() {
        let doc = compile(two_page_source());

        let one = page_svg(&doc.pages()[0]);
        let all = page_svgs(&doc);

        assert_eq!(one, all[0], "单页渲染应与整篇渲染的第一页一致");
        assert_ne!(all[0], all[1], "两页内容不同，SVG 也该不同");
    }

    /// 空文档也要能导出，不能 panic。
    #[test]
    fn an_empty_document_still_exports() {
        let doc = compile("");
        let svgs = page_svgs(&doc);

        assert!(!svgs.is_empty(), "空文档也有 1 页");
        assert!(svgs[0].contains("<svg"));
    }
}
