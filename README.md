# typst-engine

Typst 的**实时增量编译引擎** —— 一个 UI 无关的 Rust 库。

输入「正在编辑中的文本」，输出「排版好的文档 / 诊断」，全程在进程内完成：

- **不依赖 tinymist**，**不依赖 typst fork** —— 编译落点就是官方的 `typst::compile`
- **不依赖 tree-sitter** —— 语法树直接来自官方 `typst-syntax`
- **支持未保存内容** —— 通过覆盖式虚拟文件系统，编译器看到的就是你正在敲的文本

## 实测

300 段中文文档（34 KB / 9 页 A4），逐字输入：

```
字体冷启动（扫系统字体）：  67.6 ms   ← 只做一次
冷编译（全量）：           191.1 ms
平均每次敲键：  重解析 276 字节（占全文 0.80%），编译 1.9 ms
对比冷编译：增量快 102.6×
30 次敲键累计 55.9 ms，而全量重编需要 5732.4 ms
```

自己跑一遍：

```bash
cargo run --example realtime
```

## 当前进度

| 层 | 内容 | 状态 |
|---|---|---|
| L0 | 覆盖式 VFS（内存盖住磁盘 + revision 语义） | ✅ 完成 |
| L1 | 增量 World（`SourceDb` / 字体 / 包 / `impl typst::World`） | ✅ 完成 |
| L2 | 编译驱动（防抖 / 队列 / `success_doc` 不白屏） | ⏳ Plan 2 |
| L3 | 导出计算图（SVG / 位图 / PDF / 页级增量） | ⏳ Plan 2 |
| L4 | 语法服务（高亮 / 大纲 / 折叠） | ⏳ Plan 3 |
| L5 | GPUI 外壳 | 不进本仓库 |

测试：**68 个全绿**（62 单测 + 6 集成），clippy 零警告。

```bash
cargo test                    # 全部
cargo clippy --all-targets    # 应该是干净的
cargo run --example realtime  # 看实时编译数据
```

## 目录

```
typst-engine/                      Cargo workspace
├── crates/engine/src/
│   ├── path_util.rs               就地消掉 `.` / `..`
│   ├── vfs/                       L0：访问抽象 / 内存 / 磁盘 / 覆盖层 / Vfs+revision
│   └── world/                     L1：QueryRef / SourceDb / EntryState / 字体 / 包 / EngineWorld
│       ├── source_db.rs           ★ 靠 Source::replace 做增量重解析
│       └── engine.rs              impl typst::World
├── crates/engine/examples/realtime.rs   可运行的实时编译演示
├── crates/engine/tests/           集成测试 + fixtures
└── docs/superpowers/              设计文档与实施计划
```

（GPUI 外壳不在本仓库 —— 引擎先独立跑通并达标，再接 UI。）

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
