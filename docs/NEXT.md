# 交接清单

> 写给「下次接着干」的自己/助手。项目本体在 `README.md`，设计依据在
> `docs/superpowers/specs/2026-09-16-typst-engine-design.md`。

## 一句话

`typst-engine`（UI 无关的实时增量编译引擎）+ `typst-live`（GPUI 外壳）。
敲键即重排，全程进程内，无子进程 / 无 IPC。写盘只发生在两件事上：**保存**
（`Ctrl+S`，或打字停下 1 秒的自动保存）与**导出**。
反过来，磁盘上那一份被外部改过（别的编辑器 / 脚本 / agent / `git checkout`）
会被**自动重读**（`disk_watch`，500 ms 轮询 + 200 ms 防抖），不用手点。

## 怎么跑

```bash
cargo test --workspace          # 259 个测试
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
| L10 | 界面向 `wu` 对齐：菜单条 · 目录树 · 右侧多视图（预览/Markdown/图片） | ✅ `crates/app/src/{tree,image_view,markdown_view}.rs` |
| L11 | AI 编辑（选中文字 → Ctrl+K → 逐块 diff → 应用） | ✅ `crates/app/src/{ai,diff}.rs` + 浮层接线 |
| L12 | 交互式终端（alacritty_terminal + PowerShell，Ctrl+4） | ✅ `crates/app/src/{terminal,terminal_view,term_colors}.rs` |
| L13 | 工具栏补齐 `wu` 全部条目（26 按钮 + 9 色 + AI 下拉）· 状态栏合成一行贴底 | ✅ `crates/app/src/markup.rs` 扩到 29 个条目 |
| L14 | 标签页（多文件切换，切换时重编）· 可拖动分区 · 预览适应宽度 · 编辑区去边框 | ✅ `Tab` + `h_resizable` + `zoom_fit` |
| L15 | 关窗保护（未保存改动会拦一下）· 终端 pty 回收 · 借用竞态兜底 · 日志与 panic 留痕 | ✅ `safe_update.rs` · `diag.rs` · `terminal.rs::shutdown` |
| L16 | 工程化：CI（windows-latest：fmt / clippy -D warnings / test）· LICENSE · `rust-toolchain.toml` | ✅ `.github/workflows/ci.yml` · `LICENSE-MIT/-APACHE` |
| L17 | `main.rs` 拆分（4100 行 → 2100 行，只搬不改） | ✅ `ui.rs` · `ui/{chrome,panes,overlays,util}.rs` · `preview.rs` · `jump_glue.rs` |
| L18 | 编辑辅助与显示开关：补全 · 配对括号底色 · 错误行底色 · 折叠入口 · 跟随光标 · 预览背景网格 · 大纲折叠 · 状态栏分组 | ✅ `completion.rs` · `editor_marks.rs` · `engine/src/syntax/completion.rs` · `ui/{chrome,panes}.rs` · 见 `docs/FEATURES.md` |
| L19 | 文件夹工作区（文件菜单：打开文件夹… / 最近文件夹）· 状态栏精简（主题搬进菜单、性能指标变开关）· 状态栏终端按钮 | ✅ `FolderPicker` · `tree::subdirs` · `ui/chrome.rs` |
| L23 | 终端复制/粘贴（选区实时重绘 · 四种复制键 · 四种粘贴键 · 右键一键两用 · `\n`→`\r` + 括号粘贴） | ✅ `terminal_view.rs` · `terminal::paste_bytes` |
| L22 | 删除标签页功能（标签栏 · `Tab` · 切换/关闭 · 单文档化 + 未保存拒绝换文件）· 撤 `TAB_BAR_HEIGHT` | ✅ `open_in_editor` · `load_document` |
| L21 | 括号换色（`punctuation.bracket` 从无到有；方括号补进 scm）· 顶部那一行只留标签页（撤左栏「目录」与右栏「排版预览」标题行）· 编辑区:展示区 = 1:1 | ✅ `themes::tint_brackets` · `ui/panes.rs` |
| L20 | 按用户要求做减法：删终端面板的关闭按钮 · 删「快速打开」（含 `finder.rs`）· 删「大纲」面板 · 「视图」菜单改名为「主题」且只留主题列表 | ✅ 菜单条只剩 文件 · 主题 · AI |
| L25 | 左栏目录树恢复扫描入口（空状态整块可点；以前只能靠「打开文件夹」触发，提示还指着一个已被删掉的刷新按钮）· **输入法打中文崩溃根治**（fork 上游改 wrap map 记账差一拍） | ✅ `ui/panes.rs::render_tree_body` · fork `deviswu/gpui-kit@fix/input-stale-wrap-map` |
| L26 | 底部状态栏 / 终端面板消失（`ResizablePanelGroup` 不收缩，被 A4 整页内容撑破）· 把三处「藏在本地 cargo checkout 里的库改动」搬进 fork，从此依赖写在源码里 | ✅ fork `deviswu/gpui-kit@66eb1a8a` |
| L24 | 自动保存（打字停下 1 秒写盘 · 默认开 · `Ctrl+Alt+S` 或点状态栏那格开关 · 内置示例文档永不写盘 · 与手动保存共用唯一一条写盘路径） | ✅ `autosave.rs` · `main.rs::{mark_edited,touch_autosave,autosave_now,write_to_disk,set_autosave}` |
| L27 | 磁盘上的**外部改动自动重读**（500 ms 轮询 + 200 ms 防抖，稳定了才读；本地有未保存改动时**一个字都不动**，只在状态栏说一句；只盯主文件，被 `#include` 的那些仍靠 `Ctrl+B`） | ✅ `disk_watch.rs`（纯函数 + 12 个单测）· `main.rs::{spawn_disk_watch,apply_disk_reload}` · `ui.rs`（`window.defer`）· `safe_update.rs::safe_task_read` |
| L28 | **AI 编辑能用**（用户原话：Ctrl+K 出来的功能不能用）：① 把「端点 / 模型 / Key」搬上界面（AI 菜单 → AI 设置…，之前只能手改 `settings.conf`，而没配 Key 的请求必然 401）② 没配 Key 就不发请求、开浮层就说 ③ 报错带 HTTP 状态码 + 响应原文（之前没 Key 时只报 `expected value at line 1 column 1`）④ `--ai-selftest` 一行命令验证通不通 ⑤ 作用范围扩成**选中文字 / 整篇 / 一个文件**（目录树右键 →「AI 处理…」，可处理没打开的那个文件，不必存盘再换文件） | ✅ `ai_scope.rs`（纯函数 + 3 个单测）· `ai.rs`（错误信息 + 3 个单测）· `main.rs::{open_ai_settings,open_ai_for_file,ai_scope_text,apply_ai,ai_selftest}` · `ui/overlays.rs::render_ai_settings` · `ui/chrome.rs`（AI 菜单）· `ui/panes.rs`（目录树右键） |
| L29 | AI 交互照 `wu` 的对话框来：**上下文范围**变成一排小按钮（选区 / **光标段落**（上下各 5 行，无选区时的默认）/ 全文 / **插入**），并新增**插入模式**（自取名「插入」：内容插在光标处、原文一个字不动，光标跟到新内容之后；提示词里加了 `INSERT_HINT` 说明「上下文只是位置参照，只准输出新内容」）。同时把应用后的**光标位置**还回去（以前 `set_value` 会把它清成 0，每用一次 AI 光标就跳回第一行），并加了**错位保护**（取上下文的那段文字变了就不应用，不拿旧偏移去拼） | ✅ `ai_scope.rs::{AiScope,ScopeChoice,paragraph_range,line_span,splice_at,landing}`（+12 个单测）· `main.rs::{default_ai_scope,set_ai_scope,ai_scope_choices,install_editor_text,apply_ai}` · `ui/overlays.rs`（按钮行、插入预览、阶段提示）· `ai.rs::INSERT_HINT` |

