//! `typst::World` 实现。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use typst::diag::{FileError, FileResult, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, Source, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt};
use typst_kit::fonts::FontStore;
use typst_layout::PagedDocument;

use super::entry::EntryState;
use super::packages::Packages;
use super::source_db::SourceDb;
use crate::vfs::{SystemAccessModel, Vfs};

/// 一次排版的产出。
#[derive(Debug)]
pub struct CompileOutcome {
    /// **要显示的文档**：本次成功就是新的；本次失败则是上一次成功的（如果有）。
    ///
    /// 调用方不需要自己记住上一次的结果 —— 那是本类型存在的全部意义。
    pub doc: Option<Arc<PagedDocument>>,
    /// 本次排版是否成功。`false` 时 `doc` 是旧的或 `None`。
    pub fresh: bool,
    /// 本次的错误。成功时为空。
    pub errors: Vec<SourceDiagnostic>,
    /// 本次的警告。成功失败都可能有。
    pub warnings: Vec<SourceDiagnostic>,
    /// 本次排版耗时。
    pub elapsed: Duration,
}

impl CompileOutcome {
    /// 本次是否什么都没产出（既没成功、也没可回退的旧结果）。
    pub fn is_empty(&self) -> bool {
        self.doc.is_none()
    }
}

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
    /// 上一次**成功**排版的文档。
    ///
    /// 打字打错时，预览该继续显示上一次的结果而不是变成空白 ——
    /// 这件事需要有人记住「上一次成功的」，而且应该是引擎而不是每个外壳各记一份。
    success_doc: Option<Arc<PagedDocument>>,
    /// 排版尝试次数（成功与失败都算）。
    compile_attempts: usize,
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
            success_doc: None,
            compile_attempts: 0,
        }
    }

    /// 排版一次，并把「上一次成功的结果」维护好。
    ///
    /// 这是本类型唯一需要 `&mut self` 的方法 —— 它就是那个记住结果的人。
    pub fn compile(&mut self) -> CompileOutcome {
        let started = Instant::now();
        let typst::diag::Warned { output, warnings } = typst::compile::<PagedDocument>(&*self);
        let elapsed = started.elapsed();
        self.compile_attempts += 1;

        match output {
            Ok(doc) => {
                let doc = Arc::new(doc);
                self.success_doc = Some(doc.clone());
                CompileOutcome {
                    doc: Some(doc),
                    fresh: true,
                    errors: Vec::new(),
                    warnings: warnings.to_vec(),
                    elapsed,
                }
            }
            Err(errors) => CompileOutcome {
                // ★ 关键一行：失败时**不动** success_doc，把它交回去继续显示。
                doc: self.success_doc.clone(),
                fresh: false,
                errors: errors.into_iter().collect(),
                warnings: warnings.to_vec(),
                elapsed,
            },
        }
    }

    /// 换一个文档打开。
    ///
    /// **保留字体** —— 扫系统字体要 60 多毫秒，不该为了换个文件重做一遍。
    /// 重置的是：入口、VFS 覆盖层（上篇文档的未保存编辑不该带到新文件上）、
    /// 源文件缓存、以及上一次成功的排版结果。
    pub fn reopen(&mut self, root: impl Into<PathBuf>, main: impl AsRef<Path>) {
        self.entry = EntryState::new(root, main);
        self.vfs.clear_shadows();
        self.sources.invalidate_all();
        self.success_doc = None;
    }

    /// 上一次成功排版的文档。
    pub fn success_doc(&self) -> Option<&Arc<PagedDocument>> {
        self.success_doc.as_ref()
    }

    /// 排版尝试次数。
    pub fn compile_attempts(&self) -> usize {
        self.compile_attempts
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

    /// 把文档改成另一份文本（同时走覆盖层与语法树两条路）。
    fn feed(world: &mut EngineWorld, text: &str) {
        let id = world.entry().main();
        let path = world.entry().main_path().to_path_buf();
        world.vfs_mut().map_shadow(
            &path,
            typst::foundations::Bytes::from_string(text.to_owned()),
        );
        world.sources().feed_memory(id, text);
    }

    #[test]
    fn a_successful_compile_produces_a_document() {
        let (_dir, mut world) = world_for("main.typ");

        let outcome = world.compile();

        assert!(outcome.fresh);
        assert!(outcome.errors.is_empty());
        assert!(outcome.doc.is_some());
        assert!(world.success_doc().is_some());
        assert_eq!(world.compile_attempts(), 1);
    }

    /// ★ 这条是整个「失败不白屏」的根据。
    /// 用 `Arc::ptr_eq` 断言拿回的是**同一份**旧文档，
    /// 而不是「内容相同的新文档」—— 后者证明不了我们没有丢掉旧结果。
    #[test]
    fn a_failed_compile_keeps_the_last_successful_document() {
        let (_dir, mut world) = world_for("main.typ");

        let good = world.compile();
        assert!(good.fresh);
        let good_doc = good.doc.expect("首次该成功");

        // 换成坏语法
        feed(&mut world, "#let x = (1 + \n\n= 未闭合");
        let bad = world.compile();

        assert!(!bad.fresh, "坏语法不该算成功");
        assert!(!bad.errors.is_empty(), "至少该给一条错误");

        let kept = bad.doc.expect("应该回退到上一次成功的结果");
        assert!(
            Arc::ptr_eq(&kept, &good_doc),
            "回退的必须是同一份旧文档，而不是重新排出来的内容等价物"
        );
        assert!(Arc::ptr_eq(world.success_doc().unwrap(), &good_doc));
        assert_eq!(world.compile_attempts(), 2);
    }

    /// 一直没成功过的时候，不能编造一个文档出来。
    #[test]
    fn a_failed_first_compile_yields_no_document() {
        let (_dir, mut world) = world_for("main.typ");
        feed(&mut world, "#let x = (1 + \n");

        let outcome = world.compile();

        assert!(!outcome.fresh);
        assert!(outcome.is_empty(), "从没成功过，不该有图可显示");
        assert!(world.success_doc().is_none());
    }

    /// 修好之后要能恢复，而不是卡在旧结果上。
    #[test]
    fn a_later_success_replaces_the_kept_document() {
        let (_dir, mut world) = world_for("main.typ");
        let first = world.compile().doc.unwrap();

        feed(&mut world, "#let x = (1 +\n");
        assert!(!world.compile().fresh);

        feed(&mut world, "= 修好了\n\n新正文。\n");
        let fixed = world.compile();

        assert!(fixed.fresh);
        let fixed_doc = fixed.doc.unwrap();
        assert!(!Arc::ptr_eq(&fixed_doc, &first), "该换上新文档了");
        assert!(Arc::ptr_eq(world.success_doc().unwrap(), &fixed_doc));
        assert_eq!(world.compile_attempts(), 3, "成功失败都算一次尝试");
    }

    /// 反复失败不会把已经留住的结果弄丢。
    #[test]
    fn repeated_failures_do_not_lose_the_document() {
        let (_dir, mut world) = world_for("main.typ");
        let good = world.compile().doc.unwrap();

        for i in 0..3 {
            feed(&mut world, &format!("#let x = (1 +\n\n= {i}"));
            let outcome = world.compile();
            assert!(!outcome.fresh);
            assert!(
                Arc::ptr_eq(&outcome.doc.unwrap(), &good),
                "第 {i} 次失败后丢了结果"
            );
        }
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
