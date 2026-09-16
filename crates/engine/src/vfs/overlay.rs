//! 覆盖访问模型：用内存内容盖住下层模型的同名路径。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use typst::diag::FileResult;
use typst::foundations::Bytes;

use super::access::PathAccessModel;

/// 在下层模型之上叠一层内存覆盖。
///
/// 这是「实时编译」的地基：编辑器把未保存的文本写进覆盖层，编译器读到的
/// 就是正在编辑的内容，而磁盘上的文件纹丝不动。
#[derive(Debug, Default, Clone)]
pub struct OverlayAccessModel<M> {
    shadow: HashMap<PathBuf, Bytes>,
    inner: M,
}

impl<M> OverlayAccessModel<M> {
    pub fn new(inner: M) -> Self {
        Self {
            shadow: HashMap::new(),
            inner,
        }
    }

    pub fn inner(&self) -> &M {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut M {
        &mut self.inner
    }

    /// 当前被覆盖的路径。
    pub fn shadow_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.shadow.keys()
    }

    /// 覆盖 `path`。返回**内容是否真的改变了**。
    ///
    /// 写进完全相同的内容返回 `false` —— 调用方据此避免无意义地推进 revision。
    pub fn add_file(&mut self, path: impl Into<PathBuf>, content: Bytes) -> bool {
        let path = path.into();
        let changed = match self.shadow.get(&path) {
            Some(old) => old.as_slice() != content.as_slice(),
            None => true,
        };
        self.shadow.insert(path, content);
        changed
    }

    /// 撤掉覆盖。返回原本是否存在覆盖。
    pub fn remove_file(&mut self, path: &Path) -> bool {
        self.shadow.remove(path).is_some()
    }
}

impl<M: PathAccessModel> PathAccessModel for OverlayAccessModel<M> {
    fn reset(&mut self) {
        self.shadow.clear();
        self.inner.reset();
    }

    fn content(&self, src: &Path) -> FileResult<Bytes> {
        if let Some(bytes) = self.shadow.get(src) {
            return Ok(bytes.clone());
        }
        self.inner.content(src)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::diag::FileError;
    use typst::foundations::Bytes;

    use super::*;
    use crate::vfs::InMemoryAccessModel;

    fn inner_with(path: &str, text: &str) -> InMemoryAccessModel {
        let mut m = InMemoryAccessModel::new();
        m.insert(path, Bytes::from_string(text.to_owned()));
        m
    }

    fn bytes(s: &str) -> Bytes {
        Bytes::from_string(s.to_owned())
    }

    #[test]
    fn shadows_an_existing_file() {
        let mut o = OverlayAccessModel::new(inner_with("a.typ", "disk"));
        o.add_file("a.typ", bytes("memory"));

        assert_eq!(o.content(Path::new("a.typ")).unwrap().as_slice(), b"memory");
    }

    #[test]
    fn removing_a_shadow_falls_back_to_the_inner_model() {
        let mut o = OverlayAccessModel::new(inner_with("a.typ", "disk"));
        o.add_file("a.typ", bytes("memory"));
        assert!(o.remove_file(Path::new("a.typ")));

        assert_eq!(o.content(Path::new("a.typ")).unwrap().as_slice(), b"disk");
    }

    /// 覆盖一个下层不存在的路径 = 新建文件。
    /// 没有这条，「编辑一个还没存盘的新文件」就跑不通。
    #[test]
    fn shadows_a_path_that_does_not_exist_yet() {
        let mut o = OverlayAccessModel::new(inner_with("a.typ", "disk"));
        o.add_file("new.typ", bytes("brand new"));

        assert_eq!(
            o.content(Path::new("new.typ")).unwrap().as_slice(),
            b"brand new"
        );
    }

    #[test]
    fn a_missing_path_is_still_not_found() {
        let o = OverlayAccessModel::new(inner_with("a.typ", "disk"));

        let err = o.content(Path::new("nope.typ")).unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    /// add_file 的返回值必须反映「内容是否真的变了」。
    /// Vfs 的 revision 语义直接建立在这个返回值上。
    #[test]
    fn add_file_reports_whether_the_content_actually_changed() {
        let mut o = OverlayAccessModel::new(InMemoryAccessModel::new());

        assert!(o.add_file("a.typ", bytes("one")), "首次写入算变化");
        assert!(
            !o.add_file("a.typ", bytes("one")),
            "写入完全相同的内容不算变化"
        );
        assert!(o.add_file("a.typ", bytes("two")), "内容不同算变化");
    }

    #[test]
    fn remove_file_reports_whether_a_shadow_existed() {
        let mut o = OverlayAccessModel::new(InMemoryAccessModel::new());

        assert!(!o.remove_file(Path::new("a.typ")), "没有覆盖时返回 false");
        o.add_file("a.typ", bytes("x"));
        assert!(o.remove_file(Path::new("a.typ")), "有覆盖时返回 true");
    }

    #[test]
    fn shadow_paths_lists_only_the_overridden_ones() {
        let mut o = OverlayAccessModel::new(inner_with("a.typ", "disk"));
        o.add_file("b.typ", bytes("x"));

        let paths: Vec<_> = o.shadow_paths().cloned().collect();
        assert_eq!(paths, vec![std::path::PathBuf::from("b.typ")]);
    }
}