- 44 个 commit，8400 行 Rust，**279 个测试全绿**（132 引擎单测 + 6 编译集成 + 7 诊断集成
  + 12 跳转集成 + 1 性能 + 121 外壳），`cargo fmt --check` 与 `clippy -D warnings` 都干净
- 远端：`github.com/deviswu/typst-engine`（public，`master`，SSH）—— `git push` 即可
- 依赖只有 crates.io 官方 `typst 0.15.1`，无 git fork（gpui / gpui-component 是 git rev，与 `wu` 同款）

性能（3002 行 / 51 KB / 45 页）：增量重解析 0.1 ms，大纲 1.1 ms，
增量排版 9.8 ms，一次敲键合计 **11.1 ms**。

纹理：A4 单页 100% 下 7.7 MiB，**常驻 2–3 页、不随文档长度增长**
（100 页从 770 MiB 降到约 23 MiB）。

## 外壳的代码落在哪（拆分之后）

| 文件 | 管什么 |
|---|---|
| `main.rs` | `Previewer` 的状态、启动流程、菜单/快捷键接线、`recompile` |
| `ui.rs` | `impl Render for Previewer` —— 一帧怎么摆 |
| `ui/chrome.rs` · `ui/panes.rs` · `ui/overlays.rs` | 窗框 / 三块主区 / 三层浮层 |
| `ui/util.rs` | 路径缩写、占位块、波浪线端点 |
| `preview.rs` | 光栅化、纹理按视口装卸、缩放、翻页 |
| `jump_glue.rs` | 双击/单击双向跳转的外壳侧 |
| `safe_update.rs` | **常驻任务更新视图必须走这里**（见「踩过的坑」） |
| `diag.rs` | `logln!` 日志 + panic 钩子 |

