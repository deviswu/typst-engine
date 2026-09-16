//! PDF 导出 —— **进程内**。
//!
//! 对比 `wu`：它调外部 `typst compile` 生成 PDF 再拿去看。
//! 我们手上已经有 `PagedDocument`（排版结果），直接交给 `typst-pdf`
//! 出字节 —— 不重新排版、不重读文件、不起子进程。

use typst::diag::SourceResult;
use typst_layout::PagedDocument;
use typst_pdf::{PdfOptions, pdf as render_pdf};

/// 把已排版好的文档写成 PDF 字节。
///
/// 失败时返回的是**诊断**（`SourceResult`），不是字符串 —— 与编译错误
/// 同一套形状，所以调用方能把它接到同一个波浪线渲染器上。
pub fn pdf(document: &PagedDocument) -> SourceResult<Vec<u8>> {
    render_pdf(document, &PdfOptions::default())
}

#[cfg(test)]
mod tests {
    use crate::world::{EngineWorld, EntryState, embedded_and_system_fonts};

    use super::*;

    fn compile(source: &str) -> (tempfile::TempDir, std::sync::Arc<PagedDocument>) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.typ");
        std::fs::write(&main, source).unwrap();

        let mut world = EngineWorld::new(
            embedded_and_system_fonts(),
            EntryState::new(dir.path(), &main),
        );

        let doc = world.compile().doc.expect("该编译成功");
        (dir, doc)
    }

    #[test]
    fn produces_a_real_pdf() {
        let (_dir, doc) = compile("= Title\n\nHello PDF.\n");
        let bytes = pdf(&doc).expect("该能导出");
        assert!(bytes.len() > 1000, "PDF 只有 {} 字节，太小了", bytes.len());
        assert_eq!(&bytes[..5], b"%PDF-", "缺 PDF 魔数");
        assert!(bytes.windows(5).any(|w| w == b"%%EOF"), "缺 PDF 结束标记");
    }

    #[test]
    fn multiple_pages_export() {
        let (_dir, doc) = compile("= One\n\nA\n\n#pagebreak()\n\n= Two\n\nB\n");
        assert_eq!(doc.pages().len(), 2);

        let bytes = pdf(&doc).expect("该能导出");
        assert_eq!(&bytes[..5], b"%PDF-");
    }

    /// 中文文档要能导出 —— 字体嵌入是最容易出问题的一环。
    #[test]
    fn cjk_documents_export() {
        let (_dir, doc) = compile("= 中文标题\n\n这是正文。\n");
        let bytes = pdf(&doc).expect("中文该能导出");

        assert!(bytes.len() > 1000);
        assert_eq!(&bytes[..5], b"%PDF-");
    }

    /// 空文档也要能导出，不能 panic。
    #[test]
    fn an_empty_document_exports() {
        let (_dir, doc) = compile("");
        let bytes = pdf(&doc).expect("空文档该能导出");

        assert_eq!(&bytes[..5], b"%PDF-");
    }
}
