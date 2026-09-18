# 能力清单：已实现 / 缺失 / 明确不做

**这份文档是给「来提改进意见的人（或 Agent）」看的。** 提之前先在这里查一遍：
问「这个编辑器有没有 X」时，答案多半已经在下面了 —— 两张「看截图猜功能」
的评审清单里，17 条有 8 条是这里已经实现的东西，或者描述的界面元素根本不存在。

维护规则：

- **已实现**要写清**入口**（快捷键 / 菜单 / 鼠标动作）与**代码位置** ——
  只说「实现了」等于没说，评审没法验证，就会当成「没有」再提一遍。
- **明确不做**必须写**理由**。没有理由的「不做」下一个人会重新捡起来。
- 从「缺失」搬到「已实现」时，顺手在 `NEXT.md` 的 L 表里加一行。

---

## 一、已实现（附入口与位置）

### 排版与性能

| 能力 | 入口 | 位置 |
| --- | --- | --- |
| 增量排版（敲键即重排） | 直接打字 | `crates/engine`（L0/L1/L2） |
| 全量排版耗时（177 页 ~854ms） | 状态栏 `排版 xx.xms` | `NEXT.md` 有实测数字 |
| 纹理按视口惰性出图 | 自动 | `ui.rs` / `preview.rs` |
| 长文档首屏先出窗口 | 启动自动 | L9 |
| 缩放：`Ctrl+=` / `Ctrl+-` / `Ctrl+0` / `Ctrl+滚轮` | 菜单「视图 → 放大/缩小/适应宽度」 | `ui.rs` `set_zoom*` |
| 翻页：`PageUp` / `PageDown` / `Ctrl+Home` / `Ctrl+End` | 菜单亦可 | `ui.rs` |
| 状态栏（**默认精简**）：错误徽标 · 页码/缩放 · 字数/标题/保存状态 | 底部状态栏；错误徽标**可点**（跳到第一条错误）；**保存状态那格也可点**（切自动保存） | `ui/chrome.rs::render_statusbar` |
| 性能指标（排版/光栅化/索引耗时、重解析字节、纹理 MiB、三个计数） | 「视图 → 显示性能指标」开 | 同上 |
| 终端面板 | 状态栏「终端」按钮 或 `Ctrl+4` | `toggle_shell` |
| 工作区目录（目录树根） | 「文件 → 打开文件夹…」（应用内浮层，逐层点） | `FolderPicker` |
| 最近文件夹 | 「文件 → 最近文件夹」（最近 8 个；目录没了自动摘掉） | `recent_dirs` |

### 跳转（双向）

| 能力 | 入口 | 位置 |
| --- | --- | --- |
| 正向：源码 → 预览 | **双击编辑区**；或 `Ctrl+Alt+J`；或菜单「跳转 → 跳到预览」 | `jump_glue.rs` |
| 反向：预览 → 源码 | **单击预览页**（跟着点击位置定位到最近的字） | `jump_glue.rs` |
| 跟随光标（光标停下自动跟） | 菜单「视图 → 跟随光标」/ `Ctrl+Alt+L`；手动滚预览会静默 3 秒 | `main.rs::spawn_follow_poll` |
| 跳转高亮 | 自动（跳过去闪一个框，1.4s 淡出） | `ui.rs` flash |
| 大纲点击跳转 | 左栏「大纲」页签 | `ui/panes.rs` |
| 预览里点链接 | `Ctrl+单击` | `ui.rs` |

**实现方式不是 SyncTeX**：直接读排版结果里的**字形出处**（每个字形来自哪些源码字节），
所以不需要辅助文件、不改文档，也不要求先编译出 PDF。详见 `README.md`
「双击双向跳转」一节。

### 编辑器