⚠️ 方法跨模块调用要 `pub(crate)`：**方法的可见性跟着写 `impl` 块的那个模块走**，
不是跟着类型走。拆文件时最容易漏这一条（编译器会报 `E0624 method is private`）。

## 环境事实（换机器要重新确认）

- 显示器 2560×1600，Windows 缩放 **150%** → `window.scale_factor() = 1.5`。
  光栅化必须乘它，否则预览发虚（见 README 的 DPI 一节）
- gpui / gpui-component 钉在 git rev 上，与 `wu` / `jicheng` 同一组合：
  zed `1d217ee`、gpui-component `1505b14`
- `image` crate **必须与 gpui 的版本严格一致**（`0.25.1`），否则
  `RenderImage::new` 收的 `image::Frame` 是两个不同类型
- 日志 `%APPDATA%\typst-live\typst-live.log`，设置同目录的 `settings.conf`
- 工具链：`rust-toolchain.toml` 写 `stable`；MSRV 由 `Cargo.toml` 的
  `rust-version = "1.92"` 决定（edition 2024 + let-chains）

## 下一步（按我的推荐排序）

1. **终端还没验的部分**：键盘输入 → pty（`TerminalElement` 里那套按键编码是照搬
   `wu` 的）、鼠标选中复制、面板高度可拖。自检只验到了「pty 通、网格能读回、
   面板能画」；剩下的得人点。
2. **首次排版的过程感** —— 现在只是「推迟 + 一行提示」。177 页要 854 ms，
   可以先把第一页排出来先显示（需要把 `typst::compile` 换成按页/分段的办法，
   或者给首屏用 syntax-only 骨架）—— **先写能复现慢编译的测试再动**。
3. **下载包时的进度** —— `typst-kit` 有 `ProgressDownloader`（带回调）。
   现在只在下完之后报一条耗时，慢网下看着像卡死。
4. ~~**标签页的第二版**~~：**功能已删**（用户要求，L22）。这套「引擎入口单份就做多入口」的分析留着，将来真要多文档时能直接用。
   要让每个标签各自持有排版结果，得把引擎从「单入口」改成多入口
   （`EntryState` / `success_doc` / `SourceDb` 按文件分家）—— 这是真架构改动，
   **先量出切标签到底多慢再决定**（小文档 0–20 ms，45 页约 200 ms）。
5. **设置界面** —— 现在只有「主题」一个下拉框；缩放/上次文件/包源都只能手改
   `settings.conf`。另外可以考虑把「打开的文件」列成最近文件。

