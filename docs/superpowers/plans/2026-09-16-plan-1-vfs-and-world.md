# typst-engine 实施计划 · Plan 1：L0 VFS + L1 World

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 造出 `typst-engine` 的地基两层 —— 一个「内存覆盖磁盘」的虚拟文件系统，和一个「增量维护语法树」的 `typst::World` 实现，做到能把**未保存的文本**编译成排版结果，并能用数字证明增量真的生效。

**Architecture:** VFS 只做「路径 → 字节」并支持内存覆盖（2 层：`OverlayAccessModel<SystemAccessModel>`）；`FileId → 路径` 的解析归 `EntryState`；解析结果的缓存与**增量重解析**归 `SourceDb`（它不只是缓存，而是一棵被持续就地编辑的语法树，靠 typst 自带的 `Source::replace`）。三层各自可独立单测，互不知道对方存在。

**Tech Stack:** Rust edition 2024 · `typst` / `typst-syntax` / `typst-layout` / `typst-kit` 全部 `0.15.1`（crates.io 官方，**无 git fork**） · `parking_lot` · `tempfile`(dev)

**Spec:** `docs/superpowers/specs/2026-09-16-typst-engine-design.md`

## Global Constraints

- **版本**：所有 `typst*` 依赖锁 `0.15.1`，**只用 crates.io**。禁止 `[patch.crates-io]`、禁止 git 依赖。（验收 A7）
- **Rust**：edition `2024`，`rust-version = "1.92"`（与已装的 1.98 兼容）。
- **本计划的范围**：只做 L0 + L1。**不写** `driver/`（防抖与队列）、`export/`（导出计算图）、`syntax-svc`。这三块属于 Plan 2 / Plan 3。
- **不引入的依赖**：`rpds`（`Bytes` 已是 `Arc` 支撑，普通 `HashMap` 克隆就够）、`notify`（属于 L2 driver）、`comemo`（typst 自己已依赖，我们只需**间接**利用它，不直接调用）。
- **命名**：crate 名 `typst-engine`，目录 `crates/engine`。模块文件一律小写、按职责单文件。
- **错误处理**：文件读取失败一律映射成 `typst::diag::FileError`（8 个变体见 `typst-library-0.15.1/src/diag.rs`），**任何情况下不 panic**。
- **测试位置**：单元测试用同文件内联 `#[cfg(test)] mod tests`（Rust 惯例，能测私有项）；跨模块的集成测试放 `crates/engine/tests/`。
- **每个 Task 结束必须 `cargo test` 全绿 + commit。**

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `Cargo.toml` | workspace 虚拟清单 |
| `crates/engine/Cargo.toml` | `typst-engine` 包清单 |
| `crates/engine/src/lib.rs` | 公开 API 出口 |
| `crates/engine/src/vfs/mod.rs` | vfs 模块出口 |
| `crates/engine/src/vfs/access.rs` | `PathAccessModel` trait —— 「路径 → 字节」的唯一抽象 |
| `crates/engine/src/vfs/memory.rs` | `InMemoryAccessModel` —— 内存表，兼作测试底座 |
| `crates/engine/src/vfs/system.rs` | `SystemAccessModel` —— 读磁盘，错误映射 |
| `crates/engine/src/vfs/overlay.rs` | `OverlayAccessModel<M>` ★ —— 内存覆盖磁盘 |
| `crates/engine/src/vfs/vfs.rs` | `Vfs<M>` + revision 语义 ★ —— 「实时」的入口 |
| `crates/engine/src/world/mod.rs` | world 模块出口 |
| `crates/engine/src/world/query.rs` | `QueryRef<T>` —— 只算一次的格子 |
| `crates/engine/src/world/source_db.rs` | `SourceDb` ★ —— 增量维护的语法树 |
| `crates/engine/src/world/entry.rs` | `EntryState` + `resolve` —— `FileId` → 路径 + root 边界 |
| `crates/engine/src/world/fonts.rs` | `embedded_and_system_fonts()` —— 构造 typst-kit 的字体仓库 |
| `crates/engine/src/path_util.rs` | `normalize()` —— 就地消掉 `.` / `..`；`entry` 与 `packages` 共用（只此一份） |
| `crates/engine/src/world/packages.rs` | `Packages` —— typst-kit 包解析（`@preview`） |
| `crates/engine/src/world/world.rs` | `EngineWorld` + `impl typst::World` ★ |
| `crates/engine/tests/fixtures/*.typ` | 测试用 Typst 文档 |
| `crates/engine/tests/compile.rs` | 集成测试：真编译 |

**为什么 `resolve` 在 `world/` 而不在 `vfs/`**：`PathAccessModel` 的入参是 `&Path`，没有 `FileId`。若把解析做成 VFS 的一层，就得再造一个「按 FileId 索引」的 trait 与覆盖层（tinymist 的 `AccessModel` + 外层 overlay），而那一层只服务多项目与影子虚拟文件 —— 单入口模型不需要。让 **World 持有 root 并负责解析**，VFS 就退化成纯粹的「路径 → 字节 + 内存覆盖」，接口更小，也更好测。

---

## 与本计划无关的 spec 章节

以下 spec 内容**不在本计划范围**，看到时不要实现：

- §5.3 L2 编译驱动（`Interrupt` / 防抖 / `CompileSnapshot` / `success_doc`）→ Plan 2
- §5.4 L3 导出计算图（`Computable` / `TypId` 缓存 / 页级增量）→ Plan 2
- §5.5 L4 语法服务（`parse` / `highlight` / `outline` / `folding`）→ Plan 3
- §3.5 里的 `CompileSignal`、`TaskWhen`、actor 队列 → Plan 2

---

## Task 1: workspace 骨架 + `PathAccessModel` + `InMemoryAccessModel`

**Files:**
- Create: `Cargo.toml`
- Create: `crates/engine/Cargo.toml`
- Create: `crates/engine/src/lib.rs`
- Create: `crates/engine/src/vfs/mod.rs`
- Create: `crates/engine/src/vfs/access.rs`
- Create: `crates/engine/src/vfs/memory.rs`（测试内联）

**Interfaces:**
- Consumes: 无（起点）
- Produces:
  - `typst_engine::vfs::PathAccessModel` —— `fn reset(&mut self)` / `fn content(&self, src: &Path) -> FileResult<Bytes>`
  - `typst_engine::vfs::InMemoryAccessModel` —— `new() -> Self` / `insert(&mut self, path: impl Into<PathBuf>, content: Bytes)`

- [ ] **Step 1: 建 workspace 与包清单**

`Cargo.toml`（项目根）：

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
rust-version = "1.92"

[workspace.dependencies]
# 只用 crates.io 官方版本。禁止 [patch.crates-io] 与 git 依赖（验收 A7）。
typst = "0.15.1"
typst-syntax = "0.15.1"
typst-layout = "0.15.1"
typst-kit = { version = "0.15.1", features = [
    "scan-fonts",         # 扫系统字体
    "system-packages",    # 本地包目录（数据目录 + 缓存目录）
    "universe-packages",  # 从 Typst Universe 下载 @preview 包
    "system-downloader",  # 上面那条的下载器
] }
parking_lot = "0.12"
```

`crates/engine/Cargo.toml`：

```toml
[package]
name = "typst-engine"
version.workspace = true
edition.workspace = true
description = "Typst 的实时增量编译引擎（UI 无关）"

[dependencies]
typst.workspace = true
typst-syntax.workspace = true
typst-layout.workspace = true
typst-kit.workspace = true
parking_lot.workspace = true

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: 写模块出口**

`crates/engine/src/lib.rs`：

```rust
//! Typst 的实时增量编译引擎。UI 无关。
//!
//! - [`vfs`] —— 「路径 → 字节」的虚拟文件系统，支持内存覆盖磁盘
//! - [`world`] —— `typst::World` 实现，增量维护语法树、字体与包

pub mod vfs;
pub mod world;
```

`crates/engine/src/vfs/mod.rs`：

```rust
//! 虚拟文件系统：路径 → 字节，支持内存覆盖。

mod access;
mod memory;

pub use access::*;
pub use memory::*;
```

