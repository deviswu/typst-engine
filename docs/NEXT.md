# 交接清单

> 写给「下次接着干」的自己/助手。项目本体在 `README.md`，设计依据在
> `docs/superpowers/specs/2026-09-16-typst-engine-design.md`。

## 一句话

`typst-engine`（UI 无关的实时增量编译引擎）+ `typst-live`（GPUI 外壳）。
敲键即重排，全程进程内，无子进程 / 无 IPC / 无存盘。

## 怎么跑

```bash
cargo test --workspace          # 144 个测试
cargo run -p typst-live         # 开窗（内置示例文档）
cargo run -p typst-live -- doc.typ
cargo run --example realtime    # 终端的逐字输入性能数据
```

## 当前状态（均已验证，数字是实测不是估算）

| 层 | 内容 | 状态 |
|---|---|---|
| L0 | 覆盖式 VFS（内存盖住磁盘 + revision 语义） | ✅ |
| L1 | 增量 World（SourceDb / 字体 / 包 / `impl typst::World`） | ✅ |
| L2 | 编译驱动 | 🟡 只有 `success_doc` 失败保留 + 「文本没变不重排」 |
| L3 | 导出：SVG / 位图（可控 DPI）/ PDF | ✅ |
| L4 | 语法服务：大纲 / 诊断 / 字数 | ✅（高亮仍走 tree-sitter） |
| L5 | GPUI 外壳 | ✅ |
| L6 | 跳转索引（源码字节 ⇄ 页/页内 pt） | ✅ `crates/engine/src/jump.rs` + 外壳双击接入 |
| L7 | 外壳体验：设置持久化 · 链接可点 · 主题切换 · 工具栏 | ✅ `crates/app/src/{settings,themes,coords,markup}.rs` |
| L8 | `@preview` 联网取包 | ✅ `Packages::with_downloads()`（typst-kit 的 SystemPackages） |
| L9 | 长文档首屏（首次排版推迟到开窗之后） | ✅ 窗口先出来，预览区显示「首次排版中…」 |

- 28 个 commit，8000 行 Rust（36 个 `.rs` 文件），clippy 与 `cargo fmt` 都干净
- 远端：`github.com/deviswu/typst-engine`（public，`master`，SSH）—— `git push` 即可
- 远端：`github.com/deviswu/typst-engine`（public，`master`，SSH）—— 已推完，`git push` 即可
- 依赖只有 crates.io 官方 `typst 0.15.1`，无 git fork

性能（3002 行 / 51 KB / 45 页）：增量重解析 0.1 ms，大纲 1.1 ms，
增量排版 9.8 ms，一次敲键合计 **11.1 ms**。

纹理：A4 单页 100% 下 7.7 MiB，**常驻 2–3 页、不随文档长度增长**
（100 页从 770 MiB 降到约 23 MiB）。

## 环境事实（换机器要重新确认）

- 显示器 2560×1600，Windows 缩放 **150%** → `window.scale_factor() = 1.5`。
  光栅化必须乘它，否则预览发虚（见 README 的 DPI 一节）
- gpui / gpui-component 钉在 git rev 上，与 `wu` / `jicheng` 同一组合：
  zed `1d217ee`、gpui-component `1505b14`
- `image` crate **必须与 gpui 的版本严格一致**（`0.25.1`），否则
  `RenderImage::new` 收的 `image::Frame` 是两个不同类型

## 下一步（按我的推荐排序）

1. **首次排版的过程感** —— 现在只是「推迟 + 一行提示」。177 页要 854 ms，
   可以先把第一页排出来先显示（需要把 `typst::compile` 换成按页/分段的办法，
   或者给首屏用 syntax-only 骨架）—— **先写能复现慢编译的测试再动**。
2. **下载包时的进度** —— `typst-kit` 有 `ProgressDownloader`（带回调）。
   现在只在下完之后报一条耗时，慢网下看着像卡死。
3. **设置界面** —— 现在只有「主题」一个下拉框；缩放/上次文件/包源都只能手改
   `settings.conf`。另外可以考虑把「打开的文件」列成最近文件。

想做的还有（都小）：索引改按页惰性建（22 ms → 0.5 ms 级，**只有真感觉到卡顿才做**）、
双击只跳转不选词（一行 `stop_propagation`）、主题列表改成读 `themes/` 目录
（`ThemeRegistry::watch_dir`，现在只在启动时抓一次）、
工具栏按钮的自定义（现在写死在 `Markup::ALL`）。

刚做完（不要再提）：坐标换算单测、设置持久化、预览链接可点、主题切换、
工具栏、长文档首屏、`@preview` 联网取包。

## 已论证「不做」（别重新捡起来）