想做的还有（都小）：索引改按页惰性建（22 ms → 0.5 ms 级，**只有真感觉到卡顿才做**）、
双击只跳转不选词（一行 `stop_propagation`）、主题列表改成读 `themes/` 目录
（`ThemeRegistry::watch_dir`，现在只在启动时抓一次）、
工具栏按钮的自定义（现在写死在 `Markup::ALL`）。

**最近这一轮做完的（不要再捡）**：关窗保护 + 终端 pty 回收、借用竞态兜底
（`safe_update`）、终端泵退出条件、`line_col` 字符边界、设置原子写盘、去掉
`~/.pi` 隐式凭据回退、`windows_subsystem` + 日志文件 + panic 钩子、`main.rs` 拆分、
CI、LICENSE、`rust-toolchain.toml`、`recompile` 只读一次编辑器内容。

**再上一轮（UI/UX 那一批，见 `docs/FEATURES.md`）**：补全（`CompletionProvider` 钩子 +
引擎给候选）、配对括号底色与错误行底色（`DocumentColorProvider` 钩子，走 `bg_segments`）、
错误面板与状态栏错误徽标可点跳转、代码折叠的键盘/菜单入口、跟随光标（轮询 + 静音期）、
预览背景纯色/网格、大纲折叠与 padding 缩进、状态栏三组分隔、工具栏 gap/高度/分隔条、
AI 入口去重、目录树后台扫描与空目录提示。

**已知限制（不是 bug，是两个钩子的固有限制，别再当 bug 修）**：
`DocumentColorProvider` 只在**文本变化**后重新要颜色（库的 `_pending_update`），
所以「配对括号随光标移动实时变色」做不到；折叠箭头**只在悬停行号列/光标行/已折叠时**才画。
两者要改都得起一个 gpui-component fork —— 已列入 `docs/FEATURES.md` 的「不做」。

## 已论证「不做」（别重新捡起来）

| 不做 | 理由 |
|---|---|
| actor 队列 + 防抖（Plan 2 的 A1/A2） | 排版只要 0.5–2 ms，同步比开线程更快。**真要做的前提是能写出复现慢编译的测试** —— 先写测试再动架构 |
| `ComputeGraph`（`TypeId` 键缓存） | 没有第二次调用它的消费者。真正的浪费点已用 `Arc::ptr_eq` 挡掉 |
| 独立 `syntax-svc` crate | 隔离理由不成立：消费方必须链 engine 才能拿到增量维护的语法树 |
| 用 typst 官方语法树做编辑器高亮 | gpui-component 的 `SyntaxHighlighter` 是具体结构体且内部持 tree-sitter 树，**没有注入点**。要换得 fork gpui-component |
| 目录树 / 多标签 / 终端 / AI | 那是重建 `wu` 的整个外壳；本项目的价值在引擎（**现状：外壳已经有了，但这是后来的判断，不必回退**） |
| 纹理视口卸载 —— **已做**，从「不做」移出了 | A4 单页 7.7 MiB × 100 页 = 770 MiB，确实会爆 |

## 踩过的坑（已修，但容易重犯）