- [ ] **Step 3: 写失败的测试**

`crates/engine/src/vfs/memory.rs` —— 先只写测试，实现留空：

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::diag::FileError;
    use typst::foundations::Bytes;

    use super::*;

    #[test]
    fn reads_an_existing_entry() {
        let mut m = InMemoryAccessModel::new();
        m.insert("/a.typ", Bytes::from_string("hello".to_owned()));

        let got = m.content(Path::new("/a.typ")).unwrap();
        assert_eq!(&got[..], b"hello");
    }

    #[test]
    fn a_missing_entry_is_not_found_not_a_panic() {
        let m = InMemoryAccessModel::new();

        let err = m.content(Path::new("/nope.typ")).unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }
}
```

- [ ] **Step 4: 跑测试，确认它编译失败**

Run: `cargo test -p typst-engine reads_an_existing_entry`
Expected: 编译错误 `cannot find type InMemoryAccessModel in this scope`

- [ ] **Step 5: 写 minimal 实现**

`crates/engine/src/vfs/access.rs`：

```rust
//! 「路径 → 字节」的最小访问抽象。

use std::path::Path;

use typst::diag::FileResult;
use typst::foundations::Bytes;

pub use typst::diag::FileResult as VfsFileResult;

/// 一个能把路径读成字节的东西。
///
/// 实现者负责自己的缓存；VFS 只负责组合它们，不关心实现细节。
pub trait PathAccessModel: Send + Sync {
    /// 清空内部缓存。VFS 重置时调用。
    fn reset(&mut self) {}

    /// 读取文件内容。
    fn content(&self, src: &Path) -> FileResult<Bytes>;
}
```

`crates/engine/src/vfs/memory.rs`：

```rust
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
    // ...（Step 3 写的内容）
}
```

- [ ] **Step 6: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 2 passed`

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/engine
git commit -m "feat(vfs): workspace 骨架 + PathAccessModel + InMemoryAccessModel"
```

---

## Task 2: `SystemAccessModel`

**Files:**
- Create: `crates/engine/src/vfs/system.rs`
- Modify: `crates/engine/src/vfs/mod.rs`（加一行 `mod system; pub use system::*;`）
- Test: `crates/engine/src/vfs/system.rs`（内联）

**Interfaces:**
- Consumes: `PathAccessModel`（Task 1）
- Produces: `typst_engine::vfs::SystemAccessModel` —— `new() -> Self`，无状态（`Copy`）

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::diag::FileError;

    use super::*;

    #[test]
    fn reads_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.typ");
        std::fs::write(&path, "hello").unwrap();

        let got = SystemAccessModel::new().content(&path).unwrap();
        assert_eq!(&got[..], b"hello");
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
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine system::`
Expected: 编译错误 `cannot find type SystemAccessModel in this scope`

- [ ] **Step 3: 写实现**

```rust
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
        let data = std::fs::read(src).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => FileError::NotFound(src.to_path_buf()),
            std::io::ErrorKind::PermissionDenied => FileError::AccessDenied,
            // 目录：Linux 上 read 会给出 IsADirectory，Windows 上给出 PermissionDenied
            // 或 Other，所以这里显式判一次，保证跨平台语义一致。
            _ if src.is_dir() => FileError::IsDirectory,
            _ => FileError::Other(None),
        })?;
        Ok(Bytes::new(data))
    }
}
```

> 注：`FileError::Other` 的载荷是 `Option<EcoString>`（`typst-library-0.15.1/src/diag.rs`）。这里先给 `None`；要让错误可读需引入 `ecow` 依赖，属后续优化，本计划不做。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 5 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/vfs
git commit -m "feat(vfs): SystemAccessModel + FileError 映射"
```

---

## Task 3: `OverlayAccessModel<M>` —— 内存覆盖磁盘

**Files:**
- Create: `crates/engine/src/vfs/overlay.rs`
- Modify: `crates/engine/src/vfs/mod.rs`
- Test: `crates/engine/src/vfs/overlay.rs`（内联）

**Interfaces:**
- Consumes: `PathAccessModel`（Task 1）、`InMemoryAccessModel`（Task 1，测试用底座）
- Produces:
  - `typst_engine::vfs::OverlayAccessModel<M>` —— `new(inner: M) -> Self` / `inner() -> &M` / `inner_mut() -> &mut M`
  - `add_file(&mut self, path: impl Into<PathBuf>, content: Bytes) -> bool`（返回**内容是否真的变了**）
  - `remove_file(&mut self, path: &Path) -> bool`（返回**原本是否存在覆盖**）
  - `shadow_paths(&self) -> impl Iterator<Item = &PathBuf>`

- [ ] **Step 1: 写失败的测试**

```rust
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

    #[test]
    fn shadows_an_existing_file() {
        let mut o = OverlayAccessModel::new(inner_with("/a.typ", "disk"));
        o.add_file("/a.typ", Bytes::from_string("memory".to_owned()));

        assert_eq!(&o.content(Path::new("/a.typ")).unwrap()[..], b"memory");
    }

    #[test]
    fn removing_a_shadow_falls_back_to_the_inner_model() {
        let mut o = OverlayAccessModel::new(inner_with("/a.typ", "disk"));
        o.add_file("/a.typ", Bytes::from_string("memory".to_owned()));
        assert!(o.remove_file(Path::new("/a.typ")));

        assert_eq!(&o.content(Path::new("/a.typ")).unwrap()[..], b"disk");
    }

    /// 覆盖一个磁盘上不存在的路径 = 新建文件。
    /// 没有这条，「编辑一个还没存盘的新文件」就跑不通。
    #[test]
    fn shadows_a_path_that_does_not_exist_yet() {
        let mut o = OverlayAccessModel::new(inner_with("/a.typ", "disk"));
        o.add_file("/new.typ", Bytes::from_string("brand new".to_owned()));

        assert_eq!(&o.content(Path::new("/new.typ")).unwrap()[..], b"brand new");
    }

    #[test]
    fn a_missing_path_is_still_not_found() {
        let o = OverlayAccessModel::new(inner_with("/a.typ", "disk"));

        let err = o.content(Path::new("/nope.typ")).unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)), "got {err:?}");
    }

    /// add_file 的返回值必须反映「内容是否真的变了」。
    /// Task 4 的 revision 语义直接建立在这个返回值上。
    #[test]
    fn add_file_reports_whether_the_content_actually_changed() {
        let mut o = OverlayAccessModel::new(InMemoryAccessModel::new());

        assert!(o.add_file("/a.typ", Bytes::from_string("one".to_owned())), "首次写入算变化");
        assert!(
            !o.add_file("/a.typ", Bytes::from_string("one".to_owned())),
            "写入完全相同的内容不算变化"
        );
        assert!(o.add_file("/a.typ", Bytes::from_string("two".to_owned())), "内容不同算变化");
    }

    #[test]
    fn remove_file_reports_whether_a_shadow_existed() {
        let mut o = OverlayAccessModel::new(InMemoryAccessModel::new());

        assert!(!o.remove_file(Path::new("/a.typ")), "没有覆盖时返回 false");
        o.add_file("/a.typ", Bytes::from_string("x".to_owned()));
        assert!(o.remove_file(Path::new("/a.typ")), "有覆盖时返回 true");
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine overlay::`
Expected: 编译错误 `cannot find type OverlayAccessModel in this scope`

- [ ] **Step 3: 写实现**

```rust
//! 覆盖访问模型：用内存内容盖住下层模型的同名路径。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use typst::diag::FileResult;
use typst::foundations::Bytes;

use super::access::PathAccessModel;

/// 在下层模型之上叠一层内存覆盖。
///
/// 这是「实时编译」的地基：编辑器把未保存的文本写进覆盖层，
/// 编译器读到的就是正在编辑的内容，磁盘上的文件纹丝不动。
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
    // ...（Step 1 写的内容）
}
```

> `Bytes` 提供了 `as_slice()`，且克隆只走一次 `Arc` 引用计数（`Bytes(Arc<LazyHash<dyn Bytelike>>)`，`typst-library-0.15.1/src/foundations/bytes.rs:46`）。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 11 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/vfs
git commit -m "feat(vfs): OverlayAccessModel —— 内存覆盖磁盘"
```

