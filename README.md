# typst-engine

Typst 的**实时增量编译引擎** —— 一个 UI 无关的 Rust 库。

输入「正在编辑中的文本」，输出「排版好的文档 / 诊断」，全程在进程内完成：

- **不依赖 tinymist**，**不依赖 typst fork** —— 编译落点就是官方的 `typst::compile`
- **不依赖 tree-sitter** —— 语法树直接来自官方 `typst-syntax`
- **支持未保存内容** —— 通过覆盖式虚拟文件系统，编译器看到的就是你正在敲的文本

## 功能

### Typst 编辑（与 `wu` 对齐，但全部进程内）

| 功能 | `wu` 的做法 | 本项目的做法 |
|---|---|---|
| 实时编译预览 | 外部 `typst watch` 子进程 + PDF 轮询 | **进程内** `typst::compile`，敲键即重排 |
| 编译错误波浪线 | 跑 `typst compile --diagnostic-format short` | **进程内**，而且**不编译**就能先报语法错误 |
| 代码格式化 | 要求用户先装 `typstyle` 可执行文件 | **`typstyle-core` 进程内**，无安装前置 |
| 导出 PDF | 外部 `typst compile` | **`typst-pdf` 直接吃已排版的 `PagedDocument`** |
| 字数统计 | app 内的私有逻辑 | 引擎内，**有测试**（词数规则写清楚了） |
| 源码大纲 | 正则提取 `=` 标题 | `typst-syntax` 的 `ast::Heading`，可点击跳转 |
| 手动重编译 | 重启 watch | 作废源文件缓存后重编（把外部改动吃进来） |

### 快捷键

| 快捷键 | 功能 |
|---|---|
| `Ctrl+P` | **快速跳转**：按文件名/路径片段模糊搜项目内文件，↑↓ 选、Enter 开、Esc 取消 |
| `Ctrl+S` | 保存当前文件（写盘后撤掉内存覆盖层） |
| `Ctrl+B` | 手动重新编译（作废源文件缓存，把外部改动吃进来） |
| `Ctrl+Shift+F` | 格式化（进程内 typstyle） |
| `Ctrl+E` | 导出 PDF（到同名的 `.pdf`） |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | 预览缩放 放大 / 缩小 / 100% |
| `Ctrl+滚轮` | 预览缩放（缩完把当前页锚回顶部） |
| `PageUp` / `PageDown` | 预览上一页 / 下一页 |
| `Ctrl+Home` / `Ctrl+End` | 预览首页 / 末页 |
| 点大纲条目 | 光标跳到那一行 |

### 快速跳转的打分规则

子序列匹配（字符按序出现即可，不要求连续），在此之上按「像不像用户想要的」加分：

| 加分 | 条件 | 理由 |
|---|---|---|
| +1 | 每个命中字符 | 基础分 |
| +8 | 与上一个命中相邻 | `util` 命中 `util.rs` 优于命中 `u_t_i_l.rs` |
| +6 | 命中在词边界（`/ _ - . 空格` 后，或大小写切换处） | `util` 命中 `src/util.rs` 优于命中 `src/xutil.rs` |
| +4 | 命中在**文件名**部分 | 搜 `util` 时 `src/util.rs` 应排在 `util/x.rs` 前 |

同分按路径字典序 —— 次序必须确定，否则用户会觉得列表在「乱跳」。

### 没有对齐 `wu` 的部分（不是说做不到，是判断不值得）

`wu` 还有目录树、多标签、项目搜索、快速打开、最近文件、术语表、Markdown 预览、
图片查看、**交互式终端**、**AI 诊断/对话改稿**、编辑区↔PDF 双向联动。

这些要么是重建 `wu` 的整个外壳（而本项目的价值在引擎），要么与「实时编译」这个核心无关。
把引擎做扎实、把 Typst 编辑本身做好，比再做一个 `wu` 有意义。

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
| L4 | 语法服务 | ✅ 大纲 + 波浪线（高亮仍走 tree-sitter，见下） |
| L5 | GPUI 外壳（大纲栏 · 波浪线 · 0.25×–4× 缩放） | ✅ `crates/app`（`typst-live`） |

测试：**144 个全绿**（116 引擎 + 6 集成 + 7 诊断 + 1 性能 + 14 外壳），clippy 零警告。