| 症状 | 根因 | 在哪 |
|---|---|---|
| 预览永久空白 | gpui 的 `ImageSource::Image` 走 `use_asset`，**异步解码**，第一次必然拿不到 | 改用同步构造 `RenderImage` |
| **所有快捷键静默失效** | `dispatch_action` 从**聚焦节点**开始派发；窗口没焦点就没有起点 | 启动时 `state.focus()` |
| 预览发虚 | 光栅化没乘显示器 DPI 缩放（150% 屏上差 1.5 倍像素） | 出图 × scale_factor，布局 ÷ scale_factor |
| **外部改动落不进编辑器**（重读后预览还是旧的，或光标跳回第一行） | `InputState::set_value` 是**静默路径**（不发 `Change` 事件），重排得自己发；它还会把光标与滚动位置清成 0 —— 而 `set_cursor_position` 会 `focus`，焦点在终端上时那一下就是把键盘抢走 | `apply_disk_reload`：自己 `recompile`；焦点在编辑器里才还光标，否则只还 `set_scroll_offset` |
| **开机就闪一句「已重新加载」** | 轮询第一拍会把「我们自己刚读进来的文件」当成外部改动（启动时的指纹是 `None`） | `Previewer::new` 里就把 `disk_stamp` 记上 |
| **Ctrl+K 出来的 AI 不能用** | 没配 Key（`settings.conf` 里没有 `ai_api_key`、环境变量也没有）→ 每次请求 401；而服务端的 401 响应体是**纯文本**（`Authentication Fails (governor)`），旧代码直接丢给 `serde` → 报的是一句 `expected value at line 1 column 1`。界面上又没有任何填 Key 的地方（只能手改 `settings.conf`，但没人告诉你） | ① AI 菜单 →「AI 设置…」三个框 ② 没 Key 不开请求、开浮层就说 ③ 报错带 HTTP 状态码 + 响应原文 ④ `--ai-selftest` |
| **滚轮滚不动** | 页框没设 `flex_shrink_0`，被 flex 压进视口 → 容器永不溢出 | 页框加 `.flex_shrink_0()` |
| **预览滚过之后，双击位置整页偏** | `ScrollHandle::bounds_for_item` 给的是**内容坐标**（不是窗口坐标），漏加滚动偏移 | 加回 `offset`；正反两个方向共用一对互逆函数 |
| 中文文档建跳转索引时 panic | `Glyph.span` 里那个 `u16` 偏移可能落在字符中间 | 对齐到字符边界取整个字符 |
| （同类，09-17 补）诊断的 `line_col` 也会被同一个偏移咬 | 它只夹了 `min(len)`，没对齐边界 —— `text[..end]` 在汉字中间直接 panic | `syntax/diagnostic.rs::floor_char_boundary` |
| 跳转到错的行 | 排版失败时预览是旧结果而源码已变，字节偏移对不上 | 编译失败期间禁用跳转 |
| 大纲 106 ms（超一帧） | `line_of` 每个标题都从文本开头数换行 = O(n²) | 一次性建行首索引 + 二分 |
| **窗口每开一次往下爬 11px** | 存的是 `window.bounds()`（客户区）而不是 `window_bounds()` | 存后者（下次开窗用的就是那一份几何） |
| **换了主题、磁盘上没写** | 「现在想要的」与「已经存盘的那份」混用一个字段，「变了才写」的比较永远相等 | 分开存（`theme_name` vs `settings.theme`） |
| `#[test]` 报 recursion limit | 文件里 `use gpui::*` 把 gpui 自己的 `#[gpui::test]`（名字就叫 `test`）带进来了 | 那个文件只引要用的类型，不要通配符 |
| 工具栏插入后预览不更新 | gpui-component 的 `insert` / `replace` 走**静默**路径（不发 `InputEvent::Change`） | 自己 `recompile()`（`format_document` 同一条） |
| `@preview` 那行正文报 label 不存在 | 正文里的 `@preview` 被当成**标签引用**（`@name` 是引用语法） | 正文要写 `\@preview` 或换个说法 |
| 取包卡住好几秒没反应 | 下载是**同步阻塞**在排版中间的 | 每次都把取包耗时打印出来（冷取 452 ms / 热取 0.1 ms） |
| `Button::new(("a", "b"))` 编译不过 | `ElementId` 只接受 `&str` / `(&str, EntityId)` 这类，**不接受** `(&str, &str)` | 用 `&str` 当 id（同一栏里标签本来就唯一） |
| `.selected(bool)` 找不到方法 | 它在 `gpui_component::Selectable` trait 上，不在 `Button` 上 | 把 `Selectable as _` 导进来 |
| 目录树点一下又展开又收起 | `Tree` 在外层包了一个 `mouse_down` 自己调 `toggle_expand` | 应用侧的回调里只处理「打开文件」，目录直接 return |
| **AI 改一句，整篇行尾从 CRLF 变成 LF** | `apply_hunks` 重建时写死了 `\n` | 行尾跟着原文走（`old.contains("\r\n")`） |
| **点了「拒绝」，文件还是被改了** | 重建时无条件补了个结尾换行 | 结尾换行也跟原文一致；「全拒绝」必须逐字节等于原文（有测试） |
| AI 请求把界面卡住（设置防抖/终端轮询一起停摆） | 阻塞调用丢进 gpui 的 background_executor 池 | 放 `std::thread`，界面只按 100ms 轮询回执 |
| 报「curl 退出码 7」看不出该怎么办 | 没翻成人话 | 7→连不上端点、28→超时、35→TLS、60→证书 |
| 浅色主题下终端里的黄字几乎看不见 | ANSI 经典固定色是为深色底挑的 | 跟随主题换调色板 + `ensure_contrast` 只调亮度 |
| `Button.xsmall()` 编译不过 | 这个 gpui-component rev 的 Sizable 走 `with_size(Size::Small)` | 用 `with_size` |
| 构造里 `root` 被 `EntryState::new` 移走后再用 | `PathBuf` 不是 Copy | 重新从 `main_path.parent()` 算一次 |
| 两个按钮用同一个 `ElementId` 会撞 | 工具栏的「图片」与右侧页签的「图片」同名 | 工具栏 id 加前缀 `tb:` |
| 编到一半报「拒绝访问 typst-live.exe」 | 上一次 `cargo run` 的窗口还开着，文件被占 | `taskkill /F /IM typst-live.exe` 再编 |
| **按钮文字溢到邻居身上（看着像重叠）** | flex 行默认 `flex-shrink: 1`，容器一窄就把子项压得比文字还窄 | 每个按钮/指标加 `flex_shrink_0`（与页框那次同一个坑，这次是**全应用**补） |
| 工具栏挤成一团、还盖到预览上 | 工具栏挂在 520px 宽的编辑区列里，26 个按钮必然溢出 | 工具栏提到外层 v_flex，**全宽一条** |
| 点标签的 `×` 会把标签也切一下 | 点击事件冒泡到外层标签（它也有 on_click） | 子元素里先 `cx.stop_propagation()` |
| `no method named on_click found for Div` | gpui 的 `on_click` 在 `StatefulInteractiveElement` 上，元素得有 `id` | 先 `.id(("tab", index))` |
| `ResizablePanelGroup` 上不能 `.flex_1()` | 它没实现 `Styled`，但自己的 render 里已经 `size_full + flex_1` | 直接当 flex 子项放 |
| **分区拖窄后编辑区盖到侧栏上** | 三件事凑一起：面板没有 `size_range` 下限、内容 `min-width:auto` 不肯变小、外层没 `overflow_hidden` | 三样都补上（下限 300px + `min_w_0` + `overflow_hidden`） |
| 右键示例文档报波浪线 | 我写成了 Markdown 的 `**粗体**`，Typst 是 `*...*` | — |
| **「一打字进程就没了」（exit 0xc0000409）** | gpui 的 Windows 后端会在**已持有 `App` 借用**时派发就绪的前台任务；常驻循环里的 `weak.update` 撞上就是 `already borrowed`，panic 在 wndproc 里展开不了 → fastfail。中文输入法（嵌套消息循环）命中率极高 | **凡「任务里更新视图」一律走 `safe_update::safe_task_update`**（`Busy` 等下一拍重试，其它 panic 原样抛） |
| **又是「一打字进程就没了」（同一个 0xc0000409，但根因不同）** | 这次第一条 panic 不是借用竞态，而是 gpui-component `input/element.rs:1281` 的 `debug_assert_eq!`：输入法合成中文时 wrap map 记的行长比真实文本**差一拍**（实测 `left: 1, right: 3` = 1 字节拼音 vs 3 字节汉字）。panic 在 wndproc 里展开不了，展开途中再有任务借 `App` → 第二条 panic → fastfail。**release 也躲不掉**：断言后面的 `line_text[range]` 拿脏区间切片同样 panic（汉字还会踩非字符边界） | 只能改库 → **fork 上游**（`longbridge/gpui-component` 已改名 `gpui-kit`）：`deviswu/gpui-kit` 分支 `fix/input-stale-wrap-map`，基线就是上游 `1505b148`、只改那一处（行长不一致时按整行单段排版，不用脏区间切片）。`crates/app/Cargo.toml` 那两行指 fork，注释写了理由与「上游合并后换回官方 rev」 |
| **底部状态栏 / 终端面板整条不见了**（`Ctrl+4` 叫不出来、右下角也没了那个「终端」按钮） | `ResizablePanelGroup` 只写了 `size_full()`（即 width/height:100%）：在 taffy 里「父尺寸由 flex 分配」时那个百分比解析不了，`flex-basis:auto` 退回内容尺寸 → panel 被内容（一页 A4 高）撑破，排在它后面的 shell 与状态栏被**挤出窗口**（最大化也没用）。而这个修复当时只活在**本地被就地改过的 cargo checkout** 里 | 修进 fork：`deviswu/gpui-kit@66eb1a8a` 带上了 `.flex_1() + .min_h_0() + .min_w_0()`（连同行号列宽、prepaint `text_style` 两处）。**但真正的病根是“依赖 checkout 里的幽灵改动”**：cargo 不校验 checkout，改了就长期生效，换机器 / CI / 新 rev 就静默丢失 —— 底下那三个文件全部搬进 fork 就是为了拆掉这颗雷 |
| **本地 cargo checkout 被改过（三处 + 两个图标）** | 2026-08-28 调 `jicheng` 时为了试布局修直接改了 `~/.cargo/git/checkouts/gpui-component-*/1505b14/`（`resizable/panel.rs` / `input/element.rs` 两处 / 手加的 `paperclip.svg`·`send.svg`），从未入版本库 | typst-live 已经把需要的都搬进 fork（不再依赖它）；**那个 checkout 没动** —— wu / jicheng 还在靠它。要彻底干净，得把同样几处也带进那两个项目的 fork，或者等上游合并 |
| 终端泵关窗后还在空转 | `_ = view.update(...)` 把 `Err` 吞了，循环永不退出；子进程退出后也不 break | 走 `safe_entity_update`，`Gone` 就 `break`；`ChildExit` 也 `break` |
| 关窗丢字 | 编辑器从不写盘，而窗口没有任何关闭钩子 | `Window::on_window_should_close` + 浮层（保存/放弃/取消）。**注意**：早先注释里写「这个 rev 没有可靠的关闭钩子」是错的，`1d217ee` 就有（`window.rs:5362`） |
| 终端子进程关窗后还活着 | `Terminal::shutdown` 写了但没人调 | 关窗路径调它（`Msg::Shutdown` → `EventLoop` 退出 → ConPTY 析构 → 子进程结束） |
| 发布版双击带一个控制台黑框 | 没写 `windows_subsystem`（而且是**发布版**才有感觉） | `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`；诊断改走 `logln!`（同时进日志文件） |
| 设置文件被写坏一半 | 直接 `fs::write` 覆盖原文件，写一半崩就截断 | 同目录 tmp + `rename` 原子替换（有测试断言不留 `.tmp`） |
| 拆文件后一片 `E0624 method is private` | 方法的可见性跟着**写 `impl` 的模块**走 | 跨模块调用的方法标 `pub(crate)`；自由函数同理 |