| 能力 | 入口 | 位置 |
| --- | --- | --- |
| Typst 语法高亮 | 自动（tree-sitter + `typst-highlights.scm`） | `crates/app/src/typst-highlights.scm` |
| 括号/引号配对高亮 | 自动（**底色**，按嵌套深度三档配色；光标所在那对更亮） | `editor_marks.rs` |
| 错误行整行底色 | 自动（错误红 / 警告黄，很淡的背景色） | `editor_marks.rs` |
| 语法错误波浪线（不用编译就有） | 自动 | `engine/src/syntax/diagnostic.rs` |
| 编译错误波浪线（与语法错误同一形状） | 自动 | 同上 |
| 错误列表（可点，点了跳到那一行） | 右栏下方红底区域 | `ui/panes.rs::render_errors` |
| 状态栏错误徽标（可点，跳到第一条错误） | 底部 `✗ N 错误` | `ui/chrome.rs` |
| 代码折叠 | **鼠标移到行号列**，箭头出现，点击折叠；开关在菜单「视图 → 代码折叠」/ `Ctrl+Alt+F` | 库自带（`InputState::set_folding`） |
| 补全（IntelliSense） | 打 `#` 之后继续敲字母，下拉出现；`#` 后为空会给全部候选 | `completion.rs` + `engine/src/syntax/completion.rs` |
| 补全候选来源 | ① `#let` / `#import` 定义的名字（本文档）② Typst 关键字 ③ `Library::default()` 的全部内置名字 | 同上 |
| 查找文件 | `Ctrl+P` | `finder.rs` |
| **自动保存**（打字停下 1 秒后写盘，默认**开**） | 开关：`Ctrl+Alt+S` 或**点状态栏那一格**（`● 待自动保存` / `已自动保存`）；设置键 `autosave` | `autosave.rs` · `main.rs::{touch_autosave,write_to_disk}` |
| 自动保存的三条守卫 | ① 开关关着不写 ② **内置示例文档永远不写**（它的路径在 `%TEMP%`，不是用户的文件，与开关无关）③ 文本与磁盘上那份比较，一样就不重写（打字又撤回去不会白写） | `autosave.rs::should_save`（纯函数，四态有单测） |
| 自动保存失败怎么办 | `dirty` 保持为 true、状态栏一条「保存失败」，**不重试**（只读文件上重试就是每秒刷屏）；下次编辑重新计时再试 | `main.rs::autosave_now` |
| **外部改动自动重读**（别的编辑器 / 脚本 / agent / `git checkout` 写了当前这个文件） | 自动：500 ms 一拍查指纹（长度 + mtime），变化后等 200 ms 稳定了才读；装好之后状态栏一句「已重新加载…（第 N 次）」。**不需要快捷键，也不会动手输入的东西** | `disk_watch.rs` · `main.rs::{spawn_disk_watch,apply_disk_reload}` · `safe_update.rs::safe_task_read` |
| 外部改动时不重读的两种情况 | ① **本地有未保存改动**：一个字都不动，状态栏一句「磁盘上被改过 —— 你有未保存的修改，没有重载（保存后以你为准）」（保存后提示自动消失）② 内置示例文档（它不是用户的文件）。文件被删 / 读不动也一样只提示 | `disk_watch::verdict`（纯函数，三态有单测） |
| 重读时保护什么 | 光标按**行列**还回去（越界夹住）、视图滚回原处；被 `#include` 进来的外部文件一并作废重读（等于顺手做了一次 `Ctrl+B`） | `main.rs::apply_disk_reload` |
| 重读的边界 | 只盯**当前打开的主文件**。被 `#include` 的文件被外部改了，仍靠 `Ctrl+B`（重新编译 = 源缓存全部作废重读） | 同上 |
| ~~多标签页~~ | **已删**（用户要求）：只有一份文档，换文件=换内容，有未保存改动会拒绝 | — |

### 界面与交互