---

## Task 4: `Vfs<M>` + revision 语义

**Files:**
- Create: `crates/engine/src/vfs/vfs.rs`
- Modify: `crates/engine/src/vfs/mod.rs`
- Test: `crates/engine/src/vfs/vfs.rs`（内联）

**Interfaces:**
- Consumes: `OverlayAccessModel<M>`（Task 3）、`InMemoryAccessModel`（Task 1）
- Produces:
  - `typst_engine::vfs::Vfs<M>`
  - `Vfs::new(root_content: M) -> Self`（内部自动包一层 overlay）
  - `revision(&self) -> NonZeroUsize`
  - `content(&self, path: &Path) -> FileResult<Bytes>`
  - `map_shadow(&mut self, path: impl Into<PathBuf>, content: Bytes) -> bool`
  - `unmap_shadow(&mut self, path: &Path) -> bool`
  - `snapshot(&self) -> Self`
  - `latest(&self) -> &OverlayAccessModel<M>`

> **revision 契约（照抄到代码注释里）**
> 1. 初值 `1`（`NonZeroUsize`，便于用 `Option<NonZeroUsize>` 零开销表示「还没编译过」）。
> 2. **只在内容实际变化时**自增。
> 3. 写入相同内容、撤掉不存在的覆盖 → **不自增**。

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::foundations::Bytes;

    use super::*;
    use crate::vfs::InMemoryAccessModel;

    fn bytes(s: &str) -> Bytes {
        Bytes::from_string(s.to_owned())
    }

    #[test]
    fn revision_starts_at_one() {
        let vfs = Vfs::new(InMemoryAccessModel::new());
        assert_eq!(vfs.revision().get(), 1);
    }

    #[test]
    fn shadowing_new_content_bumps_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());

        assert!(vfs.map_shadow("/a.typ", bytes("one")));
        assert_eq!(vfs.revision().get(), 2);
    }

    /// 这条是「防抖 + 无意义重编译」的地基：
    /// 编辑器重复喂同一份文本（很常见），不能一直推高 revision。
    #[test]
    fn shadowing_identical_content_does_not_bump_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        vfs.map_shadow("/a.typ", bytes("one"));
        let rev = vfs.revision();

        assert!(!vfs.map_shadow("/a.typ", bytes("one")), "内容一样，返回 false");
        assert_eq!(vfs.revision(), rev, "revision 不该动");
    }

    #[test]
    fn unmapping_a_shadow_bumps_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        vfs.map_shadow("/a.typ", bytes("one"));
        let rev = vfs.revision();

        assert!(vfs.unmap_shadow(Path::new("/a.typ")));
        assert!(vfs.revision() > rev);
    }

    #[test]
    fn unmapping_a_non_existent_shadow_does_not_bump_the_revision() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        let rev = vfs.revision();

        assert!(!vfs.unmap_shadow(Path::new("/a.typ")));
        assert_eq!(vfs.revision(), rev);
    }

    #[test]
    fn a_snapshot_is_not_affected_by_later_changes() {
        let mut vfs = Vfs::new(InMemoryAccessModel::new());
        vfs.map_shadow("/a.typ", bytes("one"));

        let snap = vfs.snapshot();
        vfs.map_shadow("/a.typ", bytes("two"));

        assert_eq!(&snap.content(Path::new("/a.typ")).unwrap()[..], b"one");
        assert_eq!(&vfs.content(Path::new("/a.typ")).unwrap()[..], b"two");
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine vfs::vfs::`
Expected: 编译错误 `cannot find type Vfs in this scope`

- [ ] **Step 3: 写实现**

```rust
//! 虚拟文件系统：把下层模型包进一层内存覆盖，并跟踪 revision。

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use typst::diag::FileResult;
use typst::foundations::Bytes;

use super::access::PathAccessModel;
use super::overlay::OverlayAccessModel;

/// 可覆盖、带 revision 的文件系统。
///
/// **revision 契约**
/// 1. 初值 `1`。
/// 2. 只在内容实际变化时自增。
/// 3. 写入相同内容、撤掉不存在的覆盖 → 不自增。
pub struct Vfs<M> {
    access: OverlayAccessModel<M>,
    revision: NonZeroUsize,
}

impl<M: PathAccessModel> Vfs<M> {
    pub fn new(inner: M) -> Self {
        Self {
            access: OverlayAccessModel::new(inner),
            revision: NonZeroUsize::MIN,
        }
    }

    pub fn revision(&self) -> NonZeroUsize {
        self.revision
    }

    /// 供 `EngineWorld` 取用底层模型（例如读磁盘）。
    pub fn latest(&self) -> &OverlayAccessModel<M> {
        &self.access
    }

    pub fn content(&self, path: &Path) -> FileResult<Bytes> {
        self.access.content(path)
    }

    /// 把编辑器里的未保存文本写进覆盖层。
    ///
    /// 返回内容是否真的变了；变了才推进 revision。
    pub fn map_shadow(&mut self, path: impl Into<PathBuf>, content: Bytes) -> bool {
        if self.access.add_file(path, content) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// 撤掉覆盖，回落到下层模型（磁盘）。
    pub fn unmap_shadow(&mut self, path: &Path) -> bool {
        if self.access.remove_file(path) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// 廉价快照：`Bytes` 是 `Arc` 支撑的，这里的克隆只是引用计数。
    pub fn snapshot(&self) -> Self {
        Self {
            access: self.access.clone(),
            revision: self.revision,
        }
    }

    fn bump(&mut self) {
        // NonZeroUsize 不会溢出：wrap 到 0 时取 MAX 继续走。
        self.revision = NonZeroUsize::new(self.revision.get().wrapping_add(1))
            .unwrap_or(NonZeroUsize::MAX);
    }
}

#[cfg(test)]
mod tests {
    // ...（Step 1 写的内容）
}
```

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 18 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/vfs
git commit -m "feat(vfs): Vfs + revision 语义（内容不变不推进）"
```

---

## Task 5: `QueryRef<T>` —— 只算一次的格子

**Files:**
- Create: `crates/engine/src/world/mod.rs`
- Create: `crates/engine/src/world/query.rs`
- Test: 内联

**Interfaces:**
- Consumes: 无
- Produces: `typst_engine::world::QueryRef<T>` ——
  `new() -> Self` / `get_or_init(&self, f) -> FileResult<T>` / `from_value(v) -> Self` /
  `peek(&self) -> Option<T>` / `get_mut_if_filled(&mut self) -> Option<&mut T>` /
  `rehydrate(&mut self)` / `is_filled(&self) -> bool`

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use typst::diag::FileError;

    use super::*;

    #[test]
    fn computes_at_most_once() {
        let cell: QueryRef<String> = QueryRef::new();
        let calls = AtomicUsize::new(0);

        for _ in 0..5 {
            let got = cell
                .get_or_init(|| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok("computed".to_owned())
                })
                .unwrap();
            assert_eq!(got, "computed");
        }

        assert_eq!(calls.load(Ordering::Relaxed), 1, "只该算一次");
    }

    /// 失败也要被记住 —— 否则每次编译都会重试同一个坏文件。
    #[test]
    fn caches_failures_too() {
        let cell: QueryRef<String> = QueryRef::new();
        let calls = AtomicUsize::new(0);

        for _ in 0..3 {
            let err = cell
                .get_or_init(|| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Err(FileError::AccessDenied)
                })
                .unwrap_err();
            assert!(matches!(err, FileError::AccessDenied));
        }

        assert_eq!(calls.load(Ordering::Relaxed), 1, "失败也只算一次");
    }

    #[test]
    fn rehydrate_recomputes() {
        let cell: QueryRef<String> = QueryRef::new();
        assert_eq!(&cell.get_or_init(|| Ok("v1".to_owned())).unwrap(), "v1");

        cell.rehydrate();
        assert_eq!(&cell.get_or_init(|| Ok("v2".to_owned())).unwrap(), "v2");
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine query::`
Expected: 编译错误 `cannot find type QueryRef in this scope`

- [ ] **Step 3: 写实现**

`crates/engine/src/world/mod.rs`：

```rust
//! Typst 世界：解析、字体、包。

mod query;

pub use query::*;
```

`crates/engine/src/world/query.rs`：

```rust
//! 一个「只算一次」的格子。

use std::sync::{Arc, OnceLock};

use typst::diag::FileResult;

/// 记忆化的取值格。
///
/// 内部是 `Arc<OnceLock<_>>`：克隆只走引用计数，多个世界快照可以共享同一份结果。
/// 我们刻意**同时缓存失败** —— 否则每次编译都会重试同一个坏文件。
pub struct QueryRef<T> {
    cell: Arc<OnceLock<FileResult<T>>>,
}

impl<T> Default for QueryRef<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for QueryRef<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
        }
    }
}

