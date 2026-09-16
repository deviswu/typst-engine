//! 路径工具。

use std::path::{Component, Path, PathBuf};

/// 就地消掉 `.` 与 `..`。
///
/// 不碰磁盘 —— `Path::canonicalize` 要求路径已经存在，而我们要解析的
/// 可能是**还没存盘的新文件**。
///
/// 返回值可以直接用 `starts_with(root)` 做边界检查：`starts_with` 按路径
/// 组件比较，所以 `..` 被消掉之后就不会再误判。
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_dot_and_dot_dot() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn a_climb_past_the_root_does_not_panic() {
        assert_eq!(normalize(Path::new("/../../a")), PathBuf::from("/a"));
    }

    #[test]
    fn a_clean_path_is_unchanged() {
        let p = Path::new("/a/b/c.typ");
        assert_eq!(normalize(p), p);
    }
}