**子代理（AgentShell）在这台机器上能不能用**（2026-09-16 实测，别再重复试）：

| agent_type | 结果 |
|---|---|
| `claude_code` | ✗ `[WinError 2]` —— 没装这个 CLI |
| `pi` | ✗ 同一个错（虽然 `pi` 在 PATH 里，AgentShell 仍拉不起来） |
| `codex` | **装是装了**，但 Windows 下非 ASCII 参数传不进去：`invalid UTF-8 was detected in one or more arguments` |

结论：**派活时 prompt 必须写成纯 ASCII**（codex 那条就能用），或者干脆自己干。
终端（1260 行）与 AI 模块（原计划外包）最后都是自己做的。

**移植大模块的省力办法**（终端那 1260 行就是这么搬的）：同一套
gpui / gpui-component rev 下，别读代码再重写 —— `cp` 过来、加 `mod` 声明、
让 `cargo check` 报错，按错误逐个补（只差一个 `crate::theme`）。
只有「本项目没有对应模块」的地方才需要真改。

**拆分大文件的办法**（这次 `main.rs` 4100 → 2100 就是这么搬的）：按名字 + 括号/缩进
定位每个 `fn` 的区间（方法以「恰好 4 空格 + `}`」结束），把 `///` 文档注释一起带走，
搬完包一层 `impl Previewer { … }`（方法的可见性跟着 impl 所在的模块走，跨模块调用要
`pub(crate)`），然后让编译器把缺的 import 一条条报出来。**纯搬移不改逻辑**，
所以「测试数不变」就是这次拆分没搞坏东西的证据。