impl<T> QueryRef<T> {
    pub fn new() -> Self {
        Self {
            cell: Arc::new(OnceLock::new()),
        }
    }

    /// 取已有结果，没有就算一次并存下。
    pub fn get_or_init(&self, f: impl FnOnce() -> FileResult<T>) -> FileResult<T>
    where
        T: Clone,
    {
        self.cell.get_or_init(f).clone()
    }

    /// 丢掉已缓存的结果，下次重新算。
    pub fn rehydrate(&mut self) {
        *self = Self::new();
    }

    /// 是否已经算过。
    pub fn is_filled(&self) -> bool {
        self.cell.get().is_some()
    }

    /// 直接放进一个已算好的值。
    pub fn from_value(value: FileResult<T>) -> Self {
        let cell = Arc::new(OnceLock::new());
        let _ = cell.set(value);
        Self { cell }
    }

    /// 看一眼已有值，不算。
    pub fn peek(&self) -> Option<T>
    where
        T: Clone,
    {
        self.cell.get().cloned()
    }

    /// 若已填充且当前是唯一持有者，拿到 `&mut T` 供**就地修改**。
    ///
    /// 拿不到独占（被别的快照共享）时返回 `None`，调用方退回「重建」路径 ——
    /// 语义仍然正确，只是慢一点。
    pub fn get_mut_if_filled(&mut self) -> Option<&mut T>
    where
        T: Clone,
    {
        Arc::get_mut(&mut self.cell)?.get_mut()
    }
}

#[cfg(test)]
mod tests {
    // ...（Step 1 写的内容）
}
```

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 21 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/world
git commit -m "feat(world): QueryRef —— 只算一次且缓存失败的取值格"
```

---

## Task 6: `SourceDb` —— 增量维护的语法树 ★

**Files:**
- Create: `crates/engine/src/world/source_db.rs`
- Modify: `crates/engine/src/world/mod.rs`
- Test: 内联

**Interfaces:**
- Consumes: `QueryRef<T>`（Task 5）
- Produces: `typst_engine::world::SourceDb`

```rust
pub struct FeedOutcome {
    /// 是否新建了 Source（而非增量重解析）。
    pub created: bool,
    /// 实际重解析的字节范围。新建时为 None。
    pub reparsed: Option<Range<usize>>,
}

impl SourceDb {
    pub fn new() -> Self;
    pub fn source(&self, id: FileId, f: impl FnOnce() -> FileResult<Source>) -> FileResult<Source>;
    pub fn bytes(&self, id: FileId, f: impl FnOnce() -> FileResult<Bytes>) -> FileResult<Bytes>;
    pub fn feed_memory(&self, id: FileId, text: &str) -> FeedOutcome;
    pub fn invalidate(&self, id: FileId);
    pub fn cached_text(&self, id: FileId) -> Option<String>;
    pub fn source_arc_id(&self, id: FileId) -> Option<usize>;   // 测试用：指针身份
    pub fn construct_count(&self) -> usize;                    // 测试用：Source::new 次数
}
```

**这一条 Task 是整个项目的地基** —— `feed_memory` 走 `Source::replace` 增量重解析，而不是重建 `Source`。

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;

    use typst::foundations::Bytes;
    use typst::syntax::{FileId, RootedPath, VirtualPath, VirtualRoot};

    use super::*;

    fn fid(name: &str) -> FileId {
        RootedPath::new(VirtualRoot::Project, VirtualPath::new(name).unwrap()).intern()
    }

    fn source_for(id: FileId, text: &str) -> typst::syntax::Source {
        typst::syntax::Source::new(id, text.to_owned())
    }

    /// I1：同一份内容取两次，不该重新构造 Source。
    #[test]
    fn does_not_recompute_a_cached_source() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        let first = db.source(id, || Ok(source_for(id, "= Title"))).unwrap();
        let second = db.source(id, || Ok(source_for(id, "= Title"))).unwrap();

        assert_eq!(db.construct_count(), 1, "闭包只该跑一次");
        assert!(std::ptr::eq(first.root(), second.root()), "该是同一棵树");
    }

    #[test]
    fn caches_bytes_too() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        let a = db
            .bytes(id, || Ok(Bytes::from_string("data".to_owned())))
            .unwrap();
        let b = db
            .bytes(id, || Ok(Bytes::from_string("other".to_owned())))
            .unwrap();

        assert_eq!(&a[..], b"data");
        assert_eq!(&b[..], b"data", "第二次该拿缓存，而不是 other");
    }

    /// I2（本计划最重要的一条）：编辑走增量重解析，不是重建。
    ///
    /// 直接读 `Source::replace` 返回的重解析范围 —— 让「增量是否生效」
    /// 变成一个可以断言的数字，而不是靠跑分推测。
    #[test]
    fn feeding_memory_reparses_incrementally_not_from_scratch() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        // 一份够长的文档，好让「只重解析一小段」与「全文重解析」差别明显。
        let mut text = String::new();
        for i in 0..300 {
            text.push_str(&format!("= Heading {i}\n\nSome body text number {i}.\n\n"));
        }
        db.source(id, || Ok(source_for(id, &text))).unwrap();
        assert_eq!(db.construct_count(), 1);

        // 在中间插一个字符。
        let insert_at = text.len() / 2;
        let mut edited = text.clone();
        edited.insert(insert_at, 'X');

        let outcome = db.feed_memory(id, &edited);

        assert!(!outcome.created, "已有缓存时必须是增量重解析，不能重建");
        assert_eq!(db.construct_count(), 1, "不能调 Source::new");
        assert_eq!(db.cached_text(id).unwrap(), edited, "文本要更新");

        let reparsed = outcome.reparsed.expect("增量路径必须给出重解析范围");
        assert!(
            reparsed.len() < text.len() / 10,
            "只重解析了 {} 字节，占全文 {} 的 {}% —— 退化成了全量",
            reparsed.len(),
            text.len(),
            reparsed.len() * 100 / text.len(),
        );
    }

    /// 首次编辑（还没缓存过）必须能工作：退化成新建。
    #[test]
    fn the_first_feed_creates_the_source() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        let outcome = db.feed_memory(id, "= Hello");

        assert!(outcome.created);
        assert!(outcome.reparsed.is_none());
        assert_eq!(db.construct_count(), 1);
        assert_eq!(db.cached_text(id).unwrap(), "= Hello");
    }

    #[test]
    fn invalidate_forces_a_recompute() {
        let db = SourceDb::new();
        let id = fid("/a.typ");
        db.source(id, || Ok(source_for(id, "one"))).unwrap();

        db.invalidate(id);
        db.source(id, || Ok(source_for(id, "two"))).unwrap();

        assert_eq!(db.construct_count(), 2);
        assert_eq!(db.cached_text(id).unwrap(), "two");
    }

    #[test]
    fn invalidating_a_missing_entry_is_harmless() {
        let db = SourceDb::new();
        db.invalidate(fid("/nope.typ"));
        assert_eq!(db.construct_count(), 0);
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine source_db::`
Expected: 编译错误 `cannot find type SourceDb in this scope`

- [ ] **Step 3: 写实现**

```rust
//! 源文件数据库：一棵被持续就地编辑的语法树。
//!
//! 它不只是「带记忆化的缓存」。typst 官方文档
//! （`typst-library-0.15.1/src/lib.rs:47-55`）明确建议长驻程序保留 `Source`
//! 并用 `Source::edit` 就地修改，以换取增量性能。本模块照此实现：
//! 被编辑的文件在 `feed_memory` 里走 `Source::replace` 增量重解析。

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use typst::diag::{FileError, FileResult};
use typst::foundations::Bytes;
use typst::syntax::{FileId, Source};

use super::query::QueryRef;

/// `feed_memory` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedOutcome {
    /// 是否新建了 `Source`（而非增量重解析）。
    pub created: bool,
    /// 实际重解析的字节范围。新建时为 `None`。
    pub reparsed: Option<Range<usize>>,
}

