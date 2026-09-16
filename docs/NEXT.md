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
| L10 | 界面向 `wu` 对齐：菜单条 · 目录树 · 右侧多视图（预览/Markdown/图片） | ✅ `crates/app/src/{tree,image_view,markdown_view}.rs` |
| L11 | AI 编辑（选中文字 → Ctrl+K → 逐块 diff → 应用） | ✅ `crates/app/src/{ai,diff}.rs` + 浮层接线 |
| L12 | 交互式终端（alacritty_terminal + PowerShell，Ctrl+4） | ✅ `crates/app/src/{terminal,terminal_view,term_colors}.rs` |
| L13 | 工具栏补齐 `wu` 全部条目（26 按钮 + 9 色 + AI 下拉）· 状态栏合成一行贴底 | ✅ `crates/app/src/markup.rs` 扩到 29 个条目 |
| L14 | 标签页（多文件切换，切换时重编）· 可拖动分区 · 预览适应宽度 · 编辑区去边框 | ✅ `Tab` + `h_resizable` + `zoom_fit` |

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

0. **标签页的第二版**（等真有体感再说）：现在切标签**重新编译**（引擎入口是单份的）。
   要让每个标签各自持有排版结果，得把引擎从「单入口」改成多入口
   （`EntryState` / `success_doc` / `SourceDb` 按文件分家）—— 这是真架构改动，
   **先量出切标签到底多慢再决定**（小文档 0–20 ms，45 页约 200 ms）。
1. **终端还没验的部分**：键盘输入 → pty（`TerminalElement` 里那套按键编码是照搬
   `wu` 的）、鼠标选中复制、面板高度可拖。自检只验到了「pty 通、网格能读回、
   面板能画」；剩下的得人点。
1. **首次排版的过程感**
2. **首次排版的过程感**（承接上一条）—— 现在只是「推迟 + 一行提示」。177 页要 854 ms，
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
| 右键示例文档报波浪线 | 我写成了 Markdown 的 `**粗体**`，Typst 是 `*...*` | — |

**子代理（AgentShell）在这台机器上能不能用**（2026-09-16 实测，别再重复试）：

| agent_type | 结果 |
|---|---|
| `claude_code` | ✗ `[WinError 2]` —— 没装这个 CLI |
| `pi` | ✗ 同一个错（虽然 `pi` 在 PATH 里，AgentShell 仍拉不起来） |
| `codex` | **装是装了**，但 Windows 下非 ASCII 参数传不进去：`invalid UTF-8 was detected in one or more arguments` |

结论：**派活时 prompt 必须写成纯 ASCII**（codex 那条就能用），或者干脆自己干。
本次终端（1260 行）与 AI 模块（原计划外包）最后都是自己做的。

**移植大模块的省力办法**（这次终端的 1260 行就是这么搬的）：同一套
gpui / gpui-component rev 下，别读代码再重写 —— `cp` 过来、加 `mod` 声明、
让 `cargo check` 报错，按错误逐个补（这次只差一个 `crate::theme`，把它需要的
几个函数按**括号配平**从对方文件里摘出来成单独模块就完事了）。
只有「本项目没有对应模块」的地方（如 `crate::log`）才需要真改。

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
- **想给这个应用的窗口拍照，别指望自动化**（2026-09-16 实测）：
  - 全屏 `CopyFromScreen` 会拍到**盖在上面的别的窗口**（那次拍到的是浏览器）
  - `SetForegroundWindow` 对后台进程**被系统挡掉**，提不到前台
  - `PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT)` 对 gpui 这种 GPU 合成窗口
    **只能拿到空图**（PNG 10KB）
  - PowerShell 脚本要**纯 ASCII**：本机 PowerShell 按 ANSI 读脚本，中文串会把
    语法打断（`字符串缺少终止符`）
  - 结论：UI 观感类改动只能**请人看一眼**；能自动化的部分改成「核对结构 + 让程序
    报数字」（例如断言工具栏是顶层子项、按钮数、flex_shrink_0 的处数）