**通用教训**：多窗口桌面上截图对比不可靠（会被别的窗口挡住/干扰），
**程序自报的数字才可信**（现在这些数字都落 `typst-live.log`）。

## 验证手法备忘

- gpui **不接受注入的键盘/滚轮输入**（SendKeys 无效）。要做端到端验证，
  在 `render()` 里用临时静态计数器 + `window.dispatch_action(...)` 派发
  （记得把临时块放在 `let theme = cx.theme()` **之前**，否则和 theme 的
  不可变借用冲突）。用完删干净。
- 更进一步：**按帧号推进的自检**。在 `render()` 开头按 `FRAME.fetch_add(1)` 分步
  做事（第 3 帧造光条件、第 5 帧报结果、第 7 帧 `cx.quit()`），能直接把真代码路径
  走一遍。两个必要条件：① 每一步末尾要 `cx.notify()`，否则帧不会往前走、自检停在半路；
  ② 自己 `cx.quit()` 关窗，不然它开在那儿不走了。跳转那两个方向就是这样验的。
- 验证坐标类逻辑时，**让断言用一条独立算式**（而不是复用被测函数）：
  “目标行现在画在窗口 y=142.7px，期望 142.7px” 才揪出了那个
  “内容坐标 / 窗口坐标” 的 bug；把同一个函数算两遍是抓不到错的。