#[derive(Default)]
struct Slot {
    source: QueryRef<Source>,
    bytes: QueryRef<Bytes>,
}

/// 源文件与资源文件的缓存。
#[derive(Default)]
pub struct SourceDb {
    slots: Mutex<HashMap<FileId, Slot>>,
    /// `Source::new` 被调用了多少次。供测试断言「没有重建」。
    constructs: AtomicUsize,
}

impl SourceDb {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取 `Source`，未缓存则用 `f` 构造。
    ///
    /// **不变式：`f` 内部不得调用 `SourceDb` 的其它方法**，否则会自死锁
    /// （下面的锁在 `f` 执行期间是持着的）。调用方需要先把自己要缓存的东西
    /// 取好，再传进来 —— `EngineWorld::source` 就是这么做的。
    pub fn source(&self, id: FileId, f: impl FnOnce() -> FileResult<Source>) -> FileResult<Source> {
        let mut slots = self.slots.lock();
        let slot = slots.entry(id).or_default();
        let constructs = &self.constructs;
        slot.source.get_or_init(|| {
            constructs.fetch_add(1, Ordering::Relaxed);
            f()
        })
    }

    /// 取 `Bytes`，未缓存则用 `f` 构造。
    ///
    /// 与 [`SourceDb::source`] 相同的锁不变式。
    pub fn bytes(&self, id: FileId, f: impl FnOnce() -> FileResult<Bytes>) -> FileResult<Bytes> {
        let mut slots = self.slots.lock();
        let slot = slots.entry(id).or_default();
        slot.bytes.get_or_init(f)
    }

    /// ★ 编辑器敲键：对已缓存的 `Source` 做增量重解析。
    ///
    /// 已有缓存 → `Source::replace`（typst 自己找最小改动并增量 reparse）；
    /// 尚无缓存 → 退化成 `Source::new`。
    pub fn feed_memory(&self, id: FileId, text: &str) -> FeedOutcome {
        let mut slots = self.slots.lock();
        let slot = slots.entry(id).or_default();

        if let Some(src) = slot.source.get_mut_if_filled() {
            let reparsed = src.replace(text);
            // 文本变了，字节缓存必须作废。
            slot.bytes.rehydrate();
            return FeedOutcome {
                created: false,
                reparsed: Some(reparsed),
            };
        }

        self.constructs.fetch_add(1, Ordering::Relaxed);
        *slot = Slot {
            source: QueryRef::from_value(Ok(Source::new(id, text.to_owned()))),
            bytes: QueryRef::new(),
        };
        FeedOutcome {
            created: true,
            reparsed: None,
        }
    }

    /// 作废单个文件。其余条目原样保留 —— 这是增量生效的关键。
    pub fn invalidate(&self, id: FileId) {
        let mut slots = self.slots.lock();
        if let Some(slot) = slots.get_mut(&id) {
            slot.source.rehydrate();
            slot.bytes.rehydrate();
        }
    }

    /// 已缓存的文本。主要给测试与上层展示用。
    pub fn cached_text(&self, id: FileId) -> Option<String> {
        let slots = self.slots.lock();
        let slot = slots.get(&id)?;
        slot.source.peek().map(|s| s.text().to_owned())
    }