| 能力 | 入口 | 位置 |
| --- | --- | --- |
| 三分区可拖动 | 拖分栏线（左栏 / 编辑区 / 右栏） | `h_resizable` |
| 主题切换（含深色） | **「视图 → 主题」菜单**（原来在状态栏右下角的下拉里），选项来自 `ThemeRegistry`，切换后写进设置记住 | `themes.rs` · `ui/chrome.rs` |
| 预览背景：纯色 / 网格 | 菜单「视图 → 预览背景：纯色 / 网格」 | `ui.rs::paint_grid` |
| 预览头只读状态（`第 1/9 页 · 充满 56%`） | 看即可 | `ui/panes.rs::viewport_label` |
| 大纲：层级缩进 + 层级配色 + 折叠展开 | 左栏「大纲」：点标题跳转、点 ▾/▸ 折叠 | `ui/panes.rs` |
| 目录树（后台扫描、空目录给提示） | 左栏；**空状态整块可点 = 扫描当前目录**（否则就先去「文件 → 打开文件夹…」） | `main.rs::refresh_tree` · `ui/panes.rs::render_tree_body` |
| 右侧多视图（预览 / Markdown / 图片） | 按打开的文件类型自动切 | `markdown_view.rs` / `image_view.rs` |
| 工具栏（26 按钮 + 9 色下拉，按组分隔；**末尾多一个「录屏」按钮** —— 录制中变红带计时） | 顶部第二行 | `markup.rs` · `ui/chrome.rs::render_record_button` |
| 交互式终端 | `Ctrl+4` | `terminal.rs` |
| **录屏**：只录本软件窗口那一块 + 麦克风（不是整屏） | 工具栏「录屏」/ 菜单「视图 → 开始录屏」/ `Ctrl+Alt+R`；成品落 `D:\录屏\typst-live-<年月日-时分秒>.mp4` | `record.rs`（`crop_region` / `screen_args` / `camera_args` / `concat_args` / `overlay_args` 纯函数 + 16 个单测）· `main.rs::{capture_region,start_recording,stop_recording}` |
| 录屏**暂停 / 继续**（暂停 = 把这一段收干净，继续 = 开新的一段；停止后 `-c copy` 无损拼接 —— 成品里**没有**暂停那段） | 工具栏「⏸ 暂停」/ 菜单「视图 → 暂停录」/ `Ctrl+Alt+P` | `record.rs::{pause,resume}` · `Finalize::run` |
| 录屏**画中画**（摄像头单独录一路文件，停止后合成到右下角） | 菜单「视图 → 录摄像头画中画」（默认开） | `record.rs::overlay_args` · `nvenc_available` |
| 录屏自检（一行命令端到端：开窗 → 录 → 中途暂停/继续 → 收尾 → 打印成品路径） | `cargo run -p typst-live -- --rec-selftest 8` | `main.rs::REC_SELFTEST` · `ui.rs`（自测那一段） |
| 面板与两条栏的显隐（**录「干净画面」靠它**：录出来的画面就是窗口本身） | 菜单「视图 → 显示目录 / 显示编辑区 / 显示展示区 / 显示工具栏 / 显示状态栏」（勾选，随设置持久化；三块面板全关时中间给一句恢复提示） | `ui/chrome.rs::render_main_area` · `main.rs::set_pane_visible` · `settings.rs`（+7 项） |
| AI 编辑（选中 → `Ctrl+K` → 逐块 diff → 应用） | 菜单「AI」/ `Ctrl+K` | `ai.rs` / `diff.rs` |
| AI 编辑的**上下文范围**（浮层里一排小按钮，点一下就换） | ① **选区**（有选区时才有这个按钮）② **光标段落**（上下各 5 行，`Ctrl+K` 无选区时的默认）③ **全文** ④ **插入**（上下文也取光标段落，但结果**插在光标处、原文不动**，光标跟到新内容之后） | `ai_scope.rs`（`paragraph_range` / `splice_at` / `landing`，纯函数，15 个单测）· `main.rs::{default_ai_scope,set_ai_scope,ai_scope_text,apply_ai}` · `ui/overlays.rs`（按钮行 + 插入预览） |
| AI 应用结果时做什么 | 编辑器内（选区/段落/全文/插入）：`set_value` + **把光标放回应在的位置**（否则 gpui-component 会清成 0，每用一次 AI 光标就跳回第一行）→ 置脏 → 重排；文件：写盘 + 源缓存作废 + 重排 | `main.rs::{apply_ai,install_editor_text}` |
| AI 应用前的错位保护 | 取上下文时那一整段文字到应用前又变了（外部改动重读 / 用户手快）→ **什么都不做**，报「目标文字已变化」（拿旧偏移去拼就是改坏文档） | `main.rs::apply_ai`（比对 `doc[range]` 与当时的上下文） |
| AI 固定任务（语法修复 / 校对 / 术语 / 互译） | 菜单「AI → …」（有选中就只改那段，**否则整篇** —— 与 `Ctrl+K` 的默认值不同是故意的） | 同上 |
| AI 处理**一个文件**（不必先换文件） | 目录树里**右键**该文件 →「AI 处理「x.typ」…」（AI 菜单里也有）；写回那个文件 + 源缓存作废 + 重排。右键的正是打开着的那个时，一切以编辑器里那一份为准 | `main.rs::open_ai_for_file` · `ui/panes.rs`（目录树右键） |
| **AI 设置**（端点 / 模型 / Key） | 菜单「AI → **AI 设置…**」（三个框，Enter 存盘 · Esc 关）；也可手改 `settings.conf` 或设 `AI_BASE_URL`/`AI_MODEL`/`AI_API_KEY`（环境变量优先） | `main.rs::{open_ai_settings,save_ai_settings}` · `ui/overlays.rs::render_ai_settings` |
| 没配 Key 就不发请求 | `Ctrl+K` 一开浮层就把「没配 API Key —— 菜单「AI → AI 设置…」」写在上面；`spawn_ai` 也不再发那个注定 401 的请求（本地端点不需要 Key，照发） | `main.rs::open_ai_overlay`/`spawn_ai` |
| AI 通不通自检 | `cargo run -p typst-live -- --ai-selftest` —— 真发一次最小请求，打印端点/模型/Key/回复，退出码 `0`=通；加一个 `insert` 参数就跑一遍「插入」模式的提示词 | `main.rs::ai_selftest` |
| AI 报错里的信息 | 带 **HTTP 状态码**与**响应原文**（服务端报错不一定是 JSON：DeepSeek 没 Key 时回的是纯文本 `Authentication Fails (governor)`）；curl 退出码翻译成人话 | `ai.rs::{chat_messages,parse_completion_body,curl_hint}` |
| 关窗保护（未保存会拦一下） | 点窗口 × | `main.rs::on_close_requested` |
| 设置持久化（窗口/文件/缩放/主题/背景/跟随/折叠/自动保存/AI） | 自动，`%APPDATA%\typst-live\settings.conf` | `settings.rs` |
| 日志与 panic 留痕 | `%APPDATA%\typst-live\typst-live.log` | `diag.rs` |

