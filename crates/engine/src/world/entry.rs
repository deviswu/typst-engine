//! 入口状态：项目根、主文件，以及 `FileId` → 真实路径的解析。

use std::path::{Path, PathBuf};

use typst::diag::{FileError, FileResult};
use typst::syntax::{FileId, RootedPath, VirtualPath, VirtualRoot};

use crate::path_util::normalize;

/// 一次编译的入口信息，同时负责把 `FileId` 解析成磁盘路径并强制 root 边界。
#[derive(Debug, Clone)]
pub struct EntryState {
    root: PathBuf,
    main: FileId,
    main_path: PathBuf,
}

impl EntryState {
    /// `main` 应当位于 `root` 之下。
    pub fn new(root: impl Into<PathBuf>, main: impl AsRef<Path>) -> Self {
        let root = normalize(&root.into());
        let main_path = normalize(main.as_ref());

        let vpath = main_path
            .strip_prefix(&root)
            .ok()
            .and_then(|rel| {
                let rel = rel.to_string_lossy().replace('\\', "/");
                VirtualPath::new(format!("/{rel}")).ok()
            })
            .unwrap_or_else(|| VirtualPath::new("/main.typ").expect("静态路径必然合法"));

        let main = FileId::new(RootedPath::new(VirtualRoot::Project, vpath));

        Self {
            root,
            main,
            main_path,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn main(&self) -> FileId {
        self.main
    }

    pub fn main_path(&self) -> &Path {
        &self.main_path
    }

    /// 把项目内的 `FileId` 解析成真实路径。
    ///
    /// 用 typst 官方的 [`VirtualPath::realize`]。它的文档明确说这就是
    /// 「虚拟路径 → 真实路径」的唯一转换点，且「can be used in the
    /// implementations of `World::source` and `World::file`」。
    ///
    /// 路径逃逸由 `VirtualPath` **类型本身**保证不可能：构造时的
    /// `Segments::normalize` 就会消掉 `..`，越出根时直接报 `PathError::Escapes`。
    /// 所以这里不需要再自查一遍 `..`。
    ///
    /// 包路径在此拒绝 —— 它由 [`crate::world::Packages`] 负责。
    ///
    /// **已知边界**：如果 root 之下有符号链接指向外面，`realize` 挡不住
    /// （官方文档也标了 "a path might still escape through symlinks"）。
    /// 要堵这个口子得靠 `canonicalize` 对比，但那要求文件已存在，
    /// 与「编辑未存盘文件」矛盾。本项目接受这个边界。
    pub fn resolve(&self, id: FileId) -> FileResult<PathBuf> {
        match id.root() {
            VirtualRoot::Project => id.vpath().realize(&self.root).map_err(FileError::Realize),
            VirtualRoot::Package(_) => Err(FileError::AccessDenied),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooted(name: &str) -> FileId {
        FileId::new(RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new(name).unwrap(),
        ))
    }

    #[test]
    fn the_main_file_round_trips_to_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.typ");
        let entry = EntryState::new(dir.path(), &main);

        assert_eq!(entry.resolve(entry.main()).unwrap(), normalize(&main));
    }

    #[test]
    fn a_nested_path_resolves_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let entry = EntryState::new(dir.path(), dir.path().join("main.typ"));

        let got = entry.resolve(rooted("/chapters/intro.typ")).unwrap();

        assert_eq!(got, normalize(&dir.path().join("chapters/intro.typ")));
    }

    /// **逃逸防护来自 typst 的类型系统，不是我们自己的检查。**
    ///
    /// `VirtualPath` 构造时就会消掉 `..`，越出根直接报 `PathError::Escapes`。
    /// 所以根本构造不出一个「会逃逸的 FileId」—— 这条测试把这个上游保证钉住，
    /// 将来 typst 若放寛了它，我们会立刻知道。
    #[test]
    fn an_escaping_virtual_path_cannot_even_be_constructed() {
        assert!(
            VirtualPath::new("/../escaped.typ").is_err(),
            "typst 应该拒绝会越出根的虚拟路径"
        );
        assert!(VirtualPath::new("/../../etc/passwd").is_err());
    }

    /// 内部的 `..` 会被规范化掉，而不是报错 —— 它不越出根。
    #[test]
    fn an_inner_dot_dot_is_normalized_away() {
        let vpath = VirtualPath::new("/a/../b.typ").expect("不越出根，应该允许");

        assert_eq!(vpath.get_without_slash(), "b.typ");
    }

    /// 包路径不归 entry 管，要明确拒绝而不是悄悄拼成磁盘路径。
    #[test]
    fn package_paths_are_rejected_here() {
        let dir = tempfile::tempdir().unwrap();
        let entry = EntryState::new(dir.path(), dir.path().join("main.typ"));
        let spec: typst_syntax::package::PackageSpec = "@preview/tablex:0.0.2".parse().unwrap();
        let id = FileId::new(RootedPath::new(
            VirtualRoot::Package(spec),
            VirtualPath::new("/lib.typ").unwrap(),
        ));

        let err = entry.resolve(id).unwrap_err();

        assert!(matches!(err, FileError::AccessDenied), "got {err:?}");
    }

    #[test]
    fn root_and_main_are_accessible() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.typ");
        let entry = EntryState::new(dir.path(), &main);

        assert_eq!(entry.root(), normalize(dir.path()).as_path());
        assert_eq!(entry.main_path(), normalize(&main).as_path());
    }
}
