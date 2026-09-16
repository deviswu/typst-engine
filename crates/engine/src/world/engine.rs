//! `typst::World` 实现。

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, Source, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt};
use typst_kit::fonts::FontStore;

use super::entry::EntryState;
use super::packages::Packages;
use super::source_db::SourceDb;
use crate::vfs::{SystemAccessModel, Vfs};

/// 给 typst 编译器用的世界。
///
/// 它同时是「未保存内容的入口」：编辑器把文本喂进 `vfs` 的覆盖层与
/// `sources` 的语法树，编译器读到的就是正在编辑的内容。
pub struct EngineWorld {
    library: LazyHash<Library>,
    fonts: FontStore,
    entry: EntryState,
    packages: Packages,
    vfs: Vfs<SystemAccessModel>,
    sources: SourceDb,
}

impl EngineWorld {
    pub fn new(fonts: FontStore, entry: EntryState) -> Self {
        Self {
            library: LazyHash::new(Library::default()),
            fonts,
            entry,
            packages: Packages::local_only(),
            vfs: Vfs::new(SystemAccessModel::new()),
            sources: SourceDb::new(),
        }
    }

    /// 接上包仓库。
    pub fn with_packages(mut self, packages: Packages) -> Self {
        self.packages = packages;
        self
    }

    pub fn vfs_mut(&mut self) -> &mut Vfs<SystemAccessModel> {
        &mut self.vfs
    }

    pub fn vfs(&self) -> &Vfs<SystemAccessModel> {
        &self.vfs
    }

    pub fn sources(&self) -> &SourceDb {
        &self.sources
    }

    pub fn entry(&self) -> &EntryState {
        &self.entry
    }

    pub fn packages(&self) -> &Packages {
        &self.packages
    }

    /// 把 `FileId` 读成字节：项目路径走 VFS（含内存覆盖），包路径走 `Packages`。
    fn read(&self, id: FileId) -> FileResult<Bytes> {
        let path = match id.root() {
            VirtualRoot::Project => self.entry.resolve(id)?,
            VirtualRoot::Package(spec) => self.packages.resolve(spec, id.vpath())?,
        };
        // 包内容也走 VFS：这样测试里能覆盖，且缓存逻辑统一。
        self.vfs.content(&path)
    }
}

impl typst::World for EngineWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.entry.main()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        // 先取：`cached_text` 要进 SourceDb 拿锁，必须在调 `sources.source()`
        // 之前完成（后者的锁在闭包执行期间是持着的 —— 见 SourceDb 的锁不变式）。
        let cached = self.sources.cached_text(id);
        let this = self;

        self.sources.source(id, move || {
            // 编辑器喂过的文件用缓存里的文本（那就是未保存的内容）；否则读 VFS。
            let bytes = match cached {
                Some(text) => Bytes::from_string(text),
                None => this.read(id)?,
            };
            let text = String::from_utf8(bytes.to_vec()).map_err(|_| FileError::InvalidUtf8)?;
            Ok(Source::new(id, text))
        })
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let cached = self.sources.cached_text(id);
        let this = self;

        self.sources.bytes(id, move || match cached {
            Some(text) => Ok(Bytes::from_string(text)),
            None => this.read(id),
        })
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        // 返回 None：typst 的 `datetime` 会给出明确诊断，
        // 比编造一个假日期安全。
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::embedded_and_system_fonts;

    fn world_for(name: &str) -> (tempfile::TempDir, EngineWorld) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join(name);
        std::fs::write(&main, "= Hi\n\nBody.\n").unwrap();
        let world = EngineWorld::new(
            embedded_and_system_fonts(),
            EntryState::new(dir.path(), &main),
        );
        (dir, world)
    }

    #[test]
    fn reads_the_main_source() {
        let (_dir, world) = world_for("main.typ");

        let src = typst::World::source(&world, world.entry().main()).unwrap();

        assert!(src.text().contains("Body."));
    }

    /// `file()` 对 `source()` 能读的路径也必须能读 —— 这是 typst 的硬要求。
    #[test]
    fn file_agrees_with_source() {
        let (_dir, world) = world_for("main.typ");
        let id = world.entry().main();

        let bytes = typst::World::file(&world, id).unwrap();

        assert!(String::from_utf8(bytes.to_vec()).unwrap().contains("Body."));
    }

    #[test]
    fn a_missing_source_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("nope.typ");
        let world = EngineWorld::new(
            embedded_and_system_fonts(),
            EntryState::new(dir.path(), &main),
        );

        let err = typst::World::source(&world, world.entry().main()).unwrap_err();

        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    #[test]
    fn today_is_none() {
        let (_dir, world) = world_for("main.typ");
        assert!(typst::World::today(&world, None).is_none());
    }

    /// 喂进未保存文本后，`source()` 必须把它反映出来。
    #[test]
    fn source_reflects_fed_memory() {
        let (_dir, world) = world_for("main.typ");
        let id = world.entry().main();

        // 先读一次，把 Source 建起来。
        typst::World::source(&world, id).unwrap();

        let outcome = world.sources().feed_memory(id, "= Edited\n\nNew body.\n");
        assert!(!outcome.created, "已有缓存时该走增量重解析路径");

        let src = typst::World::source(&world, id).unwrap();
        assert_eq!(src.text(), "= Edited\n\nNew body.\n");
    }

    /// 没读过就直接喂，应该能工作（退化成新建）。
    #[test]
    fn feeding_before_reading_works() {
        let (_dir, world) = world_for("main.typ");
        let id = world.entry().main();

        let outcome = world.sources().feed_memory(id, "= Cold start\n");
        assert!(outcome.created, "还没缓存过，这是首次构建");

        let src = typst::World::source(&world, id).unwrap();
        assert_eq!(src.text(), "= Cold start\n");
    }
}