| 不做 | 理由 |
|---|---|
| actor 队列 + 防抖（Plan 2 的 A1/A2） | 排版只要 0.5–2 ms，同步比开线程更快。**真要做的前提是能写出复现慢编译的测试** —— 先写测试再动架构 |
| `ComputeGraph`（`TypeId` 键缓存） | 没有第二次调用它的消费者。真正的浪费点已用 `Arc::ptr_eq` 挡掉 |
| 独立 `syntax-svc` crate | 隔离理由不成立：消费方必须链 engine 才能拿到增量维护的语法树 |
| 用 typst 官方语法树做编辑器高亮 | gpui-component 的 `SyntaxHighlighter` 是具体结构体且内部持 tree-sitter 树，**没有注入点**。要换得 fork gpui-component |
| 目录树 / 多标签 / 终端 / AI | 那是重建 `wu` 的整个外壳；本项目的价值在引擎 |
| 纹理视口卸载 —— **已做**，从「不做」移出了 | A4 单页 7.7 MiB × 100 页 = 770 MiB，确实会爆 |

## 踩过的坑（已修，但容易重犯）

| 症状 | 根因 | 在哪 |
|---|---|---|
| 预览永久空白 | gpui 的 `ImageSource::Image` 走 `use_asset`，**异步解码**，第一次必然拿不到 | 改用同步构造 `RenderImage` |
| **所有快捷键静默失效** | `dispatch_action` 从**聚焦节点**开始派发；窗口没焦点就没有起点 | 启动时 `state.focus()` |
| 预览发虚 | 光栅化没乘显示器 DPI 缩放（150% 屏上差 1.5 倍像素） | 出图 × scale_factor，布局 ÷ scale_factor |
| **滚轮滚不动** | 页框没设 `flex_shrink_0`，被 flex 压进视口 → 容器永不溢出 | 页框加 `.flex_shrink_0()` |
| **预览滚过之后，双击位置整页偏** | `ScrollHandle::bounds_for_item` 给的是**内容坐标**（不是窗口坐标），漏加滚动偏移 | 加回 `offset`；正反两个方向共用一对互逆函数 |
| 中文文档建跳转索引时 panic | `Glyph.span` 里那个 `u16` 偏移可能落在字符中间 | 对齐到字符边界取整个字符 |
| 跳转到错的行 | 排版失败时预览是旧结果而源码已变，字节偏移对不上 | 编译失败期间禁用跳转 |
| 大纲 106 ms（超一帧） | `line_of` 每个标题都从文本开头数换行 = O(n²) | 一次性建行首索引 + 二分 |
| **窗口每开一次往下爬 11px** | 存的是 `window.bounds()`（客户区）而不是 `window_bounds()` | 存后者（下次开窗用的就是那一份几何） |
| **换了主题、磁盘上没写** | 「现在想要的」与「已经存盘的那份」混用一个字段，「变了才写」的比较永远相等 | 分开存（`theme_name` vs `settings.theme`） |
| `#[test]` 报 recursion limit | 文件里 `use gpui::*` 把 gpui 自己的 `#[gpui::test]`（名字就叫 `test`）带进来了 | 那个文件只引要用的类型，不要通配符 |
| 工具栏插入后预览不更新 | gpui-component 的 `insert` / `replace` 走**静默**路径（不发 `InputEvent::Change`） | 自己 `recompile()`（`format_document` 同一条） |
| `@preview` 那行正文报 label 不存在 | 正文里的 `@preview` 被当成**标签引用**（`@name` 是引用语法） | 正文要写 `\@preview` 或换个说法 |
| 取包卡住好几秒没反应 | 下载是**同步阻塞**在排版中间的 | 每次都把取包耗时打印出来（冷取 452 ms / 热取 0.1 ms） |
| 右键示例文档报波浪线 | 我写成了 Markdown 的 `**粗体**`，Typst 是 `*...*` | — |

**通用教训**：多窗口桌面上截图对比不可靠（会被别的窗口挡住/干扰），
**程序自报的数字才可信**。上面前三条都是靠加日志/让程序报数才定位的。

## 验证手法备忘

- gpui **不接受注入的键盘/滚轮输入**（SendKeys 无效）。要做端到端验证，
  在 `render()` 里用临时静态计数器 + `window.dispatch_action(...)` 派发
  （记得把临时块放在 `let theme = cx.theme()` **之前**，否则和 theme 的
  不可变借用冲突）。用完删干净。
- 更进一步：**按帧号推进的自检**。在 `render()` 开头按 `FRAME.fetch_add(1)` 分步
  做事（第 3 帧造光条件、第 5 帧报结果、第 7 帧 `cx.quit()`），能直接把真代码路径
  走一遍。两个必要条件：① 每一步末尾要 `cx.notify()`，否则帧不会往前走、自检停在半路；
  ② 自己 `cx.quit()` 关窗，不然它开在那儿不走了。跳转那两个方向就是这样验的（见 README）。
- 验证坐标类逻辑时，**让断言用一条独立算式**（而不是复用被测函数）：
  “目标行现在画在窗口 y=142.7px，期望 142.7px” 才揪出了那个
  “内容坐标 / 窗口坐标” 的 bug；把同一个函数算两遍是抓不到错的。
- 读剪贴板式的窗口截图：先 `SetProcessDpiAwarenessContext(-4)` 再
  `CopyFromScreen`，否则拿到的是被系统缩过的逻辑分辨率，看不出清晰度差异。
