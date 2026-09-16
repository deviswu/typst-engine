# typst-engine

Typst 的**实时增量编译引擎** —— 一个 UI 无关的 Rust 库。

输入「正在编辑中的文本」，输出「排版好的文档 / 诊断」，全程在进程内完成：

- **不依赖 tinymist**，**不依赖 typst fork** —— 编译落点就是官方的 `typst::compile`
- **不依赖 tree-sitter** —— 语法树直接来自官方 `typst-syntax`
- **支持未保存内容** —— 通过覆盖式虚拟文件系统，编译器看到的就是你正在敲的文本

## 跑起来看

```bash
cargo run -p typst-live                # 开窗，用内置示例文档
cargo run -p typst-live -- doc.typ     # 开窗，打开指定文件
```

左边是带 Typst 语法高亮的编辑器，右边跟着实时更新。状态栏那几个数字就是
「实时」的证据：排版耗时 / 光栅化耗时 / 重解析字节 / 页数 / 缩放 / 排版与光栅化次数。

*Ctrl+=* 放大、*Ctrl+-* 缩小、*Ctrl+0* 回到 100%。

```bash
cargo run --example realtime   # 不开窗，在终端里跑逐字输入的性能数据
```

## 实测

300 段中文文档（34 KB / 9 页 A4），逐字输入：

```
字体冷启动（扫系统字体）：  67.6 ms   ← 只做一次
冷编译（全量）：           191.1 ms
平均每次敲键：  重解析 276 字节（占全文 0.80%），编译 1.9 ms
对比冷编译：增量快 102.6×
30 次敲键累计 55.9 ms，而全量重编需要 5732.4 ms
```

## 当前进度

| 层 | 内容 | 状态 |
|---|---|---|
| L0 | 覆盖式 VFS（内存盖住磁盘 + revision 语义） | ✅ 完成 |
| L1 | 增量 World（`SourceDb` / 字体 / 包 / `impl typst::World`） | ✅ 完成 |
| L2 | 编译驱动 | 🟡 **只做了最小版**：`success_doc` 失败保留已进引擎、「文本没变不重排」；actor 队列与防抖**刻意不做**（同步只要 0.5–2 ms，见 spec「刻意不做的事」） |
| L3 | 导出：SVG 页 + **位图（可控 DPI / 缩放）** 已通 | 🟡 部分 |
| L4 | 语法服务（高亮 / 大纲 / 折叠） | ⏳ Plan 3（编辑器高亮目前走 tree-sitter） |
| L5 | GPUI 外壳（含 0.25×–4× 缩放） | ✅ `crates/app`（`typst-live`） |

测试：**82 个全绿**（76 单测 + 6 集成），clippy 零警告。

### 预览：位图，不是 SVG

初稿打算用 SVG 做预览，实测后否掉：一页 15cm 中文文档的 SVG 是 493 KiB，而 gpui
固定按自然尺寸 **×2** 光栅化，且纹理缓存键是**内容哈希** —— 改显示尺寸拿不到更高分辨率，
缩放必然糊，还把 1.5 MiB 的页面放大成 5.24 MiB 纹理。

改用 `typst-render` 自己出图后：100% 时纹理 1.5 MiB（降 3.5×），400% 依然锐利，
而且能**同步**构造 `RenderImage`，绕开 gpui 的异步 asset 系统。

由此确立的关键分层：**排版与光栅化是两层**。缩放只重做出图，不重新排版 ——
状态栏的「排版 N 次 / 光栅化 M 次」会直接把这件事显示出来。

```bash
cargo test                    # 全部
cargo clippy --all-targets    # 应该是干净的
cargo run --example realtime  # 终端的逐字输入性能数据
cargo run -p typst-live       # 开窗的实时预览器
```

## 目录

```
typst-engine/                      Cargo workspace
├── crates/engine/src/
│   ├── path_util.rs               就地消掉 `.` / `..`
│   ├── export/svg.rs              页面 → SVG（导出 / 将来的页内 diff）
│   ├── export/pixmap.rs           页面 → 位图（屏幕预览，**DPI 可控**）
│   ├── vfs/                       L0：访问抽象 / 内存 / 磁盘 / 覆盖层 / Vfs+revision
│   └── world/                     L1：QueryRef / SourceDb / EntryState / 字体 / 包 / EngineWorld
│       ├── source_db.rs           ★ 靠 Source::replace 做增量重解析
│       └── engine.rs              impl typst::World
├── crates/app/src/main.rs         L5：GPUI 外壳（typst-live）
├── crates/engine/examples/realtime.rs   终端里的性能演示
├── crates/engine/tests/           集成测试 + fixtures
└── docs/superpowers/              设计文档与实施计划
```

## 文档

| 文档 | 内容 |
|---|---|
| [设计文档](docs/superpowers/specs/2026-09-16-typst-engine-design.md) | 架构、接口、数据流、验收指标（含实测数据）、风险 |
| [Plan 1 实施计划](docs/superpowers/plans/2026-09-16-plan-1-vfs-and-world.md) | L0 VFS + L1 World，10 个 TDD 任务 |

## 设计依据

来自对 [tinymist](https://github.com/Myriad-Dreamin/tinymist) 编译链的**源码级拆解**。
采用了它的 overlay VFS、revision 失效、comemo 记忆化、`success_doc` 失败保留、
`TypeId` 键导出计算图、actor 单写者队列；砍掉了它为本项目不需要的东西
（LSP、多项目锁库、浏览器目标、typst fork、自研字体与包解析、`rpds`）。

其中三条最关键的结论是实测官方源码得出的，与直觉相反：

1. **`typst::compile` 就是全部** —— tinymist 的 `typst-shim` 只包了一个 `SYNTAX_ONLY` 开关
2. **fork 只为分析层** —— 编译路径完全不需要它
3. **typst 自带增量重解析**（`Source::replace` / `edit`）—— 这是「实时」的正解，
   而且它**返回实际重解析范围**，让增量有效性变成可直接断言的数字

## 许可

待定。