    /// `Source::new` 被调用了几次。
    pub fn construct_count(&self) -> usize {
        self.constructs.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    // ...（Step 1 写的内容）
}
```

> **实现说明**：`source()` / `bytes()` 里的锁在闭包执行期间是**持着**的。这是安全的，
> 前提是遵守上面写明的锁不变式：闭包只能调 `Vfs::content`，不能再进 `SourceDb`。
> `EngineWorld::source` 会**先**把 `cached_text` 取好（它要进 `SourceDb`），再调 `self.sources.source(...)`。
>
> 另一个已修掉的语言层面问题：不能写 `let slot = slots.entry(..).or_default(); drop(slots);` ——
> `slot` 借着 `slots`，放锁会让借用失效。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 28 passed`

**如果 `feeding_memory_reparses_incrementally_not_from_scratch` 失败**，说明 `Source::replace` 的重解析范围超出预期。**不要**放宽断言的阈值来让它变绿 —— 那会把一个真实的性能问题藏起来。改为：把实际测得的范围打印出来（`assert!` 的消息里已经带了百分比），记录下来，交给 Plan 2 决定对策（例如节流，或改用 `Source::edit` 精确传入改动范围）。

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/world
git commit -m "feat(world): SourceDb —— 靠 Source::replace 做增量重解析"
```

---

## Task 7: `EntryState` + `resolve` —— `FileId` → 路径 + root 边界

**Files:**
- Create: `crates/engine/src/path_util.rs`
- Create: `crates/engine/src/world/entry.rs`
- Modify: `crates/engine/src/world/mod.rs`
- Modify: `crates/engine/src/lib.rs`（加 `mod path_util;`）
- Test: 内联

**Interfaces:**
- Consumes: 无（纯路径运算）
- Produces:
  - `typst_engine::world::EntryState` —— `new(root: impl Into<PathBuf>, main: impl AsRef<Path>) -> Self`
  - `root(&self) -> &Path` / `main(&self) -> FileId` / `main_path(&self) -> &Path`
  - `resolve(&self, id: FileId) -> FileResult<PathBuf>`（**项目根之外一律拒绝**）

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use typst::diag::FileError;
    use typst::syntax::VirtualRoot;

    use super::*;

    #[test]
    fn the_main_file_round_trips_to_its_path() {
        let entry = EntryState::new("/proj", "/proj/main.typ");

        let resolved = entry.resolve(entry.main()).unwrap();
        assert_eq!(resolved, PathBuf::from("/proj/main.typ"));
    }

    #[test]
    fn a_nested_path_resolves_under_the_root() {
        let entry = EntryState::new("/proj", "/proj/main.typ");
        let id = RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("/chapters/intro.typ").unwrap(),
        )
        .intern();

        assert_eq!(entry.resolve(id).unwrap(), PathBuf::from("/proj/chapters/intro.typ"));
    }

    /// root 边界：`..` 逃逸必须被挡住。
    /// 没有这条，一个文档就能读到 root 之外任意文件。
    #[test]
    fn escaping_the_root_is_denied() {
        let entry = EntryState::new("/proj", "/proj/main.typ");
        let id = RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("/../../etc/passwd").unwrap(),
        )
        .intern();

        let err = entry.resolve(id).unwrap_err();
        assert!(matches!(err, FileError::AccessDenied), "got {err:?}");
    }

    #[test]
    fn the_root_itself_is_allowed() {
        let entry = EntryState::new("/proj", "/proj/main.typ");
        let id = RootedPath::new(VirtualRoot::Project, VirtualPath::new("/main.typ").unwrap())
            .intern();

        assert!(entry.resolve(id).is_ok());
    }

    /// 包路径不归 entry 管，要明确拒绝而不是悄悄拼成磁盘路径。
    #[test]
    fn package_paths_are_rejected_here() {
        let entry = EntryState::new("/proj", "/proj/main.typ");
        let spec = typst::syntax::PackageSpec {
            namespace: "preview".into(),
            name: "tablex".into(),
            version: "0.0.2".parse().unwrap(),
        };
        let id = RootedPath::new(VirtualRoot::Package(spec), VirtualPath::new("/lib.typ").unwrap())
            .intern();

        let err = entry.resolve(id).unwrap_err();
        assert!(matches!(err, FileError::AccessDenied), "got {err:?}");
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine entry::`
Expected: 编译错误 `cannot find type EntryState in this scope`

- [ ] **Step 3: 写实现**

```rust
//! 入口状态：项目根、主文件，以及 `FileId` → 真实路径的解析。

use std::path::{Path, PathBuf};

use typst::diag::{FileError, FileResult};
use typst::syntax::{FileId, RootedPath, VirtualPath, VirtualRoot};

use crate::path_util::normalize;

/// 一次编译的入口信息。
///
/// 同时负责把 `FileId` 解析成磁盘路径，并强制 root 边界。
#[derive(Debug, Clone)]
pub struct EntryState {
    root: PathBuf,
    main: FileId,
    main_path: PathBuf,
}

impl EntryState {
    /// `main` 必须是 `root` 之下的真实文件路径。
    pub fn new(root: impl Into<PathBuf>, main: impl AsRef<Path>) -> Self {
        let root = normalize(&root.into());
        let main_path = normalize(&main.as_ref().to_path_buf());

        let vpath = main_path
            .strip_prefix(&root)
            .ok()
            .and_then(|rel| VirtualPath::new(format!("/{}", rel.to_string_lossy().replace('\\', "/"))).ok())
            .unwrap_or_else(|| VirtualPath::new("/main.typ").expect("static path is valid"));

        let main = RootedPath::new(VirtualRoot::Project, vpath).intern();

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

    /// 把 `FileId` 解析成真实路径。
    ///
    /// 项目根之外的路径一律 `AccessDenied`；包路径也在此拒绝 ——
    /// 它由 `Packages`（Task 10）负责，不走磁盘拼接。
    pub fn resolve(&self, id: FileId) -> FileResult<PathBuf> {
        match id.root() {
            VirtualRoot::Project => {
                let vpath = id.vpath().get_without_slash();
                let full = normalize(&self.root.join(vpath));

                // `starts_with` 按**路径组件**比较，所以
                // `/proj/../etc` 规范化成 `/etc` 后不会被误判为在 `/proj` 之内。
                if !full.starts_with(&self.root) {
                    return Err(FileError::AccessDenied);
                }
                Ok(full)
            }
            VirtualRoot::Package(_) => Err(FileError::AccessDenied),
        }
    }
}

#[cfg(test)]
mod tests {
    // ...（Step 1 写的内容）
}
```

> 需要给 `entry.rs` 的测试补两个 import：`use std::path::PathBuf;`（已在）以及
> `use typst::syntax::{RootedPath, VirtualPath, VirtualRoot};`（已在模块顶部）。测试模块里
> 直接用 `super::*` 即可拿到。
>
> 注：`PackageSpec` 的字段构造方式以 `typst-syntax-0.15.1` 为准；若字段名或
> `parse()` 的用法对不上，用 `"preview/tablex:0.0.2".parse::<PackageSpec>().unwrap()` 代替。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: `test result: ok. 33 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/world
git commit -m "feat(world): EntryState + resolve，强制 root 边界"
```

---

## Task 8: `Fonts` + `impl typst::World` —— 第一次真编译

**Files:**
- Create: `crates/engine/src/world/fonts.rs`
- Create: `crates/engine/src/world/world.rs`
- Modify: `crates/engine/src/world/mod.rs`
- Create: `crates/engine/tests/fixtures/hello.typ`
- Create: `crates/engine/tests/compile.rs`

**Interfaces:**
- Consumes: `Vfs<SystemAccessModel>`（Task 4）、`SourceDb`（Task 6）、`EntryState`（Task 7）
- Produces:
  - `typst_engine::world::embedded_and_system_fonts() -> typst_kit::fonts::FontStore`
  - `typst_engine::world::EngineWorld`
  - `EngineWorld::new(fonts: FontStore, entry: EntryState) -> Self`
  - `EngineWorld::vfs_mut(&mut self) -> &mut Vfs<SystemAccessModel>`
  - `EngineWorld::sources(&self) -> &SourceDb`
  - `impl typst::World for EngineWorld`
  - `typst_engine::world::compile(source: &Source) -> ...`？（**不做** —— 编译入口属 Plan 2；本 Task 的集成测试直接调 `typst::compile`）

- [ ] **Step 1: 写 fixture 与失败的集成测试**

`crates/engine/tests/fixtures/hello.typ`：

```typst
#set page(width: 10cm, height: 10cm)
= Hello

This is a test document. It has two pages because of the explicit page break.

#pagebreak()

= Page two

Second page body.
```

`crates/engine/tests/compile.rs`：

```rust
//! 集成测试：真的把 .typ 编译成排版结果。

use std::path::PathBuf;

use typst_engine::world::{EntryState, EngineWorld, embedded_and_system_fonts};
use typst_layout::PagedDocument;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn compiles_a_simple_document() {
    let path = fixture("hello.typ");
    let root = path.parent().unwrap().to_path_buf();

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));

    let result = typst::compile::<PagedDocument>(&world);

    assert!(
        result.output.is_ok(),
        "编译失败：{:?}",
        result.output.as_ref().unwrap_err()
    );
    let doc = result.output.unwrap();
    assert_eq!(doc.pages.len(), 2, "fixture 里有 1 个 pagebreak，应为 2 页");

    // 顺手确认 world 真的被用上了（否则上面的成功可能是假的）。
    let _ = world.vfs_mut();
}

/// 文件缺失必须是诊断，不是 panic。
#[test]
fn a_missing_main_file_is_an_error_not_a_panic() {
    let path = fixture("does-not-exist.typ");
    let root = path.parent().unwrap().to_path_buf();
    let world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));

    let result = typst::compile::<PagedDocument>(&world);

    assert!(result.output.is_err(), "缺文件应该报错");
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine --test compile`
Expected: 编译错误 `cannot find struct EngineWorld`

- [ ] **Step 3: 写 `embedded_and_system_fonts()`**

`crates/engine/src/path_util.rs`（新文件，`entry` 与 `packages` 共用）：

```rust
//! 路径工具。

use std::path::{Component, Path, PathBuf};

/// 就地消掉 `.` 与 `..`。
///
/// 不碰磁盘 —— `Path::canonicalize` 要求路径已经存在，
/// 而我们要解析的可能是**还没存盘的新文件**。
///
/// 返回值可以直接用 `starts_with(root)` 做边界检查：
/// `starts_with` 按路径组件比较，所以 `..` 被消掉后就不会再误判。
pub(crate) fn normalize(path: &Path) -> PathBuf {
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
}
```

`crates/engine/src/world/fonts.rs`：

```rust
//! 字体：内嵌字体 + 系统字体。

use typst_kit::fonts::{FontStore, embedded, system};

/// 构造一个字体仓库。
///
/// **扫系统字体是秒级操作（R6）** —— 调用方必须在后台线程构造**一次**并长期持有，
/// 绝不能放在每次编译甚至第一帧的路径上。
///
/// 返回 `FontStore` 而不是自定义包装：它已经满足 `typst::World` 的
/// `book()` 与 `font(i)` 两个方法，包一层只是多余的间接。
pub fn embedded_and_system_fonts() -> FontStore {
    let mut store = FontStore::new();
    store.extend(embedded());
    store.extend(system());
    store
}
```

> **与初稿的差异**：不再包一层自有 `Fonts` 结构。`FontStore` 本身就能满足
> `typst::World::book()` 与 `font(i)`，包一层只是多一处需要同步的间接。
> 若后续需要跨线程共享字体（可能，因为构建代价高），再考虑用 `Arc<FontStore>`，
> 而不是自定义结构。

- [ ] **Step 4: 写 `EngineWorld` 并实现 `typst::World`**

`crates/engine/src/world/world.rs`：

```rust
//! `typst::World` 实现。

use std::path::Path;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt};

use super::entry::EntryState;
use super::source_db::SourceDb;
use crate::vfs::{SystemAccessModel, Vfs};

/// 给 typst 编译器用的世界。
pub struct EngineWorld {
    library: LazyHash<Library>,
    fonts: typst_kit::fonts::FontStore,
    entry: EntryState,
    vfs: Vfs<SystemAccessModel>,
    sources: SourceDb,
}

impl EngineWorld {
    pub fn new(fonts: typst_kit::fonts::FontStore, entry: EntryState) -> Self {
        Self {
            library: LazyHash::new(Library::default()),
            fonts,
            entry,
            vfs: Vfs::new(SystemAccessModel::new()),
            sources: SourceDb::new(),
        }
    }

    pub fn vfs_mut(&mut self) -> &mut Vfs<SystemAccessModel> {
        &mut self.vfs
    }

    pub fn sources(&self) -> &SourceDb {
        &self.sources
    }

    pub fn entry(&self) -> &EntryState {
        &self.entry
    }

    /// 把 `FileId` 读成字节：项目路径走 VFS（含内存覆盖），包路径留给 Plan 3。
    fn read(&self, id: FileId) -> FileResult<Bytes> {
        let path = self.entry.resolve(id)?;
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
        // 先取：cached_text 要进 SourceDb 拿锁，必须在调 sources.source() 之前完成
        // （后者的锁在闭包执行期间是持着的 —— 见 SourceDb 的锁不变式）。
        let cached = self.sources.cached_text(id);
        let this = &*self;

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
        let this = &*self;
        self.sources.bytes(id, || this.read(id))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        // 返回 None：typst 的 datetime 会给出明确诊断，比编造一个日期安全。
        None
    }
}
```

> **设计要点**：`feed_memory`（Task 6）负责「编辑过的文件走**增量重解析**」，
> `source()` 只负责「**冷启动首次**读取」—— 两者不能各写一套文本来源，
> 否则编辑后的文本会无处可去。这里的合并方式是：`SourceDb` 里已有文本就用它，
> 否则才去读 VFS（VFS 里也可能有 `map_shadow` 过的内容）。
>
> 两条路径都指向同一份内存文本，所以 Task 9 的「未保存文本可编译」才能成立。

`crates/engine/src/world/mod.rs` 补上：

```rust
mod entry;
mod fonts;
mod source_db;
mod world;

pub use entry::*;
pub use fonts::*;
pub use source_db::*;
pub use world::*;
```

- [ ] **Step 5: 跑测试，确认通过**

Run: `cargo test -p typst-engine --test compile`
Expected: `test result: ok. 2 passed`

**这一步首次真编译，会暴露所有 API 猜错的地方**（`FontStore::book()` 的返回类型、`LibraryExt` 是否需要、`RootedPath::intern()` 的存在性等）。逐个按编译器提示修，**不要**为了绕过去而改变设计意图。

- [ ] **Step 6: 跑全量测试并 Commit**

```bash
cargo test -p typst-engine
git add crates/engine
git commit -m "feat(world): EngineWorld + impl typst::World，首次真编译通过"
```

---

## Task 9: 未保存文本进编译 ★

**Files:**
- Modify: `crates/engine/tests/compile.rs`
- Create: `crates/engine/tests/fixtures/overlay.typ`
- Test: `crates/engine/tests/compile.rs`

**Interfaces:**
- Consumes: `EngineWorld`（Task 8）、`Vfs::map_shadow`（Task 4）、`SourceDb::feed_memory`（Task 6）
- Produces: 无新 API —— 本 Task 只证明已有的三块能拼起来

**这是本计划的验收 Task**：证明「编辑器里没存盘的文本能被编译」。

- [ ] **Step 1: 写 fixture 与失败的测试**

`crates/engine/tests/fixtures/overlay.typ`：

```typst
= Disk version

One page only.
```

往 `crates/engine/tests/compile.rs` 追加：

```rust
/// ★ 本计划的核心验收：磁盘上是 1 页的文档，喂进 2 页的未保存文本，
///   编译结果必须跟着内存走，而磁盘文件纹丝不动。
#[test]
fn compiles_unsaved_text_from_the_overlay() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let disk_before = std::fs::read_to_string(&path).unwrap();

    // 内存版本：多一个 pagebreak，所以是 2 页。
    let unsaved = "\
= Memory version

Page one body.

#pagebreak()

= Second page

Page two body.
";

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root.clone(), &path));

    // ① 先按磁盘编译：1 页。
    let doc = typst::compile::<PagedDocument>(&world).output.unwrap();
    assert_eq!(doc.pages.len(), 1, "磁盘版本应该是 1 页");

    // ② 把未保存文本喂进覆盖层 + 增量重解析。
    let main = world.entry().main();
    world
        .vfs_mut()
        .map_shadow(&path, typst::foundations::Bytes::from_string(unsaved.to_owned()));
    world.sources().feed_memory(main, unsaved);

    // ③ 再编译：必须是 2 页。
    let doc = typst::compile::<PagedDocument>(&world).output.unwrap();
    assert_eq!(doc.pages.len(), 2, "编译结果该跟着内存里的未保存文本走");

    // ④ 磁盘上什么都没动。
    assert_eq!(std::fs::read_to_string(&path).unwrap(), disk_before, "绝不能写磁盘");
}

/// 编译失败不能 panic，必须给出诊断。
#[test]
fn a_syntax_error_yields_diagnostics_not_a_panic() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let broken = "#let x = (1 + \n\n= Unclosed";

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));
    let main = world.entry().main();
    world.vfs_mut().map_shadow(
        &path,
        typst::foundations::Bytes::from_string(broken.to_owned()),
    );
    world.sources().feed_memory(main, broken);

    let result = typst::compile::<PagedDocument>(&world);
    let errors = match result.output {
        Ok(_) => panic!("坏语法该报错，却编译成功了"),
        Err(errors) => errors,
    };
    assert!(!errors.is_empty(), "至少该给出一条错误诊断");
}
```

- [ ] **Step 2: 跑测试，看它是否通过**

Run: `cargo test -p typst-engine --test compile`
Expected: 若 Task 8 的 `source()` 写对了，**这两条应当直接通过**。

若 `compiles_unsaved_text_from_the_overlay` 得到 1 页而非 2 页，说明 `source()` 仍在读磁盘 —— 回到 Task 8 Step 4 的注意事项，检查 `cached_text` 是否在闭包内取值。

- [ ] **Step 3: 加一条「重复喂相同文本不推进 revision」的集成测试**

```rust
/// revision 只在内容真变时推进 —— 这是 Plan 2 防抖的地基。
#[test]
fn feeding_identical_text_does_not_bump_the_revision() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let text = "#set page(width: 5cm, height: 5cm)\nHello";

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));
    let main = world.entry().main();
    let bytes = typst::foundations::Bytes::from_string(text.to_owned());

    world.vfs_mut().map_shadow(&path, bytes.clone());
    let rev = world.vfs_mut().revision();

    assert!(!world.vfs_mut().map_shadow(&path, bytes), "相同内容不该算变化");
    assert_eq!(world.vfs_mut().revision(), rev);

    // 但 feed_memory 仍要幂等：内容一样，重解析范围可以为空。
    let outcome = world.sources().feed_memory(main, text);
    assert!(!outcome.created);
}
```

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: 全绿

- [ ] **Step 5: 记录增量实测数据**

跑一次 Task 6 的增量测试并记下真实数字，写进 spec 的 A9：

```bash
cargo test -p typst-engine feeding_memory_reparses_incrementally -- --nocapture
```

把 300 行文档插入 1 个字符时实测的「重解析字节数 / 全文字节数」填进 spec §8 的 A9 行，
并把 A9 的目标值改成**实测值 + 合理余量**（例如实测 2% → 目标定为 < 10%）。**不要留一个没测过的数字。**

- [ ] **Step 6: Commit**

```bash
git add crates/engine docs
git commit -m "test(world): 未保存文本可编译 + 增量重解析实测数据"
```

---

## Task 10: `@preview` 包解析

**Files:**
- Create: `crates/engine/src/world/packages.rs`
- Modify: `crates/engine/src/world/mod.rs`、`crates/engine/src/world/world.rs`
- Create: `crates/engine/tests/fixtures/uses_package.typ`
- Test: `crates/engine/src/world/packages.rs`（内联，不联网）

**Interfaces:**
- Consumes: `EntryState`（Task 7）
- Produces:
  - `typst_engine::world::Packages` —— `local_only()` / `with_downloader(...)`
  - `resolve(&self, spec: &PackageSpec, vpath: &VirtualPath) -> FileResult<PathBuf>`
  - `EngineWorld::read` 对 `VirtualRoot::Package` 分支接到这里

**关键**：本 Task 的测试**只用本地目录**，不联网 —— 联网测试在 CI 上必然不稳定。

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use typst::diag::FileError;
    use typst::syntax::{PackageSpec, VirtualPath};

