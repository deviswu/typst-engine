//! 虚拟文件系统：把下层模型包进一层内存覆盖，并跟踪 revision。

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use typst::diag::FileResult;
use typst::foundations::Bytes;

use super::access::PathAccessModel;
use super::overlay::OverlayAccessModel;

/// 可覆盖、带 revision 的文件系统。
///
/// **revision 契约**
/// 1. 初值 `1`（`NonZeroUsize`，便于上层用 `Option<NonZeroUsize>` 零开销表示
///    「还没编译过」）。
/// 2. 只在内容实际变化时自增。
/// 3. 写入相同内容、撤掉不存在的覆盖 → 不自增。
pub struct Vfs<M> {
    access: OverlayAccessModel<M>,
    revision: NonZeroUsize,
}

// `M: Clone` 是给 `snapshot()` 用的：快照要复制覆盖层与底层模型。
// 目前唯一的 `M` 是 `SystemAccessModel`（`Copy`），这个约束不花任何代价。
impl<M: PathAccessModel + Clone> Vfs<M> {
    pub fn new(inner: M) -> Self {
        Self {
            access: OverlayAccessModel::new(inner),
            revision: NonZeroUsize::MIN,
        }
    }

    pub fn revision(&self) -> NonZeroUsize {
        self.revision
    }

    /// 底层模型（含覆盖层）。给 `EngineWorld` 转发用。
    pub fn latest(&self) -> &OverlayAccessModel<M> {
        &self.access
    }

    pub fn content(&self, path: &Path) -> FileResult<Bytes> {
        self.access.content(path)
    }

    /// 把编辑器里的未保存文本写进覆盖层。
    ///
    /// 返回内容是否真的变了；变了才推进 revision。
    pub fn map_shadow(&mut self, path: impl Into<PathBuf>, content: Bytes) -> bool {
        if self.access.add_file(path, content) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// 撤掉覆盖，回落到下层模型（磁盘）。
    pub fn unmap_shadow(&mut self, path: &Path) -> bool {
        if self.access.remove_file(path) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// 廉价快照：`Bytes` 是 `Arc` 支撑的，这里的克隆只是引用计数。
    pub fn snapshot(&self) -> Self {
        Self {
            access: self.access.clone(),
            revision: self.revision,
        }
    }

    fn bump(&mut self) {
        // NonZeroUsize 不会有 0；真绕回来了就钉在 MAX。
        self.revision =
            NonZeroUsize::new(self.revision.get().wrapping_add(1)).unwrap_or(NonZeroUsize::MAX);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::foundations::Bytes;

    use super::*;
    use crate::vfs::InMemoryAccessModel;

    fn bytes(s: &str) -> Bytes {
        Bytes::from_string(s.to_owned())
    }

    #[test]
    fn revision_starts_at_one() {
        let vfs = Vfs::new(InMemoryAccessModel::new());
        assert_eq!(vfs.revision().get(), 1);
    }

    #[test]
    fn shadowing_new_content_bumps_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());

        assert!(vfs.map_shadow("a.typ", bytes("one")));
        assert_eq!(vfs.revision().get(), 2);
    }

    /// 这条是「防抖 + 无意义重编译」的地基：
    /// 编辑器重复喂同一份文本（很常见），不能一直推高 revision。
    #[test]
    fn shadowing_identical_content_does_not_bump_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        vfs.map_shadow("a.typ", bytes("one"));
        let rev = vfs.revision();

        assert!(
            !vfs.map_shadow("a.typ", bytes("one")),
            "内容一样，返回 false"
        );
        assert_eq!(vfs.revision(), rev, "revision 不该动");
    }

    #[test]
    fn unmapping_a_shadow_bumps_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        vfs.map_shadow("a.typ", bytes("one"));
        let rev = vfs.revision();

        assert!(vfs.unmap_shadow(Path::new("a.typ")));
        assert!(vfs.revision() > rev);
    }

    #[test]
    fn unmapping_a_non_existent_shadow_does_not_bump_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        let rev = vfs.revision();

        assert!(!vfs.unmap_shadow(Path::new("a.typ")));
        assert_eq!(vfs.revision(), rev);
    }

    #[test]
    fn a_snapshot_is_not_affected_by_later_changes() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        vfs.map_shadow("a.typ", bytes("one"));
        let rev = vfs.revision();

        let snap = vfs.snapshot();
        vfs.map_shadow("a.typ", bytes("two"));

        assert_eq!(snap.content(Path::new("a.typ")).unwrap().as_slice(), b"one");
        assert_eq!(snap.revision(), rev, "快照的 revision 也冻住了");
        assert_eq!(vfs.content(Path::new("a.typ")).unwrap().as_slice(), b"two");
    }

    /// 覆盖层优先于下层，且下层仍能正常读。
    #[test]
    fn the_overlay_wins_over_the_inner_model() {
        let mut inner = InMemoryAccessModel::new();
        inner.insert("a.typ", bytes("disk"));
        inner.insert("b.typ", bytes("untouched"));

        let mut vfs = Vfs::new(inner);
        vfs.map_shadow("a.typ", bytes("memory"));

        assert_eq!(
            vfs.content(Path::new("a.typ")).unwrap().as_slice(),
            b"memory"
        );
        assert_eq!(
            vfs.content(Path::new("b.typ")).unwrap().as_slice(),
            b"untouched",
            "没被覆盖的文件照走下层"
        );
    }
}