- 读剪贴板式的窗口截图：先 `SetProcessDpiAwarenessContext(-4)` 再
  `CopyFromScreen`，否则拿到的是被系统缩过的逻辑分辨率，看不出清晰度差异。
- **想给这个应用的窗口拍照，别指望自动化**（2026-09-16 实测）：
  - 全屏 `CopyFromScreen` 会拍到**盖在上面的别的窗口**（那次拍到的是浏览器）
  - `SetForegroundWindow` 对后台进程**被系统挡掉**，提不到前台
  - `PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT)` 对 gpui 这种 GPU 合成窗口
    **只能拿到空图**（PNG 10KB）
  - PowerShell 脚本要**纯 ASCII**：本机 PowerShell 按 ANSI 读脚本，中文串会把
    语法打断（`字符串缺少终止符`）
  - 结论：UI 观感类改动只能**请人看一眼**；能自动化的部分改成「核对结构 + 让程序
    报数字」（例如断言工具栏是顶层子项、按钮数、flex_shrink_0 的处数）
- **关窗流程可以无人化验证**（09-17 实测，不用点鼠标）：`PostMessage(hwnd, 0x0010)`
  走的就是 `on_window_should_close` 那条路 —— 与用户点 × 同一条。配上两个临时
  env 钩子（`new()` 里把 `dirty` 置真；`render()` 里用 `window.window_handle()`
  + 定时任务自动点浮层按钮），四条路径能一次跑全：
  「有改动 → 拦下」、`discard` → 关闭、`save` → 写盘后关闭、无改动 → 直接关；
  再把目标文件设成只读，就能验「保存失败就别关」（实测：进程仍活着，字没丢）。
  验完把钩子删干净（`grep TL_TEST` 应为空）。
  ⚠️ 两个坑：① 在 `render()` 里调 `cx.notify()` **或** `window.refresh()` 都**不会**
  带来下一帧（本帧末尾才清 dirty、绘制中 refresh 是空操作），要发个定时任务或
  `cx.spawn` 才推得动；② `AnyWindowHandle::update` 的闭包是
  `(AnyView, &mut Window, &mut App)` —— **三个参数**，第一个是根视图。