    use super::*;

    fn spec() -> PackageSpec {
        "preview/tablex:0.0.2".parse().unwrap()
    }

    /// 在临时目录里伪造一个「包数据目录」，完全不联网。
    #[test]
    fn resolves_a_package_from_a_local_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        // 约定布局：<data>/preview/tablex/0.0.2/typst.toml
        let pkg = dir.path().join("preview/tablex/0.0.2");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("typst.toml"), "[package]\nname = \"tablex\"\n").unwrap();
        std::fs::write(pkg.join("lib.typ"), "// lib").unwrap();

        let packages = Packages::from_data_dir(dir.path());

        let got = packages
            .resolve(&spec(), &VirtualPath::new("/lib.typ").unwrap())
            .unwrap();

        assert_eq!(got, pkg.join("lib.typ"));
    }

    #[test]
    fn an_unknown_package_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let packages = Packages::from_data_dir(dir.path());

        let err = packages
            .resolve(&spec(), &VirtualPath::new("/lib.typ").unwrap())
            .unwrap_err();

        assert!(
            matches!(err, FileError::Package(_) | FileError::NotFound(_)),
            "got {err:?}"
        );
    }

    /// 包目录里的 `..` 同样要挡住。
    #[test]
    fn escaping_a_package_root_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("preview/tablex/0.0.2");
        std::fs::create_dir_all(&pkg).unwrap();

        let packages = Packages::from_data_dir(dir.path());

        let err = packages
            .resolve(&spec(), &VirtualPath::new("/../../../secret").unwrap())
            .unwrap_err();

        assert!(matches!(err, FileError::AccessDenied), "got {err:?}");
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p typst-engine packages::`
Expected: 编译错误 `cannot find type Packages in this scope`

- [ ] **Step 3: 写实现**

```rust
//! 包解析（`@preview/...`）。
//!
//! 用官方 `typst-kit::packages`，与 `typst` CLI 的查找顺序一致：
//! 数据目录 → 缓存目录 → 从 Typst Universe 下载。

