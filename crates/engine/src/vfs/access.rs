//! 「路径 → 字节」的最小访问抽象。

use std::path::Path;

use typst::diag::FileResult;
use typst::foundations::Bytes;

/// 一个能把路径读成字节的东西。
///
/// 实现者自己负责缓存；[`crate::vfs::Vfs`] 只负责组合它们，不关心实现细节。
pub trait PathAccessModel: Send + Sync {
    /// 清空内部缓存。VFS 重置时调用。
    fn reset(&mut self) {}

    /// 读取文件内容。
    fn content(&self, src: &Path) -> FileResult<Bytes>;
}
