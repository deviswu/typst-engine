//! 磁盘访问模型。

use std::path::Path;

use typst::diag::{FileError, FileResult};
use typst::foundations::Bytes;

use super::access::PathAccessModel;

/// 直接读磁盘。无状态，可随意克隆。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemAccessModel;

impl SystemAccessModel {
    pub fn new() -> Self {
        Self
    }
}

impl PathAccessModel for SystemAccessModel {
    fn content(&self, src: &Path) -> FileResult<Bytes> {
        let data = std::fs::read(src).map_err(|err| {
            // 先判目录：Windows 上读目录给的是 PermissionDenied，
            // Linux 给 IsADirectory。不先判就会在 Windows 上误报 AccessDenied。
            if src.is_dir() {
                FileError::IsDirectory
            } else {
                match err.kind() {
                    std::io::ErrorKind::NotFound => FileError::NotFound(src.to_path_buf()),
                    std::io::ErrorKind::PermissionDenied => FileError::AccessDenied,
                    _ => FileError::Other(None),
                }
            }
        })?;
        Ok(Bytes::new(data))
    }
}

#[cfg(test)]
mod tests {
    use typst::diag::FileError;

    use super::*;

    #[test]
    fn reads_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.typ");
        std::fs::write(&path, "hello").unwrap();

        let got = SystemAccessModel::new().content(&path).unwrap();
        assert_eq!(got.as_slice(), b"hello");
    }

    #[test]
    fn reads_binary_content_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, [0u8, 159, 146, 150]).unwrap();

        let got = SystemAccessModel::new().content(&path).unwrap();
        assert_eq!(got.as_slice(), &[0u8, 159, 146, 150]);
    }

    #[test]
    fn a_missing_file_maps_to_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.typ");

        let err = SystemAccessModel::new().content(&path).unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    /// 目录必须映射成 IsDirectory：typst 见到它才不会当成「空文件」。
    #[test]
    fn a_directory_maps_to_is_directory() {
        let dir = tempfile::tempdir().unwrap();

        let err = SystemAccessModel::new().content(dir.path()).unwrap_err();
        assert!(matches!(err, FileError::IsDirectory), "got {err:?}");
    }
}