use std::path::{Path, PathBuf};

use typst::diag::{FileError, FileResult};
use typst::syntax::{PackageSpec, VirtualPath};

use crate::path_util::normalize;

/// 包仓库。
pub struct Packages {
    /// 包内容所在的目录（数据目录或缓存目录）。
    data: Option<PathBuf>,
    /// 联网获取器。`None` 时只认本地目录。
    ///
    /// 刻意用 `Option`：默认不联网，让「离线也能编译本地项目」
    /// 成为默认行为，联网是显式选择。
    _downloader: Option<()>,
}

impl Packages {
    /// 只认本地目录，不联网。
    pub fn local_only() -> Self {
        Self {
            data: None,
            _downloader: None,
        }
    }

    /// 指定一个「包数据目录」。目录布局：`<data>/<ns>/<name>/<version>/…`
    pub fn from_data_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            data: Some(path.into()),
            _downloader: None,
        }
    }

    pub fn data_dir(&self) -> Option<&Path> {
        self.data.as_deref()
    }

    /// 把包里的虚拟路径解析成真实路径。
    pub fn resolve(&self, spec: &PackageSpec, vpath: &VirtualPath) -> FileResult<PathBuf> {
        let Some(data) = &self.data else {
            return Err(FileError::Package(typst::diag::PackageError::NotFound(spec.clone())));
        };

        let pkg_root = data
            .join(spec.namespace.as_str())
            .join(spec.name.as_str())
            .join(spec.version.to_string());

        if !pkg_root.is_dir() {
            return Err(FileError::Package(typst::diag::PackageError::NotFound(spec.clone())));
        }

        let full = normalize(&pkg_root.join(vpath.get_without_slash()));

        // 与 EntryState 同样的边界检查：包里也不许 `..` 逃逸。
        if !full.starts_with(&pkg_root) {
            return Err(FileError::AccessDenied);
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    // ...（Step 1 写的内容）
}
```

> `normalize` 直接复用 `crate::path_util::normalize`（Task 7 建的），**不要**再写一份。
>
> **`PackageError` 的具体变体名与 `PackageSpec` 的字段**（`namespace`/`name`/`version` 是
> `EcoString` 还是 `&str`）以 `typst-syntax-0.15.1` 为准。若 `PackageError::NotFound` 不存在，
> 用该 enum 里最接近「找不到」的变体。**这一步会暴露 API 细节，按编译器提示修即可。**

- [ ] **Step 4: 接进 `EngineWorld`**

把 `EntryState::resolve` 对 `VirtualRoot::Package` 的 `Err(AccessDenied)` 改为交由
`EngineWorld::read` 分流：

```rust
fn read(&self, id: FileId) -> FileResult<Bytes> {
    match id.root() {
        typst::syntax::VirtualRoot::Project => {
            let path = self.entry.resolve(id)?;
            self.vfs.content(&path)
        }
        typst::syntax::VirtualRoot::Package(spec) => {
            let path = self.packages.resolve(&spec, id.vpath())?;
            // 包内容也走 VFS：这样测试里能覆盖，且缓存逻辑统一。
            self.vfs.content(&path)
        }
    }
}
```

`EngineWorld::new` 增加 `packages: Packages` 字段，默认 `Packages::local_only()`。

- [ ] **Step 5: 跑测试，确认通过**

Run: `cargo test -p typst-engine`
Expected: 全绿

- [ ] **Step 6: 联网下载器不在本计划内**

`@preview` 的**联网下载**（`UniversePackages` + `SystemDownloader`）是一块独立工作：
它涉及异步、超时、缓存目录写入、下载失败的诊断传播（spec §9 R4 的注意事项），
体量不小且与「实时编译」主链路无关。

**本计划只交付离线能力**：本地包目录能解析（Step 1-5 已测），`Packages::local_only()` 为默认。
若需要联网，另开一个 Plan。

> 实现时注意：`Packages` 里的 `_downloader: Option<()>` 是为了占位并标明
> 「联网是显式选择」。若确定不做联网，把它删掉，让 `Packages` 只留 `data`。

- [ ] **Step 7: Commit**

```bash
git add crates/engine
git commit -m "feat(world): @preview 包解析（本地目录 + root 边界）"
```

---

## 完成标准

Plan 1 完成的判定（全部必须为真）：

- [ ] `cargo test -p typst-engine` 全绿，且测试数 ≥ 40
- [ ] `cargo clippy -p typst-engine -- -D warnings` 无警告
- [ ] `cargo tree -i typst` 只出现 crates.io `0.15.1`，**无 git 源**（验收 A7）
- [ ] **A1 通过**：`tests` 里有断言「编辑 A 后 `Source::new` 计数不增」
- [ ] **A9 通过且有实测数字**：`Source::replace` 的重解析范围 < 全文 10%，且 spec §8 已填入实测值
- [ ] `compiles_unsaved_text_from_the_overlay` 通过 —— **未保存文本可编译**
- [ ] 磁盘文件在整个测试过程中从未被写入
- [ ] spec 中 R4 已标为解除，A9 已填入实测数据

## 明确不在本计划内

- `driver/`（`Interrupt` / 防抖 / `CompileSnapshot` / `success_doc` 不白屏）→ **Plan 2**
- `export/`（`Computable` / `TypeId` 缓存 / SVG / 位图 / PDF / 页级增量）→ **Plan 2**
- 真实 `notify` 文件监听 → **Plan 2**
- `syntax-svc`（高亮 / 大纲 / 折叠）→ **Plan 3**
- GPUI 外壳 → **不进本仓库**
- `@preview` 的联网下载器 → 见 Task 10 Step 6