> **一个值得记下来的 gpui 坑**：`Window::dispatch_action` 是**从当前聚焦节点**
> 开始沿 dispatch path 上溯的（`window.rs:1992`）。如果窗口里**没有任何节点有焦点**，
> 动作派发就没有起点 —— `on_action` 的处理函数永远不会被调用，
> **所有全局快捷键静默失效**。本应用初版就中了这一条：启动后没给编辑器设焦点，
> 于是必须先点一下编辑器才能打字，而且在那之前 Ctrl+S 之类全都没反应。
> 修法是启动时 `state.focus(window, cx)`。

### L4：做了大纲与波浪线，没做高亮（附理由）

`typst_syntax::highlight()` 是现成的，但 **gpui-component 的编辑器把高亮焊死在
tree-sitter 上**（`LanguageConfig.language` 是硬字段，`SyntaxHighlighter` 是具体结构体
而非 trait），没有注入点。要用官方语法树着色就得 fork gpui-component —— 代价不成立。
所以**不写没有消费方的代码**，编辑器高亮继续走 tree-sitter。

交付的是有真实消费方的两样：**左侧大纲栏**，以及**编辑器的红/黄波浪线**
（语法错误不编译就有，编译错误同样映射成一种形状）。

一个真 bug 由此被测试揪出：大纲初版把「751 个标题 × 51 KB」算成了 O(n²)，
实测 **106 ms**（一次敲键合计 116 ms，远超一帧）。改成一次性建行首索引后 **1.1 ms**。

### 预览：位图，不是 SVG

初稿打算用 SVG 做预览，实测后否掉：一页 15cm 中文文档的 SVG 是 493 KiB，而 gpui
固定按自然尺寸 **×2** 光栅化，且纹理缓存键是**内容哈希** —— 改显示尺寸拿不到更高分辨率，
缩放必然糊，还把 1.5 MiB 的页面放大成 5.24 MiB 纹理。

改用 `typst-render` 自己出图后：DPI 完全由我们决定，而且能**同步**构造 `RenderImage`，
绕开 gpui 的异步 asset 系统。

由此确立的关键分层：**排版与光栅化是两层**。缩放只重做出图，不重新排版 ——
状态栏的「排版 N 次 / 光栅化 M 次」会直接把这件事显示出来。

### ⚠️ 光栅化必须把显示器的 DPI 缩放算进去

这条踩过坑，而且症状是「看着不清晰」这种容易被当成观感问题放过的。

**事实**：gpui 的布局用**逻辑像素**，物理像素是它的 `scale_factor` 倍
（`window.scale_factor()`）。本机面板 2560×1600、Windows 缩放 150%，
所以 `scale_factor = 1.50`。

若按 96 dpi 出图：一页 15cm = 425.2pt → 567 物理像素，
而它要占满 850 物理像素（567 逻辑 × 1.5）—— **被拉伸 1.5 倍，必然发虚**。

```
出图： pixel_per_pt = BASE(96/72) × zoom × scale_factor    → 1.33 × 1.5 = 2.00 px/pt
布局： 逻辑尺寸   = 纹理物理像素 / scale_factor              → 850 / 1.5 = 566.67
```

应用自报的实测：纹理 850×1050 物理px → 布局 566.67×700 逻辑px →
屏幕 850×1050 物理px —— **与纹理完全 1:1**。

> 注意**页面尺寸没变**：旧算法也是 850 物理像素（567 逻辑 × 1.5）。
> 所以 100% 依然是「实际大小」，变的只是纹理分辨率。
> 判断「修没修好」应该看**纹理像素与屏幕物理像素是否 1:1**，不是看字变大没变大。

另外 `RenderImage.scale_factor` 是 `pub(crate)`，我们改不了（gpui 自己的
SVG 渲染器就是靠设它来处理同一件事），所以只能在出图与布局两端自己算。

**代价**：纹理 1.5 → 3.4 MiB（1.5² = 2.25 倍），光栅化 1 → 3.5 ms。
这让高倍缩放下的显存更值得关注（400% 时每页约 54 MiB）。

**显示器切换**：窗口被拖到另一块缩放不同的屏上时，`render` 里发现
`scale_factor` 变了就重新出图 —— 否则要么糊、要么白费显存。

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
│   ├── format.rs                  进程内格式化（typstyle-core）
│   ├── export/pdf.rs              页面 → PDF（typst-pdf）
│   ├── export/pixmap.rs           页面 → 位图（屏幕预览，**DPI 可控**）
│   ├── syntax/outline.rs          文档大纲
│   ├── syntax/diagnostic.rs       语法/编译诊断 + Span → 行列
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