---

## 二、缺失（想做的话从这里挑）

按「值不值得做」排，前面的更值得。

| 缺什么 | 大概怎么做 | 代价 |
| --- | --- | --- |
| **行号列**标红（不是整行底色） | 必须改库 `element.rs` 的 gutter 绘制 —— 本项目目前**没有** fork gpui-component | 中（要开始维护一个 fork） |
| 括号配对高亮的「随光标实时刷新」 | 库只在文本变化后重新要颜色（`_pending_update`），纯粹移动光标不刷新。要实时得改库的 `Lsp::update` 调用时机 | 中 |
| 折叠箭头**常显** | 同上（库只在 hover 行号列 / 光标行 / 已折叠时画） | 中 |
| 补全带参数签名 / 文档 | 需要 `typst::Library` 里每个函数的签名与文档字符串，候选结构要换成带 `detail` 的两行下拉 | 中 |
| 大纲「当前章节」高亮 | 拿光标行号与 `outline[].line` 比对即可 | 小 |
| 补全在 `#import` / `#set` 之后也触发 | 现在是「前面紧邻 `#`」才弹 | 小 |

---

## 三、明确不做（附理由，别重新捡起来）

| 提议 | 为什么不 |
| --- | --- |
| 快速打开（Ctrl+P 文件跳转） | 用户要求撤掉（`finder` 模块一并删除）；左栏目录树 + 系统文件管理器够用。 |
| 大纲面板（标题树） | 用户要求撤掉（占左栏宽度，实际导航用目录树 + 跳转）。引擎侧的 `syntax::outline()` 保留（状态栏「N 标题」还在用它）。 |
| 用系统文件对话框（`rfd` / Windows `IFileDialog`）做「打开文件夹」 | 原生对话框会在 gpui 里开一个嵌套消息循环；本项目已被嵌套消息循环咬过一次（借用竞态 → 进程直接退）。所以选择器是**应用内浮层**。 |
| 加缩放按钮 / 缩放滑块 | 用户在 `wu` 上明确拒绝过（"这个不需要添加"）。缩放有 `Ctrl+=/-/0` 与 `Ctrl+滚轮`；预览头与状态栏已经显示百分比。 |
| 预览区加"单页/双页"切换 | 本项目的预览是位图纹理 + 自绘页框，Typst 的页是独立单元，没有"双页排版"这个语义。 |
| 让用户改"纸张尺寸" | 纸张来自文档自己的 `#set page(...)`。查看器改它 = "所见非所编译"。要改就改文档（可以在工具栏加一个"插入页面设置"的按钮，那改的是文档）。 |
| 退回 SyncTeX / 编译辅助文件方案 | 现有的字形出处方案更快（不用等 aux 文件）、更准（字级而不是段级）、不改文档。见 `engine/src/jump.rs`。 |
| fork gpui-component 去改行号列 / 折叠箭头常显 | 收益（几处视觉细节）与代价（从此要跟官方 rebase）不成比例。现在的做法是**只用它的公开钩子**（`lsp.completion_provider` / `document_color_provider` / `set_folding`）。 |
| 右侧预览换成 SVG 渲染 | 位图纹理路线已经跑通且可控（DPI、惰性出图、双击定位都依赖它），换 SVG 要重做整条预览链。 |
| 用 `typst_syntax::highlight()` 换掉 tree-sitter 高亮 | 库把高亮焊死在 tree-sitter 上（`LanguageConfig.language` 是硬字段、`SyntaxHighlighter` 是具体结构体），换要 fork。见 `engine/src/syntax/mod.rs` 的模块文档。 |
| 录屏时录**系统声音** | 用户要的是「录语音」，麦克风已经覆盖；系统声音要走 WASAPI loopback（或虚拟声卡），与「要简单」冲突。 |
| 多显示器 / 窗口移动跟随 | `gdigrab` 的桌面以**主屏左上角**为原点（副屏坐标是负的），所以只支持主屏；跟随窗口移动要重启 ffmpeg（一次录制会碎成多个文件）。现在的行为是：录制区域固定为「按下开始那一刻的窗口矩形」。 |
| 用 ffmpeg 原生能力做暂停 | 单进程 ffmpeg 没有原生暂停：`sendcmd` 管不到实时源，挂起进程会把实时时钟搞乱（dshow 的缓冲会溢）。现在是「分段 + `-c copy` 拼接」，实测拼出来全片解码零错误。 |
| 实时合成画中画（摄像头直接接进 overlay） | 实测会让**时间轴错乱**：195 帧只占 3.56s（放出来是快进），因为 dshow 摄像头的 PTS 与墙钟对不上。所以改成「录完再合成」——文件到文件，时间轴确定，7 秒素材用 nvenc 只要 0.44s。 |
| 用 `ddagrab`（桌面复制）代替 gdigrab | 理论上更对（GPU 原生、不占 CPU），但**本机跑不了**：装了 ToDesk 虚拟显示器，DXGI 0/1/2 号适配器都没有输出（`Selected output not supported`）。换成没装虚拟显示器的机器可以再评估（`record.rs` 模块文档里记了完整探测结果）。 |

---

## 四、评审清单里出现过、但**界面里不存在**的东西

（记下来，免得下次又说"我看到了"）

- 「右下角绘图/批注浮动工具条（□ ○ ↗ ✎）挡住了预览」—— 全仓 `.absolute()` 只有四处：
  三个模态浮层（`Ctrl+P` 快速打开 / `Ctrl+K` AI 编辑 / 关窗提示）+ 跳转高亮框。
  **没有任何常驻浮动工具条**，也没有绘图/批注代码。
- 「编辑区底部黑底浮动栏（含 P / 快捷键 / AI）」—— 同上。唯一的深色条是终端面板（`Ctrl+4`），
  默认收起、带「终端 / 关闭」标题。
- 「左侧大纲是空白的 / 没有层级」—— 大纲有缩进（按 `depth` 算 `pl()`）与两级配色。
- 「状态栏没有颜色提示」—— 错误徽标是红底红字（可点），无错误是绿字 `✓ 无错误`。
