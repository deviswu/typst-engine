# typst-engine

Typst 的**实时增量编译引擎** —— 一个 UI 无关的 Rust 库。

输入「正在编辑中的文本」，输出「排版好的文档 / 预览图 / 诊断」，全程在进程内完成：

- **不依赖 tinymist**，**不依赖 typst fork** —— 编译落点就是官方的 `typst::compile`
- **不依赖 tree-sitter** —— 语法树直接来自官方 `typst-syntax`
- **支持未保存内容** —— 通过虚拟文件系统的 overlay 层，编译器看到的就是你正在敲的文本

## 状态

**计划阶段（设计 + Plan 1 已写，实现未开始）**。

| 文档 | 内容 |
|---|---|
| [设计文档](docs/superpowers/specs/2026-09-16-typst-engine-design.md) | 架构、组件接口、数据流、错误处理、验收指标、待实测风险 |
| [Plan 1 实施计划](docs/superpowers/plans/2026-09-16-plan-1-vfs-and-world.md) | L0 VFS + L1 World，10 个 TDD 任务 |

后续计划：Plan 2（L2 驱动 + L3 导出，即「实时」的闭环）、Plan 3（L4 语法服务）。

设计依据来自对 [tinymist](https://github.com/Myriad-Dreamin/tinymist) 编译链的源码级拆解 —— 采用了它的
**overlay VFS + revision 失效 + comemo 记忆化 + 快照保留上次成功结果** 这几条核心思路，
但去掉了它为本项目不需要的东西（LSP、多项目锁库、浏览器目标、typst fork、自研字体与包解析）。

## 计划结构

```
typst-engine/                      Cargo workspace
├── crates/
│   ├── syntax-svc/                typst-syntax-svc —— 语法服务（只依赖 typst-syntax）
│   │                                高亮 token / 大纲 / 折叠范围 / Span→字节范围
│   └── engine/                    typst-engine —— 编译引擎（L0–L3）
│                                    overlay VFS / 增量 World / 编译驱动 / 导出计算图
└── docs/superpowers/specs/        设计文档
```

（GPUI 外壳不在本仓库 —— 引擎先独立跑通并达标，再接 UI。）

## 文档
## 许可

待定。
