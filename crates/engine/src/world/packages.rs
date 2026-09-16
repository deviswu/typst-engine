//! 包解析（`@preview/...`）。
//!
//! 两条来源，按这个顺序找：
//!
//! 1. **本地数据目录**（`from_data_dir`）：布局与 `typst` CLI 一致
//! 2. **官方包源**（`with_downloads`）：交给 `typst-kit` 的 `SystemPackages`
//!
//! ```text
//! <data>/<namespace>/<name>/<version>/typst.toml
//! <data>/<namespace>/<name>/<version>/lib.typ
//! ```
//!
//! **默认不联网**（`local_only()`）：离线也要能编译本地项目，测试更不该碰网络。
//!
//! 联网那条走的是和 CLI 一样的目录（Windows 下是 `%APPDATA%/typst/packages`
//! 与 `%LOCALAPPDATA%/typst/packages`），所以跟 CLI 共用一份：CLI 下过的包
//! 这里直接命中，这里下过的 CLI 也不用再下。
//!
//! 取包（尤其是下载）是**同步阻塞**的，就发生在排版中间 —— 第一次遇到某个
//! 包会卡住几秒。所以每次取包都记一条 `Fetch`（含耗时），外壳把它报出来：
//! 「这一下为什么慢」不该靠猜。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use typst::diag::{FileError, FileResult};
use typst::syntax::VirtualPath;
use typst_kit::downloader::SystemDownloader;
use typst_kit::packages::SystemPackages;
use typst_syntax::package::PackageSpec;

use crate::path_util::normalize;

/// 一次取包的记录。引擎不打印，交给外壳报数。
#[derive(Debug, Clone)]
pub struct Fetch {
    /// 包规格，如 `@preview/tablem:0.1.0`。
    pub spec: String,
    /// 包内容所在目录。
    pub path: PathBuf,
    /// 这次取包花了多久。缓存命中是零点几毫秒，真下载是几百毫秒到几秒。
    pub elapsed: Duration,
}

/// 包仓库。
#[derive(Debug, Default, Clone)]
pub struct Packages {
    /// 包内容所在的目录（通常是数据目录或缓存目录）。
    data: Option<PathBuf>,
    /// 官方包源。`None` = 只认本地目录，不联网。
    ///
    /// `Arc` 是因为 `SystemPackages` 不实现 `Clone`，而本类型要能克隆。
    system: Option<Arc<SystemPackages>>,
    /// 取包日志。`Arc<Mutex<..>>` 是为了让克隆出去的副本也往同一份里记。
    fetches: Arc<Mutex<Vec<Fetch>>>,
}

impl Packages {
    /// 不认任何包。默认行为 —— 「离线也能编译本地项目」。
    pub fn local_only() -> Self {
        Self::default()
    }

    /// 本地目录 **+ 联网**取官方包源的包。
    ///
    /// 下载落在 typst 自己的缓存目录（与 CLI 共用）。第一次遇到某个包要等网络，
    /// 之后都是缓存命中 —— 耗时记在 [`Self::take_fetches`] 里。
    pub fn with_downloads() -> Self {
        let user_agent = concat!("typst-engine/", env!("CARGO_PKG_VERSION"));
        Self {
            system: Some(Arc::new(SystemPackages::new(SystemDownloader::new(
                user_agent,
            )))),
            ..Self::default()
        }
    }

    /// 官方包源的缓存目录（下载落在哪儿）。给日志与设置界面看。
    pub fn cache_dir(&self) -> Option<&Path> {
        self.system.as_ref()?.cache()?.path().into()
    }

    /// 取走「取包记录」。外壳每次排版后拿它报数。
    pub fn take_fetches(&self) -> Vec<Fetch> {
        std::mem::take(&mut self.fetches.lock())
    }

    /// 指定一个包数据目录。
    pub fn from_data_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            data: Some(normalize(&path.into())),
            ..Self::default()
        }
    }

    pub fn data_dir(&self) -> Option<&Path> {
        self.data.as_deref()
    }

    /// 包根目录：`<data>/<ns>/<name>/<version>`。
    ///
    /// 先看本地目录；本地没有且开了联网，就问官方包源（**可能在这里下载**）。
    pub fn root_of(&self, spec: &PackageSpec) -> FileResult<PathBuf> {
        // ① 本地数据目录优先：离线可用、项目自带包可用、测试能覆盖
        if let Some(data) = &self.data {
            let root = data
                .join(spec.namespace.as_str())
                .join(spec.name.as_str())
                .join(spec.version.to_string());

            if root.is_dir() {
                return Ok(root);
            }
        }

        // ② 官方包源（`@preview`）。缓存里没有就下载 —— 这一下是同步阻塞的
        if let Some(system) = &self.system {
            let started = Instant::now();
            let root = system.obtain(spec).map_err(FileError::Package)?;
            let path = root.path().to_path_buf();

            self.fetches.lock().push(Fetch {
                spec: spec.to_string(),
                path: path.clone(),
                elapsed: started.elapsed(),
            });
            return Ok(path);
        }

        Err(FileError::NotFound(PathBuf::from(spec.to_string())))
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

    /// 本地目录命中时**不该联网**：不该有取包记录。
    #[test]
    fn a_local_hit_never_touches_the_network() {
        let (dir, _pkg) = fake_data_dir();
        let mut packages = Packages::from_data_dir(dir.path());
        // 顺手接上联网能力，但本地已经有一份 —— 那就该走本地
        packages.system = Packages::with_downloads().system;

        packages.root_of(&spec()).unwrap();

        assert!(
            packages.take_fetches().is_empty(),
            "本地命中就不该去取包（更不该下载）"
        );
    }

    /// 联网取包。**网络不通就跳过**（不是失败）—— 这条测的是「能联上官方源时
    /// 能把包拿下来」，不该让离线环境红一片。
    ///
    /// 挑的是官方源上很小的那类包（tablem 只有一个 lib.typ），而且落到的是
    /// typst 自己的缓存目录，跟 CLI 共用一份。
    #[test]
    fn a_preview_package_can_be_fetched_over_the_network() {
        let packages = Packages::with_downloads();
        let spec: PackageSpec = "@preview/tablem:0.1.0".parse().unwrap();

        let root = match packages.root_of(&spec) {
            Ok(root) => root,
            Err(err) => {
                println!("[跳过] 取不到 {spec}（离线？）：{err}");
                return;
            }
        };

        assert!(
            root.join("typst.toml").is_file(),
            "包目录里该有 typst.toml：{root:?}"
        );

        let fetches = packages.take_fetches();
        assert_eq!(fetches.len(), 1, "该刚好记一条取包记录");
        println!(
            "[实测] 取 {} 用 {:.1} ms → {}",
            fetches[0].spec,
            fetches[0].elapsed.as_secs_f64() * 1000.0,
            fetches[0].path.display(),
        );
        assert!(
            packages.take_fetches().is_empty(),
            "记录是 take 走的，第二次该是空的"
        );
    }
}
