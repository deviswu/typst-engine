# typst-engine 设计文档

> **状态**：待评审
> **日期**：2026-09-16
> **上游依据**：对 [tinymist](https://github.com/Myriad-Dreamin/tinymist) `0.15.8` 编译链的源码级拆解
> **方案**：甲（UI 无关的引擎库 + 极薄外壳），已由用户确认

---

## 1. 目标

做一个 **UI 无关的 Typst 实时增量编译引擎**：喂进「正在编辑中的文本」，拿回「排版结果 / 预览产物 / 诊断」，全程进程内、无子进程、无 IPC。

具体要做到：

| # | 目标 | 说明 |
|---|---|---|
| G1 | **实时** | 敲键即可看到结果，无需存盘 |
| G2 | **增量** | 改一个词，只重算受影响的部分，不整篇重排 |
| G3 | **不白屏** | 编译失败时继续显示上一次成功的结果 |
| G4 | **可单独测试** | 不依赖任何 UI，`cargo test` 直接断言编译产物 |
| G5 | **不锁死上游** | 只用 crates.io 官方 `typst 0.15.1`，无 git fork、无 patch |

## 2. 非目标（YAGNI）

明确**不做**，避免范围蔓延：

- **语言服务器 / LSP** —— 本项目是编译引擎，不是编辑器协议实现
- **分析层**（类型检查、程序依赖图、引用查找、重命名）—— 这是 tinymist-query 的领域，也是它需要 fork typst 的唯一原因
- **多项目 / lock database** —— 单入口文档模型足够
- **DAP 调试、测试运行器、覆盖率**
- **wasm / 浏览器目标** —— 桌面优先；`notify`、系统字体、文件系统都是桌面假设
- **自研字体解析与包解析** —— 用 `typst-kit` 抄近路
- **HTML / Markdown 导出** —— v1 只要 SVG / 位图 / PDF

## 3. 背景：tinymist 拆解结论

以下结论均来自源码，是实现方案的直接依据。

### 3.1 编译落点：官方 API，零魔法

tinymist 绕了两层壳，最后一行还是官方 API：

```rust
// tinymist/crates/typst-shim/src/syntax_only.rs
pub fn compile_opt<D>(world: &dyn World) -> Warned<SourceResult<D>> where D: Output {
    if is_syntax_only() { /* 返回假错误 */ }
    else { typst::compile::<D>(world) }        // ← 唯一落点
}
```

`D` = `TypstPagedDocument`（即 `typst_layout::PagedDocument`）。`typst-shim` 只加了一个全局 `SYNTAX_ONLY` 原子开关（性能分析时跳过真编译）。

**→ 结论：不需要 tinymist 的任何代码。**

### 3.2 唯一的 `[patch.crates-io]` 只为分析层

```toml
# tinymist/Cargo.toml
# These patches use a different version of `typst`, which only exports
# some private functions and information for code analysis.
typst = { git = "https://github.com/Myriad-Dreamin/typst.git", tag = "tinymist/v0.15.1" }
```

**→ 结论：fork 只服务「分析层」导出私有内部符号，编译路径完全不需要。本项目用 crates.io 官方版本。**

### 3.3 语法树：`typst-syntax`，**不是 tree-sitter**

tinymist 全项目没有用 tree-sitter 解析 Typst。它的语义高亮直接建在官方语法树上：

```rust
// tinymist/crates/tinymist-world/src/parser/semantic_tokens.rs
use typst::syntax::{LinkedNode, Source, SyntaxKind, ast};
// 文件注释还写着 "Very similar to `typst_ide::Tag`"
```

`LinkedNode` 游标遍历源码 → `SyntaxKind` 映射到 `TokenType` + `Modifier`。

**→ 结论：本项目语法服务只依赖 `typst-syntax`，不引入 tree-sitter。**

### 3.4 「实时」的三个支柱

| 支柱 | tinymist 的做法 | 源码位置 |
|---|---|---|
| **overlay VFS** | 内存内容覆盖磁盘内容，编译器读到未保存文本 | `crates/tinymist-vfs/src/overlay.rs` |
| **revision 失效** | 通过 `RevisingUniverse` 改动，只作废变化的那部分 | `crates/tinymist-world/src/world.rs:205` |
| **comemo 记忆化** | `#[comemo::memoize]`，未变的部分自动命中 | `crates/tinymist-world/src/world.rs:1112`、`compiler.rs:985` |

`tinymist-vfs` 的**源码注释写明它抄自 rust-analyzer 的 `crates/vfs`**：

```
//! upstream of following files <https://github.com/rust-lang/rust-analyzer/tree/master/crates/vfs>
```

分层组合（`crates/tinymist-vfs/src/lib.rs:101-105`）：

```rust
type VfsPathAccessModel<M> = OverlayAccessModel<ImmutPath, NotifyAccessModel<M>>;
type VfsAccessModel<M> =
    OverlayAccessModel<FileId, ResolveAccessModel<VfsPathAccessModel<M>>, RawFileId>;
```

**→ 结论：overlay VFS 是「实时」与「保存后编译」的分界线，必须自建。**

### 3.5 其它采用的设计

| 设计 | 作用 | 源码位置 |
|---|---|---|
| `CompileSnapshot.success_doc` | 编译失败保留上次成功的文档 → 不白屏 | `snapshot.rs:105` |
| `CompileSignal` + `TaskWhen` | 「这次该不该编译」的策略引擎 | `snapshot.rs:36`、`TaskWhen::{Never,Script,OnType,OnSave,..}` |
| `WorldComputeGraph`（`TypeId` → `OnceLock` 缓存） | 导出任务只算一次，快照间复用 | `world/compute.rs` |
| ~~`rpds::RedBlackTreeMapSync`~~ | tinymist 用它做持久化 map，好让快照带上整份缓存 | `compute.rs:37` 　**→ 本项目不采用，见下** |
| `SourceDb` + `QueryRef<Source>` | 未变文件的 `Source` 跨 revision 复用 → comemo 命中前提 | `world/source.rs` |
| actor + `Interrupt` 队列 | 单人串行化所有变更，杜绝并发编译 | `project/compiler.rs` |
| 增量 SVG diff（`"diff-v1 frame"`） | 预览不重传整页 | `typst-preview/src/actor/render.rs:152` |

### 3.6 明确**不采用**的

| 不采用 | 原因 |
|---|---|
| `tinymist-vfs` 的 `dummy` / `browser` / `trace` 层 | 分别只服务 mock 编译、浏览器目标、调试追踪 |
| `tinymist-world/src/font/`（10 个文件）与 `/package/` | 自研字体/包解析；`typst-kit` 已够用 |
| `ProjectInsId` / 多项目 `dedicates` | 单入口文档模型 |
| `typlite` / `crityp` / `tinymist-dap` / `tinymist-lint` | 非目标 |
| `Features` 实验特性开关 | YAGNI，需要时再加 |

---

## 4. 架构

### 4.1 分层

```
┌───────────────────────────────────────────────────────────┐
│ 外壳（不在本仓库）                                          │
│   GPUI + gpui-component InputState · 预览面板 · 波浪线      │
└─────────────────────────┬─────────────────────────────────┘
                          │  公开 API（&str 进 / 产物出）
┌─────────────────────────▼─────────────────────────────────┐
│ crates/syntax-svc        L4  语法服务                      │
│   parse → SyntaxNode · 高亮 token · 大纲 · 折叠 · Span 映射 │
│   依赖：typst-syntax（仅此一个）                            │
├───────────────────────────────────────────────────────────┤
│ crates/engine            L3  导出计算图                    │
│   Computable per 格式 · TypeId 缓存 · 页级增量渲染          │
│                          L2  编译驱动                      │
│   Interrupt 队列 · 防抖合并 · CompileSignal · 失败保留      │
│                          L1  增量 World                    │
│   SourceDb · revision 失效 · 字体 · 包 · EntryState         │
│                          L0  Overlay VFS ★                 │
│   内存覆盖 · 路径解析 · notify 事件 · revision              │
│   依赖：typst · typst-layout/render/svg/pdf · typst-kit     │
│          comemo · typst-kit · notify · tokio · parking_lot │
└───────────────────────────────────────────────────────────┘
```

### 4.2 为什么拆成两个 crate

不是为了好看，是**编译依赖隔离**：

- `syntax-svc` 只依赖 `typst-syntax`。外壳需要**每次敲键**都做高亮 —— 它应该能不链接 typst 排版/渲染/PDF 这一大坨。
- 改高亮逻辑不该触发编译器栈重编（`typst` + `typst-layout` + `typst-pdf` 是重量级依赖）。
- 两个 crate 的边界在编译期强制成立，靠约定维持的边界迟早会破。

### 4.3 目录结构

```
typst-engine/
├── Cargo.toml                      [workspace]
├── README.md
├── docs/superpowers/specs/         本设计文档
├── crates/
│   ├── syntax-svc/
│   │   ├── Cargo.toml              [package] typst-syntax-svc
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── tokens.rs           高亮 token
│   │   │   ├── outline.rs          大纲
│   │   │   ├── folding.rs          折叠范围
│   │   │   └── span_map.rs         Span ↔ 字节范围
│   │   └── tests/
│   │       └── fixtures/*.typ
│   └── engine/
│       ├── Cargo.toml              [package] typst-engine
│       ├── src/
│       │   ├── lib.rs
│       │   ├── vfs/                L0
│       │   │   ├── mod.rs
│       │   │   ├── access.rs       PathAccessModel trait
│       │   │   ├── memory.rs       InMemoryAccessModel（兼作测试底座）
│       │   │   ├── system.rs       SystemAccessModel（磁盘）
│       │   │   ├── overlay.rs      OverlayAccessModel ★
│       │   │   ├── resolve.rs      ResolveAccessModel（root 边界）
│       │   │   ├── notify.rs       NotifyAccessModel（notify 8）
│       │   │   └── vfs.rs          Vfs + RevisingVfs + revision
│       │   ├── world/              L1
│       │   │   ├── mod.rs
│       │   │   ├── world.rs        impl typst::World
│       │   │   ├── source_db.rs    QueryRef<Source> / QueryRef<Bytes>
│       │   │   ├── entry.rs        EntryState（main + sys.inputs + root）
│       │   │   ├── font.rs         typst-kit FontStore
│       │   │   └── package.rs      typst-kit 包解析
│       │   ├── driver/             L2
│       │   │   ├── mod.rs
│       │   │   ├── actor.rs        Interrupt 队列 ★
│       │   │   ├── signal.rs       CompileSignal + TaskWhen
│       │   │   ├── snapshot.rs     CompileSnapshot（含 success_doc）
│       │   │   └── debounce.rs     防抖 + pending 合并
│       │   └── export/             L3
│       │       ├── mod.rs
│       │       ├── graph.rs        ComputeGraph + Computable ★
│       │       ├── document.rs     PagedDocument 计算
│       │       ├── svg.rs
│       │       ├── pixmap.rs
│       │       └── pdf.rs
│       └── tests/
│           └── fixtures/*.typ
```

**文件边界原则**：`vfs/` 里每个 access model 一个文件（它们各自可独立单测）；`driver/` 按职责分（队列 / 策略 / 快照 / 防抖）；`export/` 每种格式一个文件。

---

## 5. 组件设计

> 以下为**接口设计**，不是实现。签名以能定死边界为度。

### 5.1 L0 · Overlay VFS

#### 核心 trait

```rust
/// 路径 → 内容的最小访问模型。
pub trait PathAccessModel: Send + Sync {
    /// 清空内部缓存（VFS 重置时调用）。
    fn reset(&mut self) {}

    /// 读取文件内容。
    fn content(&self, src: &Path) -> FileResult<Bytes>;
}
```

这是从 tinymist 抄的形状（`crates/tinymist-vfs/src/lib.rs:75`），但**去掉 `AccessModel`（按 FileId 的那一层）** —— 那一层只有 tinymist 的多项目解析需要，本项目用 `ResolveAccessModel` 一个实现即可覆盖。

#### 五个实现

| 实现 | 职责 | 关键行为 |
|---|---|---|
| `InMemoryAccessModel` | `HashMap<ImmutPath, Bytes>` | 兼作**测试底座**（替代 tinymist 的 `dummy` + `mock` 两层） |
| `SystemAccessModel` | 读磁盘 | 读失败映射成 `FileError::{NotFound, Io}` |
| `NotifyAccessModel` | 记录 notify 8 事件 | 保存「哪些路径被外部改过」；底层 `notify = "8"` |
| `OverlayAccessModel<M>` | **内存覆盖磁盘** ★ | `HashMap<ImmutPath, FileSnapshot>` 覆盖层 + `inner: M` |
| `ResolveAccessModel<M>` | `FileId` → 路径解析 | 强制 root 边界，禁止越界读 |

#### 组合

```rust
pub type VfsAccessModel<M> =
    ResolveAccessModel<OverlayAccessModel<ImmutPath, NotifyAccessModel<M>>>;
```

**相对 tinymist 的简化**：tinymist 是 6 层（`dummy` / `system` / `overlay` / `resolve` / `notify` / `trace` / `browser`），
且最外面还套一层**按 `FileId` 索引**的 overlay（`crates/tinymist-vfs/src/lib.rs:101-105`），
那一层只服务 `map_shadow_by_id`（影子虚拟文件）。本项目：

- 去掉 `dummy` / `browser` / `trace`；`mock` 合并进 `InMemoryAccessModel`
- 去掉按 `FileId` 索引的 overlay —— 单入口文档模型下，**按路径覆盖就够了**

**7 层 → 4 层。**

#### 为什么不用 `rpds`（偏离 tinymist 的第二处）

tinymist 用 `rpds::RedBlackTreeMapSync`（持久化不可变 map）做 overlay 和 compute graph，
目的是让 `snapshot()` 变成 O(1) 的结构共享。本项目**不引入 `rpds`**，用普通 `HashMap`，原因：

1. **值是 Arc 支撑的，普通克隆已经够便宜** —— `typst::foundations::Bytes` 的实际定义是
   `pub struct Bytes(Arc<LazyHash<dyn Bytelike>>)`（`typst-library-0.15.1/src/foundations/bytes.rs:46`），
   克隆只走一个 `Arc` 引用计数。克隆一个 `HashMap<Path, FileSnapshot>` = N 次原子加。
2. **N 很小** —— overlay 里只装「编辑器当前打开的文件」，量级是个位数到几十。
3. **单写者** —— 没有 tinymist 那种高并发下结构共享的收益。

少一个依赖、少一层间接。若将来 profile 显示快照真是瓶颈，再换成 `rpds`（接口不变）。

#### `Vfs` 与 revision

```rust
pub struct Vfs<M: PathAccessModel> {
    access: VfsAccessModel<M>,
    revision: NonZeroUsize,
}

impl<M: PathAccessModel> Vfs<M> {
    pub fn revision(&self) -> NonZeroUsize;
    /// 快照：access 里用持久化 map，克隆廉价；revision 直接拷贝。
    pub fn snapshot(&self) -> Self;

    /// 把编辑中的文本写进 overlay（这是「实时」的入口）。
    pub fn map_shadow(&mut self, path: &Path, content: Bytes) -> FileResult<()>;
    pub fn unmap_shadow(&mut self, path: &Path) -> FileResult<()>;

    /// 删除 / 新增 / 修改都走这里，落回磁盘或 overlay。
    pub fn revise(&mut self, f: impl FnOnce(&mut RevisingVfs<'_, M>)) -> FileResult<()>;
}
```

`RevisingVfs` 沿用 tinymist 的 **Drop 收尾**模式（`lib.rs:452-460`）：所有改动在 `Drop` 里统一结算 `goal_revision`，保证 revision 只在**真的**有内容变化时才自增。

> **缓存归属（相对 tinymist 的一处刻意偏离）**：`Source` / `Bytes` 缓存**只在 `SourceDb`（§5.2）里存一份**。
> tinymist 在 `Vfs`（`Vfs::source_cache`，`lib.rs:182-190`）和 `SourceDb`（`source.rs`）里**各存了一份**。
> 本项目不这样做 —— 一个缓存两个 owner 是 bug 温床，而且两份缓存各自的失效时机一旦不一致，
> 就会直接破坏 §5.2 的 `Arc` 复用不变式。

**revision 语义（必须写死的契约）**：

1. `revision` 只在 VFS 内容**实际变化**时自增；写进相同内容**不**自增。
2. 任何 `map_shadow` / `unmap_shadow` / 磁盘事件导致的内容变化，都必须自增。
3. `revision` 是 `NonZeroUsize` —— 便于 `Option<NonZeroUsize>` 零额外开销表示「还没编译过」。

#### overlay 的边界语义

- 覆盖**已存在**的磁盘文件 → 读取时返回 overlay 内容，磁盘不动。
- `unmap_shadow` 撤掉覆盖 → 立刻回落到磁盘内容。
- 覆盖**不存在**的路径 → 视为新文件（`#include` 一个还没存盘的文件必须能工作）。
- 覆盖内容与磁盘**相同** → 仍算一次覆盖（但 revision 不因内容相同而变）。

### 5.2 L1 · 增量 World

```rust
pub struct EngineWorld {
    library: Arc<LazyHash<Library>>,
    root: Arc<Path>,
    entry: EntryState,
    vfs: Vfs<VfsAccessModel<SystemAccessModel>>,
    source_db: SourceDb,
    fonts: FontResolver,
    packages: PackageRegistry,
    revision: NonZeroUsize,
}

impl typst::World for EngineWorld {
    fn library(&self) -> &LazyHash<Library>;
    fn main(&self) -> FileId;
    fn source(&self, id: FileId) -> FileResult<Source>;
    fn file(&self, id: FileId) -> FileResult<Bytes>;
    fn book(&self) -> &LazyHash<FontBook>;
    fn font(&self, id: usize) -> Option<Font>;
    fn today(&self, offset: Option<Duration>) -> Option<Datetime>;
}
```

`library()` / `book()` / `font()` / `today()` 都照 tinymist 的实现照抄语义（`world.rs:799-880`）。

#### `SourceDb` —— comemo 命中的前提

```rust
pub struct SourceDb {
    slots: Arc<Mutex<FxHashMap<FileId, SourceSlot>>>,
}

struct SourceSlot {
    source: QueryRef<Source>,   // parse 结果缓存 ★
    buffer: QueryRef<Bytes>,    // 原始字节缓存
    touched: bool,              // 本轮编译是否用到
}

/// Arc<OnceLock<Result<T, E>>>：只算一次，之后免费拿。
pub struct QueryRef<T, E>(Arc<OnceLock<Result<T, E>>>);
```

**两条不变式（整个「增量」的地基）**

先纠正一个容易搞错的点 —— 查了 `typst-syntax-0.15.1/src/source.rs:22-24` 与 `typst-utils` 的 `LazyHash`：

```rust
/// Values of this type are cheap to clone and hash.
#[derive(Clone, Hash)]
pub struct Source(Arc<LazyHash<SourceInner>>);
```

`Source` **不按指针比较** —— `Hash` 是派生的，落到 `LazyHash<SourceInner>`（哈希值计算一次后缓存）。
所以「内容相同的两个 `Source`」在 comemo 眼里是**相等**的，缓存**仍会命中**。
这比我最初想的更健壮，但也意味着真正的成本不在 comemo，而在 **`Source::new` 里的 `parse` + `numberize`**。

于是两条不变式各管一件事：

> **I1（省 comemo 重算）**：文件内容未变 ⇒ 不重新造 `Source`。
> 目的不是保 comemo 命中，而是**省掉一次全量 parse**。
>
> **I2（省 parse）**：被编辑的文件 ⇒ 用 `Source::replace` **原地增量重解析**，不调 `Source::new`。

I2 是官方指定的做法 —— `typst-library-0.15.1/src/lib.rs:47-55` 的原话：

> All loading functions (`main`, `source`, `file`, `font`) should perform **internal caching** so that they are
> relatively cheap on repeated invocations with the same argument. … **Advanced clients like language servers
> can also retain the source files and `edit` them in-place to benefit from better incremental performance.**

对应的两个 API（`source.rs:85` 与 `:104`）：

```rust
pub fn replace(&mut self, new: &str) -> Range<usize>;                        // 按公共前后缀找最小改动
pub fn edit(&mut self, replace: Range<usize>, with: &str) -> Range<usize>;   // 增量 reparse
```

两者都**返回「实际重解析的范围」** —— 这就是度量增量有效性的直接指标（见 §8 的 A9），
不需要猜、不需要破坏性实验。内部用 `Arc::make_mut`：我们独占时就地改，没有重新分配。

**→ 这改变了 `SourceDb` 的定位**：它不只是「带记忆化的缓存」，而是**一棵持续增量维护的语法树**。
被编辑的文件在 `SourceDb` 里**持有可变的 `Source`**，`feed_memory` 时调 `replace` 而不是重建。

#### EntryState

```rust
pub struct EntryState {
    pub root: Arc<Path>,
    pub main: Option<FileId>,
    pub inputs: Arc<LazyHash<Dict>>,   // sys.inputs
}
```

`root` 决定 `ResolveAccessModel` 的安全边界 —— **禁止 `#include` 到 root 之外**（tinymist 在 `entry.rs` 里也做同样的 workspace 校验）。

#### 字体与包

- 字体：`typst-kit` 的 `FontStore`（embedded + `scan-fonts` 扫系统）。与 `jicheng` 现在的做法一致，已验证可用。
- 包：直接用 `typst-kit`，**R4 已实测解除**（见 §9）。`typst-kit 0.15.1` 提供 `packages::SystemPackages`，
  文档原语是 "Serves packages from standard locations … loads packages from the same sources as the CLI"（数据目录 →
  缓存目录 → 从 Typst Universe 下载）。需要 features `["system-packages", "universe-packages"]`，
  下载器用 `downloader::SystemDownloader`（feature `system-downloader`）。
  `VirtualRoot::Package(PackageSpec)` → `SystemPackages::obtain(&spec)` → `FsRoot` → 拼 `vpath`。

### 5.3 L2 · 编译驱动

#### 事件与队列

```rust
pub enum Interrupt {
    /// 编辑器敲键：把未保存文本写进 overlay。
    Memory { path: PathBuf, text: String },
    /// notify 报的磁盘变化。
    Fs { changes: FileChangeSet },
    /// 该编译了。无载荷 —— 本项目是单入口文档模型，没有 tinymist 的多项目概念。
    Compile,
    /// 编译完成（把结果交回驱动）。
    Compiled(CompiledArtifact),
    /// 有副作用的导出。SVG / 位图是**拉取式**的，走 `ComputeGraph`，不入队。
    Export(ExportRequest),
    Shutdown,
}

pub struct CompiledArtifact {
    pub revision: NonZeroUsize,
    /// 本次编译产出的文档。失败时为 `None`，此时快照沿用上一次成功的。
    pub doc: Option<PagedDocument>,
    pub warnings: Vec<SourceDiagnostic>,
    pub errors: Vec<SourceDiagnostic>,
}

pub enum ExportRequest {
    Pdf { out: PathBuf },
}
```

**单写者 actor 循环**：所有变更走同一个队列串行处理。这是 tinymist `compiler.rs` 的核心结构，**必须照抄** —— 并发编译会让 comemo 缓存互相踩，表现为随机性能抖动，极难排查。

#### 「何时编译」策略

```rust
pub struct CompileSignal {
    pub by_mem_events: bool,   // 来自编辑器内存事件
    pub by_fs_events: bool,    // 来自磁盘事件
    pub by_entry_update: bool, // 来自入口切换
}

pub enum TaskWhen { Never, OnType, OnSave }

impl CompileSignal {
    pub fn any(&self) -> bool;
    pub fn merge(&mut self, other: CompileSignal);
}
```

**相对 tinymist 的简化**：`TaskWhen` 从 5 个变体砍到 3 个 —— 去掉 `Script`（需要脚本引擎）和 `OnDocumentHasTitle`（LSP 专用）。

#### 快照与「不白屏」

```rust
pub struct CompileSnapshot {
    pub signal: CompileSignal,
    pub world: EngineWorld,
    /// ★ 上一次**成功**编译的文档。本次失败时继续用它出图。
    pub success_doc: Option<PagedDocument>,
}
```

失败处理规则（照抄 tinymist 的 `success_doc` 思路）：

| 本次编译结果 | `success_doc` | 送给外壳的产物 |
|---|---|---|
| 成功 | 替换为新的 | 新文档 |
| 失败（有 error） | **保持不变** | **旧文档** + 新诊断 |
| 失败且从没成功过 | 仍为 `None` | 无预览，只有诊断 |

这就是「打错字时预览不白屏」的实现 —— 代价是预览与源码可能短暂不一致，因此诊断必须**同时**推到外壳，由外壳负责提示。

#### 防抖与合并

```rust
pub struct Debounce {
    window: Duration,   // 默认 150ms，可配
    pending: Option<CompileSignal>,
    compiling: bool,
}
```

规则：

1. `Memory` 事件重置 150ms 定时器（连续输入不触发编译）。
2. 定时器到期 → 若不在编译中，立刻编译。
3. **若正在编译**：不排队第二次编译，只置 `pending = Some(merged_signal)`；当前编译一结束立刻补跑一轮。**同一时刻最多一个编译在跑。**
4. 编译中到来的多次编辑**合并成一次**待编译，不是 N 次。

> 150ms 是**初始值**，最终值由 §8 的验收指标实测决定。

### 5.4 L3 · 导出计算图

```rust
pub struct ComputeGraph {
    snap: CompileSnapshot,
    entries: Mutex<HashMap<TypeId, Entry>>,
}

pub trait Computable: Any + Send + Sync + Sized {
    type Output: Send + Sync + 'static;
    fn compute(graph: &Arc<ComputeGraph>) -> Result<Self::Output>;
}

impl ComputeGraph {
    pub fn get<T: Computable>(&self) -> Option<Result<Arc<T::Output>>>;
    pub fn must_get<T: Computable>(&self) -> Result<Arc<T::Output>>;
    /// 同 revision 的克隆：entries 一起带上 → 缓存跨快照复用。
    pub fn snapshot(&self) -> Arc<Self>;
}
```

**为什么用 `TypeId` 当键**：新增一种导出格式 = 加一个 `Computable` 实现 + 一个类型，**不改计算图**。这是 tinymist `world/compute.rs` 的做法。

v1 的计算单元：

| 类型 | 产物 | 说明 |
|---|---|---|
| `DocumentCompute` | `PagedDocument` | 一轮编译的核心产物，其余都依赖它 |
| `SvgCompute` | `Vec<String>`（每页一个 SVG） | 矢量、可 diff、体积小 |
| `PixmapCompute` | `Vec<Arc<Pixmap>>`（每页一张位图） | 光栅，供不做矢量绘制的场景；用 `Arc` 是为了下面能比 `ptr_eq` |
| `PdfCompute` | `Bytes` | 导出用，按需 |

#### ⚠️ 预览用位图还是 SVG —— 实测后的决定

初稿打算用 SVG 做预览（矢量、体积小、可 diff）。实测否掉了，三个理由：

1. **SVG 体积大**：一页 15cm 的中文文档，SVG 字符串 **493 KiB**（CJK 字形全转路径）
2. **分辨率失控**：gpui 走 `Image::from_bytes` 时**固定按 SVG 自然尺寸 ×2 光栅化**
   （`platform.rs:2231` 传 1.0，`svg_renderer.rs` 再乘 `SMOOTH_SVG_SCALE_FACTOR = 2`），
   且 `Image::from_bytes` 的纹理缓存键是**内容哈希** —— 同一份 SVG 永远复用同一张纹理，
   改显示尺寸拿不到更高分辨率。**缩放必然糊。**
3. **显存浪费**：上述 2× 超采样把 1.5 MiB 的页面变成 **5.24 MiB** 纹理
4. 而且 `ImageSource::Image` 走 `use_asset` —— **异步解码**，第一次必然拿不到结果

改用 `typst-render` 自己出图后：**DPI 完全由我们决定**，且可以在 gpui 之外
**同步**构造 `RenderImage`，绕开 asset 系统。

| | 旧（SVG） | 新（位图） |
|---|---|---|
| 100% 纹理 | 5.24 MiB（被 2× 超采样放大） | **1.5 MiB** |
| 400% 清晰度 | **糊**（2× 封顶） | 锐利（按 5.33 px/pt 重出图） |
| 第一帧 | 异步，拿不到就得等 | 同步，无延迟 |

**代价**（实测）：光栅化耗时随像素**线性**增长 —— 100% 约 1 ms，200% 5.2 ms，
300% 8.4 ms，400% 30.4 ms。**400% 已超一帧预算**，所以流畅缩放的实际上限约 200–300%；
高倍下每页纹理 13–24 MiB，这让 A7（不可见页卸载纹理）更要紧。

SVG 导出**保留**（`export/svg.rs`）：它的价值在导出与将来的页内 diff，不在屏幕预览。

#### 排版与光栅化必须是两层 ★

这是上面那个决定的直接推论，也是本项目相对「每次重排 + 重出图」的关键分层：

```rust
doc: Option<Arc<PagedDocument>>   // 排版产物；缩放时**原样不动**
bitmaps: Vec<Arc<RenderImage>>    // 光栅产物；缩放时重建
zoom: f32                         // 只影响后者
```

实测（三次运行时缩放，100% → 200% → 100% → 300%）：**一次都没有重新编译**，
像素数严格 = `1.3333 × 缩放`，纹理按面积平方增长（1.5 / 6.1 / 13.6 MiB = 1× / 4× / 9×）。

> 外壳用「排版 N 次 / 光栅化 M 次」两个计数把这件事直接显示给用户 ——
> 缩放时只有后一个数在涨。可验证的声明胜过口头描述。

#### 页级增量（Plan 2 的 A7）

```rust
#[comemo::memoize]
fn render_page(page: &Page, pixel_per_pt: f32) -> Arc<Pixmap>;
```

**直接白拿增量**：`Page` 是 typst 的 comemo 兼容类型，未变化的页自动命中缓存返回**同一个** `Pixmap`。所以 v1 可以「重渲染所有页」，但实际只算了变化的页。

传给外壳时比较 `Arc::ptr_eq`，**只上传真的变了的页** —— 这才是「不重绘整本书」。

> 更激进的页内 SVG diff（tinymist 的 `"diff-v1 frame"`）留到 v2，先用页级粒度。

### 5.5 L4 · 语法服务（`typst-syntax-svc`）

只依赖 `typst-syntax`。**全部是纯函数，不持有状态、不依赖 `World`、不碰文件系统** —— 所以可以每次敲键同步跑。

```rust
/// 全量解析。纯函数。
pub fn parse(text: &str) -> SyntaxNode;

/// 高亮 token（typst-syntax 的 SyntaxKind → 语义类别）。
pub fn highlight(source: &Source) -> Vec<HighlightSpan>;

pub struct HighlightSpan {
    pub range: Range<usize>,   // 字节范围
    pub kind: TokenKind,
}

/// 标题层级 → 大纲。
pub fn outline(source: &Source) -> Vec<OutlineItem>;

/// 折叠范围（Markup / CodeBlock / ContentBlock 层级）。
pub fn folding_ranges(source: &Source) -> Vec<FoldRange>;

/// Span → 字节范围。诊断画波浪线用。
pub fn range_of_span(source: &Source, span: Span) -> Option<Range<usize>>;

/// 纯语法错误（不编译也能拿到的部分）。
pub fn syntax_diagnostics(source: &Source) -> Vec<SyntaxDiag>;
```

实现要点（照 tinymist 的做法）：

- 用 `LinkedNode` 做游标遍历（`LinkedNode::new(root)` + 迭代），不用递归下降手写。tinymist 的 `semantic_tokens.rs` 就是这个路子。
- `TokenKind` 的语义类别对齐 LSP 的 semantic token types，**但本 crate 不依赖 `lsp-types`** —— 保持零外部协议依赖，映射交给外壳。
- `ast::*` 类型化节点用于大纲（`ast::Heading`）和折叠（`SyntaxKind::{Markup, CodeBlock, ContentBlock}`），比手写模式匹配可靠。
- `range_of_span` 是 `typst::compile` 产出的 `SourceDiagnostic.span() → Range<usize>` 的桥 —— 编译诊断和语法诊断因此能用**同一套**波浪线渲染路径。

---

## 6. 数据流：一次按键

```
① 用户敲键
   │
   ├─→ [同步] syntax-svc::parse(&text)                    纯函数，无 World
   │     ├─ highlight()  → 编辑区着色
   │     ├─ outline()    → 大纲面板
   │     └─ syntax_diagnostics() → 即时语法波浪线
   │
   └─→ [异步] engine.feed_memory(path, text)
             │
             ▼
② Interrupt::Memory 入队（单写者）
   vfs.map_shadow(path, text)            ← 内存覆盖，磁盘不动
   置 CompileSignal { by_mem_events: true }
   防抖 150ms 重置
             │
             ▼
③ 防抖到期 & 不在编译中
   world.increment_revision()            ← 只作废内容变了的那几个 FileId
   SourceDb 里未变文件的 Source 缓存**原样保留**   ★ comemo 命中的关键
             │
             ▼
④ typst::compile::<PagedDocument>(&world)
   comemo 命中未变部分 → 只重算受影响的排版
   → Warned { output, warnings }
             │
             ▼
⑤ CompileSnapshot 更新
   成功 → success_doc = 新文档
   失败 → success_doc **不变**，只更新诊断            ★ 不白屏
             │
             ▼
⑥ ComputeGraph::must_get::<SvgCompute>() / <PixmapCompute>()
   #[comemo::memoize] render_page → 未变的页返回同一个 Arc
   与上一轮做 Arc::ptr_eq 比较 → **只上传变了的页**
             │
             ▼
⑦ 外壳拿到：变化的页 + 编译诊断（Span → 字节范围 → 波浪线）
```

**关键顺序**：语法服务（①同步）与编译引擎（②异步）**完全解耦** —— 高亮不等编译，编译不阻塞输入。这是「手感」的来源。

---

## 7. 测试策略

引擎的价值全在正确性与增量的**有效性**上，所以测试要能证明「真的没重算」，而不是只证明「结果对」。

### 7.1 L0 VFS

| 测试 | 断言 |
|---|---|
| overlay 覆盖磁盘 | `map_shadow` 后 `content` 返回内存内容，磁盘文件未被改动 |
| 撤销覆盖 | `unmap_shadow` 后回落到磁盘内容 |
| 覆盖不存在的路径 | 视为新文件，读取成功 |
| revision 语义 | 内容变化 → 自增；**写入相同内容 → 不自增** |
| 快照隔离 | `snapshot()` 后改原 VFS，快照内容不变 |

### 7.2 L1 World —— 最关键的一条

测的是**「有没有白做工作」**，不是「结果对不对」。两个方向：

**① 未变的文件不重新 parse**（I1）

```rust
// 改 A 后：B 的 Source 必须还是同一个 Arc（根本没碰过）
assert!(Arc::ptr_eq(&before_b, &after_b));
```

用 `Arc::ptr_eq` 而不是比较内容相等 —— 内容相等只能说明结果一样，**证明不了没白干活**。
同时用一个 `AtomicUsize` 计数 `Source::new` 的调用次数，断言编辑 A 后它**没增加**。

**② 被编辑的文件是增量重解析**（I2）—— 这条比 ① 更重要

```rust
// Source::replace 直接返回实际重解析的字节范围
let reparsed = db.feed_memory(path, &new_text);
// 在一个 3000 行文档的中间插入 1 个字符：
assert!(reparsed.len() < text.len() / 10, "重解析了 {reparsed:?}，退化成了全量");
```

`Source::replace` 的返回值让「增量是否真的生效」变成一个**可直接断言的数字**，
而不是靠跑分推测。这是本项目最重要的一条测试。

### 7.3 L2 Driver

用 tokio 测试运行时注入事件，断言：

- 连续 10 次 `Memory` 事件（间隔 < 150ms）→ **只产生 1 次**编译
- 编译中到来 3 次编辑 → 编译结束后**只补跑 1 次**，不是 3 次
- 编译失败后 `success_doc` **保持**为上一次成功文档
- 从没成功过时 `success_doc` 为 `None`，且不 panic

### 7.4 L3 Export

- 同一 revision 内二次 `must_get::<SvgCompute>()` → `Computable::compute` 调用计数**仍为 1**（用 `AtomicUsize` 计数器）
- 改一页后重算 → 只有变化页的 `Pixmap` 不是同一个 `Arc`

### 7.5 L4 Syntax

对 `tests/fixtures/*.typ` 固定输入断言**快照**：token 序列、大纲结构、折叠范围、`range_of_span` 映射。fixture 要覆盖：

- Markup / Code / ContentBlock 三种模式
- `#set` / `#show` / `#let` / `#import` / `#include`
- 公式（`$...$`）、原始文本（```` ``` ````）、注释（`//` 与 `/* */`）
- 嵌套的大括号 / 中括号 / 圆括号
- **错误输入**（未闭合括号）—— 解析器不能 panic

### 7.6 端到端

`tests/fixtures/` 里放真实 `.typ`（含 `#include` 多文件、图片、`#figure`），断言：页数、无 error 诊断、PDF 能导出。

---

## 8. 验收指标

Plan 1 落地后的状态。**实测环境**：Windows，Rust 1.98，依赖 `opt-level = 2`，
文档 = 300 段中文（34,412 字节，9 页 A4）。复现：`cargo run --example realtime`。

| # | 指标 | 测量方法 | 目标 | Plan 1 实测 |
|---|---|---|---|---|
| A1 | 未变文件不重新 parse | `Arc::ptr_eq` + `Source::new` 调用计数 | 编辑 A 后计数不增 | ✅ 10 轮编辑，构造计数不变 |
| A9 | **增量重解析真的增量** | `Source::replace` 返回的重解析范围长度 | < 全文 10% | ✅ **0.35%～0.93%**（单测 44/12680；演示 233～320/34412） |
| A2 | 导出不重算 | 调用计数器 | 同 revision 计算次数 = 1 | 🟡 **部分**。外壳已做到「文档没换就不重出图」（`Arc::ptr_eq` 判定）；`ComputeGraph` + 调用计数器的完整形式暂缓（见下方「刻意不做的事」） |
| A3 | on-type 延迟 | 连续输入 → 编译完成 | < 250ms | ✅ **平均 1.9 ms，最差 2.3 ms** |
| A4 | 增量加速比 | 单次编辑编译 vs 冷编译 | 增量 < 全量 40% | ✅ **快 102.6×**（阈值等价于 >2.5×） |
| A5 | 高亮帧预算 | 3000 行 `highlight()` 单次耗时 | ≤ 一帧 16ms | ⏳ Plan 3（`syntax-svc` 未建） |
| A6 | 失败不白屏 | 故意引入语法错误 | 预览仍有内容 + 波浪线出现 | ✅ **已进引擎**。`EngineWorld::compile()` 维护 `success_doc`；5 个测试，其中用 `Arc::ptr_eq` 断言回退的是**同一份**旧文档 |
| A7 | 无 fork | `cargo tree -i typst` | 只有 crates.io `0.15.1`，无 git 源 | ✅ `typst v0.15.1`，全图 0 个 git 源 |
| A8 | 解析器健壮 | 对随机截断的 `.typ` 跑 `parse` | 不 panic | ✅ `a_syntax_error_yields_diagnostics_not_a_panic` |

### 额外实测（不在原指标内，但直接回答了两个风险）

| 项 | 实测值 | 含义 |
|---|---|---|
| 字体冷启动（扫系统字体） | **67.6 ms** | 回答风险 **R6**。可接受，但**必须只做一次、放后台线程** |
| 冷编译（全量 parse + 全量排版） | **191 ms / 9 页** | 基线 |
| 30 次敲键累计编译 | **55.9 ms** | 同样 30 次全量重编需 **5,732 ms** |
| comemo 记忆化 | 未单独量化 | 可确认生效：增量从 191ms 降到 1.9ms，单靠重解析省不下这么多 |

> 以上是**单次运行**的样本，不是 P95。要让 A3/A4 成为稳定门槛，需跑热身后的多次统计。
> 目前这些数字足以证明**机制成立**。
>
> A5 仍是空白，不是失败 —— `syntax-svc` 还没写。
> **不把未测的写成已达标，也不把未写的写成已实现。**

### 刻意不做的事（以及理由）

实录一次「把已规划的东西砍掉」的决定，供以后翻账：

**A1/A2（actor 队列 + 防抖）暂不做。**
Plan 1 原本把它当成 L2 的核心。但实测排版只要 **0.5–2 ms**，同步执行比开线程更快
（省掉调度与状态同步），而开线程后 `EngineWorld` 必须 `Send`、产物要跳线程搬、
还要处理「编译中又来编辑」的竞态。真正需要它的条件是「**编译耗时 > 一帧**」，
而目前没有哪个真实文档能达到这个程度。

不做的不是「将来也不做」，而是「等能复现慢编译再作」—— **先写一个能复现慢编译的测试**，
再动架构。

**`ComputeGraph`（`TypeId` 键的缓存图）暂不做。**
现在没有消费者：外壳只在一处取产物，且已经用 `Arc::ptr_eq` 做到了「没换就不重出图」。
建一个没人第二次调用的缓存层是自娱。

**已经做了的最小替代**（各几行，但都是真需求）：

| 原计划 | 实际做法 | 为什么够 |
|---|---|---|
| `CompileSignal` 策略引擎 | 编辑器侧「文本没变就不排」 | gpui-component 的 `InputEvent::Change` 在光标移动/选中时也会发，这一道就够了 |
| `CompileSnapshot.success_doc` | `EngineWorld` 自持 `success_doc` | 外壳不再需要自己维护副本 |
| 页级增量（`#[comemo::memoize]`） | 失败时 `Arc::ptr_eq` 判定不重出图 | 失败重出图是真正的浪费点；"改一页只重光栅化一页"需要先有长文档 |

### 已知限制（不假装不存在）

**纹理显存未做卸载。** 高倍缩放下每页 13–24 MiB（实测 400% 为 24.2 MiB），
且当前**所有页都常驻**。100 页文档 @ 400% 约 2.4 GiB，会爆。

已做：把实际占用显示在状态栏（`N MiB`），让这个开销**可见**。
未做：只光栅化可见页 + 卸载离开视口的页 —— 这需要接入滚动句柄与视口矩形
（`ScrollHandle::offset()` + `v_virtual_list` / `on_children_prepainted`），
不是几行能了事的。当前最大的真实文档（9 页）@ 100% 只占 13.5 MiB，**不构成问题**，
所以不在无病呻吟的时机去造它。

---

## 9. 待实测风险

**R1 · 解析开销是否够快**（影响 A5，进而影响「每键高亮」是否可行）→ ✅ **已实测，风险解除**

`Source::replace` 的实际重解析范围：**0.35%～0.93%**（见 A9）。
34 KB 文档敲一个字只重解析 233～320 字节，增量机制完全成立。

> **`Arc::make_mut` 会因 comemo 持旧 `Source` 而退化成深克隆吗？**
> 实测证明**影响可忽略**：就算 `make_mut` 每次真克隆 `Lines<String>`（O(n) 内存拷贝），
> 相比全量 parse + 全量排版的 191 ms，总耗时仍只有 1.9 ms。
> 初稿把这里当成主要风险，实测表明** typst 那边根本不是瓶颈**。

**R2 · comemo 命中率**（影响 A3/A4）

comemo 是本进程内的全局缓存。反复编译**同一入口**时命中率如何、`evict` 该给多大阈值，需要实测。

> 初稿误判了一件事（已纠正）：`Source` 的 `Hash` 是**内容哈希**（`Arc<LazyHash<SourceInner>>`），
> 不是指针哈希。所以即使返回内容相同的新 `Source`，comemo **依旧会命中** —— 增量不会因此失效。
> 真正的风险只剩上面 R1 的 `make_mut` 退化，以及 comemo 缓存无上限增长。

**R3 · typst 编译不可取消**
`typst::compile` 是同步阻塞且没有取消机制。若某次编译意外变慢（大循环、复杂布局），防抖机制救不了，会卡住整个驱动。
v1 对策：编译跑在独立线程，驱动不阻塞；**不做**超时中断（typst 不支持安全中断，强杀会污染 comemo 缓存）。

**R4 · `typst-kit 0.15.1` 的包解析 API 形状未知** → ✅ **已实测解除**

查了本机 `~/.cargo/registry/.../typst-kit-0.15.1/src/packages.rs`，官方提供了 `SystemPackages`：

```rust
pub struct SystemPackages { .. }
impl SystemPackages {
    pub fn new(downloader: impl Downloader) -> Self;
    pub fn from_parts(..) -> Self;
    pub fn obtain(&self, spec: &PackageSpec) -> PackageResult<FsRoot>;
    pub fn latest_version(..);
}
```

按「数据目录 → 缓存目录 → 从 Universe 下载」的顺序解析，与 `typst` CLI 一致。配合
`downloader::SystemDownloader::new(user_agent)` 即可。**`@preview` 包支持直接进 v1**，不需要降级。

> 实现时注意：网络下载必须放到后台，且首次下载失败要能让编译带着 `FileError::Package` 正常报错，
> 不能 panic，也不能阻塞 UI 线程。

> 旁注：tinymist 仍然自研了 `crates/tinymist-world/src/package/`，那是因为它要支持多项目与自建 registry。
> 单入口文档模型用官方 `SystemPackages` 就够。

**R5 · 编译 panic 会否拖垮进程**
typst 理论上不该 panic，但真实文档 + 复杂包可能触发。对策：驱动层用 `catch_unwind` 包住编译，标记该轮失败并继续服务 —— **但要注意 comemo 内部状态被 panic 污染的可能**，需要实测确认 `catch_unwind` 后下一轮编译仍正常。

**R6 · 字体缓存的冷启动** → 🟡 **已量化：67.6 ms**

比预估的「秒级」快，但仍相当于 35 次增量编译。
**仍必须在进程生命周期内只做一次，且放后台线程**，不能塞进第一帧路径。
Plan 1 的 `embedded_and_system_fonts()` 只负责构造，调用时序由外层决定。

---

## 10. 公开 API 草图

外壳只需要看到这些东西（细节在实现时定）：

```rust
// ---- 创建 ----
let engine = Engine::builder()
    .root(project_root)
    .entry(main_typ)
    .debounce(Duration::from_millis(150))
    .build()?;

// ---- 喂输入（每次敲键）----
engine.feed_memory(main_typ, text);        // 非阻塞，入队

// ---- 收产物 ----
engine.subscribe()?;                       // 得到事件流
// EngineEvent::Compiled { revision, pages_changed, diagnostics }
// EngineEvent::Failed   { diagnostics }             ← success_doc 未变
// EngineEvent::PageUpdated { index, image: Arc<Pixmap> }

// ---- 主动取（按需）----
engine.snapshot()?.must_get::<SvgCompute>()?;
engine.export_pdf(&path)?;

// ---- 语法服务（独立 crate，同步调用）----
let node = typst_syntax_svc::parse(&text);
let spans = typst_syntax_svc::highlight(&source);
```

---

## 11. 实施顺序（供 writing-plans 参考）

按「每步都能独立验证」排序，风险最高的先做：

1. **`syntax-svc`** —— 零依赖、可立刻验证、能让外壳先转起来（且顺带回答 R1）
2. **L0 VFS** —— overlay / revision 语义，纯单测，是后面一切的地基
3. **L1 World** —— `Arc::ptr_eq` 那条测试通过即证明增量生效（回答 R2 的一半）
4. **L2 Driver** —— 防抖 / 队列 / 失败保留（回答 R5）
5. **L3 Export** —— 计算图 + 页级增量（回答 R2 的另一半、A4）
6. **端到端 + 基线测量** —— 填 A3/A4/A5 的真实阈值

**不在本仓库**：GPUI 外壳。等 1–6 跑通、指标达标后再接。

---

## 12. 变更记录

| 日期 | 变更 |
|---|---|
| 2026-09-16 | 初版。基于 tinymist 0.15.8 源码拆解，用户确认甲方案。 |
