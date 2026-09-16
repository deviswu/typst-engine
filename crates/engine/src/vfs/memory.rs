//! 内存访问模型。兼作测试底座。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use typst::diag::{FileError, FileResult};
use typst::foundations::Bytes;

use super::access::PathAccessModel;

/// 一张「路径 → 字节」的表。不碰磁盘。
#[derive(Debug, Default, Clone)]
pub struct InMemoryAccessModel {
    files: HashMap<PathBuf, Bytes>,
}

impl InMemoryAccessModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// 放一个文件进去。覆盖已有的同名条目。
    pub fn insert(&mut self, path: impl Into<PathBuf>, content: Bytes) {
        self.files.insert(path.into(), content);
    }
}

impl PathAccessModel for InMemoryAccessModel {
    fn reset(&mut self) {
        self.files.clear();
    }

    fn content(&self, src: &Path) -> FileResult<Bytes> {
        self.files
            .get(src)
            .cloned()
            .ok_or_else(|| FileError::NotFound(src.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::diag::FileError;
    use typst::foundations::Bytes;

    use super::*;

    #[test]
    fn reads_an_existing_entry() {
        let mut m = InMemoryAccessModel::new();
        m.insert("a.typ", Bytes::from_string("hello".to_owned()));

        let got = m.content(Path::new("a.typ")).unwrap();
        assert_eq!(got.as_slice(), b"hello");
    }

    #[test]
    fn a_missing_entry_is_not_found_not_a_panic() {
        let m = InMemoryAccessModel::new();

        let err = m.content(Path::new("nope.typ")).unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    #[test]
    fn reset_clears_everything() {
        let mut m = InMemoryAccessModel::new();
        m.insert("a.typ", Bytes::from_string("hello".to_owned()));

        m.reset();

        assert!(m.content(Path::new("a.typ")).is_err());
    }
}
