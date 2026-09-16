//! 包解析（`@preview/...`）。
//!
//! 本模块只认**本地目录**，不联网。目录布局与 `typst` CLI 的约定一致：
//!
//! ```text
//! <data>/<namespace>/<name>/<version>/typst.toml
//! <data>/<namespace>/<name>/<version>/lib.typ
//! ```

use std::path::{Path, PathBuf};

use typst::diag::{FileError, FileResult};
use typst::syntax::VirtualPath;
use typst_syntax::package::PackageSpec;

use crate::path_util::normalize;

/// 包仓库。
#[derive(Debug, Default, Clone)]
pub struct Packages {
    /// 包内容所在的目录（通常是数据目录或缓存目录）。
    data: Option<PathBuf>,
}

impl Packages {
    /// 不认任何包。默认行为 —— 「离线也能编译本地项目」。
    pub fn local_only() -> Self {
        Self::default()
    }

    /// 指定一个包数据目录。
    pub fn from_data_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            data: Some(normalize(&path.into())),
        }
    }

    pub fn data_dir(&self) -> Option<&Path> {
        self.data.as_deref()
    }

    /// 包根目录：`<data>/<ns>/<name>/<version>`。
    pub fn root_of(&self, spec: &PackageSpec) -> FileResult<PathBuf> {
        let Some(data) = &self.data else {
            return Err(FileError::NotFound(PathBuf::from(spec.to_string())));
        };

        let root = data
            .join(spec.namespace.as_str())
            .join(spec.name.as_str())
            .join(spec.version.to_string());

        if !root.is_dir() {
            return Err(FileError::NotFound(root));
        }
        Ok(root)
    }

    /// 把包里的虚拟路径解析成真实路径。
    ///
    /// 与 [`crate::world::EntryState::resolve`] 一样，用官方 `realize`；
    /// 逃逸由 `VirtualPath` 类型本身保证不可能。
    pub fn resolve(&self, spec: &PackageSpec, vpath: &VirtualPath) -> FileResult<PathBuf> {
        let root = self.root_of(spec)?;
        vpath.realize(&root).map_err(FileError::Realize)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use typst::diag::FileError;
    use typst::syntax::VirtualPath;

    use super::*;

    fn spec() -> PackageSpec {
        "@preview/tablex:0.0.2".parse().unwrap()
    }

    fn vpath(p: &str) -> VirtualPath {
        VirtualPath::new(p).unwrap()
    }

    /// 在临时目录里伪造一个「包数据目录」，完全不联网。
    fn fake_data_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("preview").join("tablex").join("0.0.2");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("typst.toml"), "[package]\nname = \"tablex\"\n").unwrap();
        std::fs::write(pkg.join("lib.typ"), "// lib\n").unwrap();
        (dir, pkg)
    }

    #[test]
    fn resolves_a_package_from_a_local_data_dir() {
        let (dir, pkg) = fake_data_dir();
        let packages = Packages::from_data_dir(dir.path());

        let got = packages.resolve(&spec(), &vpath("/lib.typ")).unwrap();

        assert_eq!(got, normalize(&pkg.join("lib.typ")));
    }

    #[test]
    fn resolves_a_nested_path_inside_the_package() {
        let (dir, pkg) = fake_data_dir();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        let packages = Packages::from_data_dir(dir.path());

        let got = packages.resolve(&spec(), &vpath("/src/deep.typ")).unwrap();

        assert_eq!(got, normalize(&pkg.join("src/deep.typ")));
    }

    #[test]
    fn an_unknown_package_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let packages = Packages::from_data_dir(dir.path());

        let err = packages.resolve(&spec(), &vpath("/lib.typ")).unwrap_err();

        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    /// 没配数据目录时不该 panic，也不该悄悄拼出一个相对路径。
    #[test]
    fn local_only_refuses_every_package() {
        let packages = Packages::local_only();

        let err = packages.resolve(&spec(), &vpath("/lib.typ")).unwrap_err();

        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    /// 包目录里的逃逸同样不可能 —— 它由 `VirtualPath` 构造时就挡住了。
    #[test]
    fn escaping_a_package_root_is_impossible_by_construction() {
        assert!(
            VirtualPath::new("/../../../secret").is_err(),
            "typst 应该拒绝会越出包的虚拟路径"
        );
    }

    #[test]
    fn root_of_reports_the_package_directory() {
        let (dir, pkg) = fake_data_dir();
        let packages = Packages::from_data_dir(dir.path());

        assert_eq!(packages.root_of(&spec()).unwrap(), normalize(&pkg));
    }
}
