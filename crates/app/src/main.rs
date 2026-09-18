// 发布版是 GUI 子系统：双击 exe 不该跟出一个控制台黑框。
//
// 用 `cfg_attr` 而不是无条件写死：debug 构建保留控制台，
// `cargo run` 时那些「首帧 137 ms」「排版 1.9 ms」还是直接打在终端里。
// （无条件写死过一次的话，你会发现开发时什么都看不见 —— 诊断全进了日志文件。）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 实时 Typst 预览器 —— `typst-engine` 的 GPUI 外壳。
//!
//! 左边敲字，右边在**几毫秒内**跟着变。中间没有子进程、没有 IPC：
//! 编辑器把文本喂给引擎的内存覆盖层，引擎在同一个进程里重排版。
//! 排版不读磁盘（不用存盘、不用辅助文件）—— 写盘只在**保存**时发生：
//! `Ctrl+S`，或打字停下 1 秒的自动保存（`autosave` 模块，默认开）。
//!
//! ```bash
//! cargo run -p typst-live              # 用内置示例文档
//! cargo run -p typst-live -- doc.typ   # 打开指定文件
//! ```
//!
//! **排版与光栅化是两层**：缩放只重做光栅化，不重新排版。状态栏上
//! 「排版 N 次 / 光栅化 M 次」两个计数会把这件事直接显示出来 ——
//! 按 Ctrl+= 缩放时你只会看到后一个数在涨。

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::highlighter::{
    Diagnostic as Squiggle, DiagnosticSeverity, LanguageConfig, LanguageRegistry,
};
use gpui_component::input::{Input, InputEvent, InputState, Position};
use gpui_component::list::ListItem;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_component::tree::{Tree, TreeEvent, TreeState};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Root, RopeExt as _, Sizable as _, h_flex, v_flex,
};

use typst::diag::SourceDiagnostic;
use typst::foundations::Bytes;
use typst::model::Destination;
use typst_engine::export::{RasterPage, pdf as export_pdf, pixel_per_pt_for_zoom, rasterize_page};
use typst_engine::jump::{Anchor, LayoutIndex};
use typst_engine::syntax as lang;
use typst_engine::world::{EngineWorld, EntryState, Packages, embedded_and_system_fonts};
use typst_layout::PagedDocument;

use ai::AiTask;
use ai_scope::{AiScope, ScopeChoice};
use completion::TypstCompletion;
use editor_marks::{EditorMarks, MarkState};
use image_view::ImageView;
use markdown_view::MarkdownView;
use markup::Markup;
use settings::Settings;
use term_colors::TerminalPalette;

use safe_update::{UpdateOutcome, safe_entity_update, safe_task_read, safe_task_update};
use ui::util::{describe_theme, short_label, short_path, squiggle_end};

/// 诊断输出：debug 下同时打 stdout，任何构建都追加进日志文件。
///
/// 定义在 `mod` 声明之前（宏是按文本顺序可见的），这样每个模块都能直接用。
/// 展开里的路径写 `$crate::` 而不是 `diag::` —— 后者会按**调用点**解析，
/// 在子模块里就找不到了。
macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::diag::log_line(&format!($($arg)*))
    };
}

mod ai;
mod ai_scope;
mod autosave;
mod completion;
mod coords;
mod diag;
mod diff;
mod disk_watch;
mod editor_marks;
mod image_view;
mod jump_glue;
mod markdown_view;
mod markup;
mod preview;
mod record;
mod safe_update;
mod settings;
mod term_colors;
mod terminal;
mod terminal_view;
mod themes;
mod tree;
mod ui;

actions!(
    typst_live,
    [
        ZoomIn,
        ZoomOut,
        ZoomReset,
        SaveFile,
        RecompileNow,
        FormatDocument,
        ExportPdf,
        PrevPage,
        NextPage,
        FirstPage,
        LastPage,
        SyncToPreview,
        AiEditOpen,
        AiSettingsCancel,
        AiSubmit,
        AiCancel,
        AiNextHunk,
        AiPrevHunk,
        AiToggleHunk,
        ToggleShell,
        ToggleFolding,
        ToggleFollow,
        TogglePreviewBg,
        ToggleMetrics,
        ToggleAutosave,
        ToggleRecording,
        ToggleRecordPause,
    ]
);

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEP: f32 = 1.25;

/// 录屏轮询间隔：刷按钮上的计时 + 看 ffmpeg 是不是偷偷退了。
const REC_POLL: Duration = Duration::from_secs(1);

/// 设置写盘前的等待时长。
///
/// 拖动窗口时尺寸每帧都在变 —— 变一次写一次盘（一秒几十次）太吵。
/// 所以是**防抖**：状态一变就重置计时器，停下来之后才写。
const SETTINGS_DEBOUNCE: Duration = Duration::from_millis(400);

/// 可见范围外再预出几页。留 1 页是为了滚动时不会先看到空白。
const PAGE_PREFETCH: usize = 1;

/// 跳转后高亮淡出的时长。
///
/// 跨页跳过去之后，没有这个框人得自己找位置 —— 它是「跳过去了」的
/// 唯一视觉证据。淡出而不是一直留着，是因为它是**一次性的**反馈，
/// 不是「当前位置」的标记。
const FLASH: Duration = Duration::from_millis(1400);

/// 最近文件夹最多记几个（菜单里排太久会很难看）。
const MAX_RECENT_DIRS: usize = 8;

/// 「跟随光标」的静默期：用户手动滚过预览之后，这期间不自动抢滚动位置。
const FOLLOW_SILENCE: Duration = Duration::from_secs(3);

/// 进程起点。给「首帧」「首次排版」这类一次性事件打时间戳 ——
/// 「窗口多久出来」「排版多久」得是能对上的数字，不是感觉。
fn since_start_ms() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// 前向跳转时把目标行放在视口顶部往下多少逻辑像素处 ——
/// 上面留一点，好看见「这是哪一段」。
const SYNC_MARGIN: f32 = 80.0;

/// 预览里页面两侧留的边距（逻辑像素）。「适应宽度」时页宽 = 展示区宽 - 2×它。
const PREVIEW_MARGIN: f32 = 24.0;

const DEMO_DOC: &str = r#"#set page(width: 15cm, height: auto, margin: 1.8cm)
#set text(size: 11pt)

= 实时编译演示

在*左边随便改点什么*，右边会在几毫秒内跟着变。

没有子进程，没有 IPC —— 编译器直接读内存里未保存的文本。

== 试试缩放

按 *Ctrl+=* 放大、*Ctrl+-* 缩小、*Ctrl+0* 回到 100%。

看状态栏：*排版次数不会变，光栅化次数才会变* —— 因为缩放只是重新出图，
没有重新排版。DPI 完全由我们自己决定，所以放到 400% 也不会糊。

== 为什么能这么快

+ 覆盖式虚拟文件系统：编译器看到的就是你正在敲的内容
+ `Source::replace` 的增量重解析：敲一个字只重解析几十字节
+ comemo 记忆化排版：没变的部分不重算

== 数学与表格

$ integral_0^infinity e^(-x^2) dif x = sqrt(pi)/2 $

#table(
  columns: 3,
  [*指标*], [*冷编译*], [*敲键*],
  [耗时], [191 ms], [1.9 ms],
  [重解析], [全文 34 KB], [276 B],
  [加速], [—], [102×],
)

== 打错字也不会白屏

把下面这行的括号删掉试试 —— 右边会保留上一次编译成功的结果，
只在下方列出错误，而不是变成一片空白。

#let broken = (1 + 2)

== 双击跳转

*双击左边*的任意一行，右边会跳到它在哪一页；*双击右边*的任意一处，
左边的光标会移到那句话上。两边都会留下一个会淡出的框。

+ `Ctrl+Alt+J` 也能做前向跳转（不用鼠标）
+ 预览里的链接用 *Ctrl+单击* 打开，比如 #link("https://typst.app")[Typst 官网]
+ 跳转靠的是排版结果里的字形出处，所以跳一次只需重新出图：
  状态栏的「排版次数」不会变
"#;

fn register_typst_language() {
    LanguageRegistry::singleton().register(
        "typst",
        &LanguageConfig::new(
            "typst",
            codebook_tree_sitter_typst::LANGUAGE.into(),
            vec![],
            include_str!("../typst-highlights.scm"),
            "",
            "",
        ),
    );
}

#[derive(Default)]
struct Status {
    /// 排版耗时（`typst::compile`）。
    compile_ms: f64,
    /// 光栅化耗时（`typst_engine::export::rasterize`）。
    raster_ms: f64,
    reparsed: usize,
    text_bytes: usize,
    pages: usize,
    /// 排版次数。缩放**不会**让它增加。
    compiles: usize,
    /// 光栅化次数。缩放**会**让它增加。
    rasters: usize,
}

/// 「视图」菜单里的显隐开关。
///
/// 用一个枚举而不是 5 个 action：菜单项要做的就是「取当前值取反」这一件事，
/// 拆成 5 个 action 只是把同一段代码抄 5 遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneToggle {
    Tree,
    Editor,
    Preview,
    Toolbar,
    Statusbar,
}

/// AI 编辑的当前阶段（一个状态机，界面上只画一个浮层，内容随阶段变）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiStage {
    /// 没开
    Closed,
    /// 正在输入要求
    Ask,
    /// 正在生成（可以取消）
    Running,
    /// 生成完了，等确认（逐块可切）
    Review,
}

/// AI 三项配置（端点 / 模型 / Key）。
///
/// `None` / 空串 = 「没配」—— 那一项就用 `ai::resolve_*` 的默认值。
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct AiConfig {
    base_url: Option<String>,
    model: Option<String>,
    key: Option<String>,
}

impl AiConfig {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            base_url: settings.ai_base_url.clone(),
            model: settings.ai_model.clone(),
            key: settings.ai_api_key.clone(),
        }
    }

    /// 状态栏 / 菜单里那句「现在用的是哪个」——配置能不能用，得能一眼看到。
    fn summary(&self) -> String {
        let endpoint = ai::resolve_base_url(self.base_url.as_deref());
        let host = endpoint
            .split("//")
            .nth(1)
            .unwrap_or(&endpoint)
            .split('/')
            .next()
            .unwrap_or(&endpoint)
            .to_string();
        let key = if ai::api_key(self.key.as_deref()).is_some() {
            "Key 已配"
        } else if ai::is_local_endpoint(&endpoint) {
            "本地端点，无需 Key"
        } else {
            "⚠ 没配 Key"
        };
        format!(
            "{host} · {} · {key}",
            ai::resolve_model(self.model.as_deref())
        )
    }
}

/// 「AI 设置」浮层的三个输入框。
///
/// 以前这三项（端点 / 模型 / Key）只能手改 `settings.conf`，界面上一个入口都没有 ——
/// 「Ctrl+K 出来的 AI 不能用」最常见的原因就是没处填 Key（请求发出去了、401 回来了，
/// 人只能对着状态栏一行字猜）。所以补上这个浮层。
struct AiSettingsForm {
    base_url: Entity<InputState>,
    model: Entity<InputState>,
    key: Entity<InputState>,
}

/// 「选中文字 → AI 编辑」的全部状态。
///
/// 设计要点：**生成在 std 线程里跑，界面只轮询**。
/// 不用 gpui 的 background_executor 跑这个阻塞调用 —— 那个池子里还跑着
/// 设置防抖、终端事件轮询等定时任务，一次 60 秒的 curl 会把它们一起饿死。
struct AiEdit {
    stage: AiStage,
    /// 这一轮改的是哪儿（选中文字 / 整篇 / 一个文件）。
    scope: AiScope,
    /// 范围说明（画在浮层上，让人知道 AI 在改哪一段）
    scope_label: String,
    /// 原文本（diff 的左边，也是取消时的回滚依据）
    original: String,
    /// 模型返回、已剥掉代码围栏的结果
    result: String,
    /// 结果与原文本之间的变更块
    hunks: Vec<diff::Hunk>,
    /// 每块是否接受（默认全接受，AI 改稿的默认意图是改）
    accepted: Vec<bool>,
    /// 当前选中的块（键盘上下移动）
    selected: usize,
    /// 已生成多少字（思考模型的耗时主要在思维链上，也计入）
    progress: Arc<AtomicUsize>,
    /// 取消标志（置 true 会 kill 掉 curl）
    cancel: Arc<AtomicBool>,
    /// 后台线程的回执通道
    rx: Option<Receiver<Result<String, String>>>,
    error: Option<String>,
}

impl Default for AiEdit {
    fn default() -> Self {
        Self {
            stage: AiStage::Closed,
            scope: AiScope::Document,
            scope_label: String::new(),
            original: String::new(),
            result: String::new(),
            hunks: Vec::new(),
            accepted: Vec::new(),
            selected: 0,
            progress: Arc::new(AtomicUsize::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            error: None,
        }
    }
}

/// 右侧主区显示什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightPane {
    /// 排版预览（位图纹理）。
    Preview,
    /// Markdown 渲染（真正的文本层，可选可复制）。
    Markdown,
    /// 图片查看。
    Image,
}

/// 预览区（纸张以外）的背景样式。
///
/// 纯灰太素、网格能帮上「一眼看出页面边界与对齐」的忙，但有人嫌花 ——
/// 所以做成可切的两个档，默认纯色（与之前一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewBg {
    /// 纯色（主题的 secondary）
    Solid,
    /// 网格：纯色底 + 一层细线
    Grid,
}

impl PreviewBg {
    /// 设置文件里的值 → 样式（认不出的当纯色）。
    fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("grid") => Self::Grid,
            _ => Self::Solid,
        }
    }

    /// 样式 → 写进设置文件的值。
    fn as_setting(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Grid => "grid",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Solid => "纯色",
            Self::Grid => "网格",
        }
    }
}

/// 「打开文件夹…」的选择器状态。
///
/// 刻意用**应用内**的浮层，不用系统文件对话框：原生对话框会在 gpui 里开一个
/// 嵌套消息循环，而本项目已经被嵌套消息循环咬过一次（借用竞态 → 进程退）。
/// 一层层往下点也够用。
pub struct FolderPicker {
    /// 现在停在哪个目录。
    cwd: PathBuf,
    /// 这一层的子目录（已排序、已过滤）。
    dirs: Vec<PathBuf>,
}

impl FolderPicker {
    fn new(cwd: PathBuf) -> Self {
        Self {
            dirs: tree::subdirs(&cwd),
            cwd,
        }
    }

    /// 进一个子目录。
    fn enter_dir(&mut self, dir: PathBuf) {
        self.dirs = tree::subdirs(&dir);
        self.cwd = dir;
    }

    /// 去上一层。
    fn up(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            let parent = parent.to_path_buf();
            self.enter_dir(parent);
        }
    }
}

/// 跳转索引 + 它是为哪一份排版结果建的。
///
/// `Arc` 指针相同就说明排版结果没换，索引可以直接用 —— 于是
/// **缩放、滚动、翻页都不会重建索引**：它们只碰光栅化那一层。
struct IndexCache {
    doc: Arc<PagedDocument>,
    index: LayoutIndex,
}

/// 跳转落点上的一小块高亮。
struct Flash {
    page: usize,
    /// 页内 pt 矩形 `[x0, y0, x1, y1]`。
    rect: [f32; 4],
    /// 递增序号。它同时是元素的 id：换了序号才会重新挂载，
    /// 淡出动画也才会从头播（同 id 的元素会复用上一次的动画状态）。
    seq: usize,
}

struct Previewer {
    engine: EngineWorld,
    main_path: PathBuf,
    /// 布局数字报过没有（开机报一次，见 `report_layout_once`）。
    layout_reported: bool,
    /// 中间三块分区的状态。**显式持有**是为了能读到每个分区的真实宽度 ——
    /// 「编辑区到底多宽」这种事不该靠推算（踩过一次：分区 875px、内容只有 117px）。
    split_state: Entity<ResizableState>,
    editor: Entity<InputState>,
    /// 排版结果。缩放时原样不动 —— 这是「排版 / 光栅化」两层的分界。
    doc: Option<Arc<PagedDocument>>,
    /// 缩放倍率。1.0 = 屏幕上「实际大小」（96 dpi）。
    zoom: f32,
    /// 缩放是不是「适应宽度」模式（默认开）：开着时每次渲染按展示区宽度重算，
    /// 所以拖动分区、改窗口大小页面都会跟着充满。手动缩放会关掉它，Ctrl+0 回来。
    zoom_fit: bool,
    /// 窗口所在显示器的缩放系数（Windows 的「缩放与布局」，常见 1.0 / 1.25 / 1.5）。
    ///
    /// **必须参与光栅化**：`zoom` 说的是「文档要占多大的逻辑尺寸」，
    /// 而真正要填满的是**物理像素**。1 逻辑像素 = `scale_factor` 个物理像素，
    /// 所以按 96 dpi 出图、再让 gpui 按 1.5 放大，结果就是糊的。
    /// （初版就踩了这个坑：把「1 纹理像素 = 1 逻辑像素」当成了
    /// 「1 纹理像素 = 1 物理像素」，于是在 150% 缩放的屏上字发虚。）
    scale_factor: f32,
    /// 文档大纲。来自 `typst-syntax`，不是正则。
    outline: Vec<lang::OutlineItem>,
    /// 本次排版产出的原始诊断。留着是因为要把它们**映射成字节范围**
    /// 去画波浪线，而转成字符串就找不回来了。
    compile_errors: Vec<SourceDiagnostic>,
    /// 语法错误 + 编译错误里「错误」的条数。
    error_count: usize,
    /// 文本规模（字/词/行）。
    stats: lang::TextStats,
    /// 自上次保存以来改过没有。
    ///
    /// 与 `saved_text` 是一对：这个回答「有没有动过」，那个回答「磁盘上是什么」。
    /// 谁决定把 `dirty` 置位，就必须一起给 `touch_autosave` 起计时器 ——
    /// 所以四处置位都走 `mark_edited`，别直接写 `self.dirty = true`。
    dirty: bool,
    /// 自动保存开着吗（设置里存，没这项时默认**开**）。
    autosave: bool,
    /// 磁盘上那一份是什么。自动保存靠它判断「有没有真的变」——
    /// 不是靠「敲过键」：打字又撤回去就不该白写一次盘。
    saved_text: String,
    /// 自动保存写盘次数（日志与状态栏里的证据）。
    autosave_writes: usize,
    /// 磁盘上那一份的指纹（长度 + mtime）。**只在「我们知道磁盘上是什么」时更新**：
    /// 打开文件、我们自己写盘之后、以及一次外部改动被处理之后。
    /// `None` = 磁盘上没有这个文件（或还没查过，见 `disk_watch::Stamp::of`）。
    disk_stamp: Option<disk_watch::Stamp>,
    /// 从磁盘读到的新版本，等 `render` 那一拍装进编辑器。
    ///
    /// 不当场 `set_value`：那要 `&mut Window`，而轮询任务手上没有
    /// （与 `pending_forward` 同一个套路，方向相反 —— 那个等的是光标就位）。
    pending_reload: Option<(String, disk_watch::Stamp)>,
    /// 外部改动的轮询循环已经在跑了吗（只允许一个）。
    disk_polling: bool,
    /// 自动重读次数（日志与状态栏里的证据）。
    disk_reloads: usize,
    /// 外部改动**没能**落地时说过的最后一句话。
    ///
    /// 轮询每 500 ms 一拍，不记着就会把同一句话刷满状态栏；
    /// 情况变了（存了 / 重读成功）就清掉，下次还能再说。
    disk_problem: Option<String>,
    /// 焦点在编辑器里吗 —— 外部改动落地时要不要跟着把光标放回去，看它。
    ///
    /// `InputState` 的 `focus_handle` 是 `pub(super)`，外面拿不到，所以走
    /// 「订阅 Focus / Blur 事件」这条公开路径自己记一个（`set_cursor_position`
    /// 会 `focus`，焦点在终端 / AI 浮层上时那一下等于把键盘从用户手里抢走）。
    editor_focused: bool,
    /// 关窗请求被拦下来了吗（有未保存改动时先问一句，见 `on_close_requested`）。
    close_prompt: bool,
    /// 用户已经确认过这次关闭（保存完了 / 选择放弃）：再来的关窗请求直接放行。
    close_confirmed: bool,
    /// 关窗提示浮层自己的焦点（Esc 取消要用 —— 动作派发从聚焦节点开始）。
    close_focus: FocusHandle,
    /// 一次性的操作反馈（已保存 / 已格式化 / 导出到哪）。
    message: Option<String>,

    /// 项目根目录。快速打开从这里往下找文件。
    root: PathBuf,
    /// 每页的光栅化纹理。`None` = 还没出图（或已被视口卸载）。
    ///
    /// **不一次性光栅化全部页**：A4 单页在 100% 下就 7.7 MiB，
    /// 100 页就是 770 MiB；400% 时每页 54 MiB。现在只留可见的几页。
    bitmaps: Vec<Option<Arc<RenderImage>>>,
    /// 每页的**逻辑**尺寸（宽, 高）。布局靠它，不靠纹理 ——
    /// 所以某一页没出图时占位尺寸依旧正确，滚动位置与翻页都不受影响。
    page_sizes: Vec<(f32, f32)>,
    /// 当前已经出图的页范围（含），用于判断「要不要重新同步」。
    live_range: Option<(usize, usize)>,
    /// 预览滚动区的句柄。翻页就是把某一页滚到顶部。
    scroll: ScrollHandle,
    /// 当前页（0 起）。翻页与缩放后回锚都靠它。
    current_page: usize,
    /// 当前纹理共占多少字节。用来把显存开销直接显示给用户。
    texture_bytes: usize,
    /// 上一次已排版的文本。
    ///
    /// gpui-component 的 `InputEvent::Change` 不止在文字变化时发出
    /// （光标移动、选中也会），不挡一下就会白排一遍。
    last_text: String,
    /// 跳转索引（源码 ⇄ 显示区）。**惰性构建**：只在真的要跳的时候建。
    index: Option<IndexCache>,
    /// 上一次排版**是否成功**。
    ///
    /// 失败时预览显示的是上一次成功的结果，而源码已经改了 ——
    /// 两边的字节素引就对不上，所以那段时间里跳转要关掉，
    /// 否则会把人送到错的行上（宁可说「现在跳不了」）。
    index_usable: bool,
    /// 索引构建次数与耗时。状态栏那两个数就是「索引是第三层」的证据：
    /// 缩放时只有光栅化在涨。
    index_builds: usize,
    index_ms: f64,
    /// 跳转留下的高亮。
    flash: Option<Flash>,
    /// 高亮序号，自增。
    flash_seq: usize,
    /// 双击编辑区后要做的前向跳转。
    ///
    /// 不当场做，而是等这一轮事件走完再在 `render` 里做 —— 那时
    /// 编辑器已经把光标挪到点击处了，读到的位置才是准的。
    pending_forward: bool,
    /// 从磁盘读来的设置（同时也是「已经写下去的那一份」——
    /// 每次保存成功后都会用新的快照替掉它，所以「变没变」一比就知道）。
    settings: Settings,
    /// 窗口几何变了、但还没写盘。
    pending_window: Option<settings::WindowBox>,
    /// 有未落盘的改动。
    settings_dirty: bool,
    /// 设置写盘次数（日志里的证据）。
    settings_writes: usize,
    /// 现在编辑的是「用户的文件」而不是内置示例 ——
    /// 只有它才值得记进设置里的「上次打开的文件」。
    explicit_file: bool,
    /// 当前主题名。**刻意与 `settings.theme` 分开**：后者是「已经写进磁盘的
    /// 那一份」，前者是「现在想要的」。混用一个字段的话，`save_settings` 里
    /// 「变了才写」的比较永远相等 —— 界面换了主题、磁盘上却没写（踩过）。
    theme_name: Option<String>,
    /// 首次排版还没做（推迟到窗口画出来之后，见 `render`）。
    first_compile: bool,
    /// 右侧主区模式（预览 / Markdown / 图片）。
    right: RightPane,
    /// 目录树状态（gpui-component 的 `Tree`）。
    tree_state: Entity<TreeState>,
    _tree_sub: Subscription,
    /// 目录树的展开集合 —— 重建条目时按 id 恢复，不然每次刷新都塌回去。
    tree_expanded: HashSet<String>,
    /// 目录树最近一次扫描的根；变了才重扫。
    tree_root: Option<PathBuf>,
    /// Markdown 视图（打开 .md 时喂给它）。
    markdown: Entity<MarkdownView>,
    /// 图片视图（打开图片时新建一个）。
    image: Option<Entity<ImageView>>,
    /// AI 浮层自己的焦点句柄。
    ///
    /// 评审阶段输入框已经不画了，若没人持有焦点，gpui 的动作派发就**没有起点**
    /// （`NEXT.md` 里那个坑：没焦点 = 所有快捷键静默失效），所以浮层自己拿一个。
    ai_focus: FocusHandle,
    /// AI 编辑：要求输入框 + 状态
    ai_input: Entity<InputState>,
    _ai_sub: Subscription,
    ai: AiEdit,
    /// 多轮对话历史（`(是否用户, 文本)`）—— 连续按 Ctrl+K 时模型能记住上文
    ai_history: Vec<(bool, String)>,
    /// AI 设置的三个输入框（常驻，开关只切可见性 —— 与 AI 要求输入框同一个套路）。
    ai_settings_form: AiSettingsForm,
    /// 「AI 设置」浮层开着没有。
    ai_settings_open: bool,
    /// 三个输入框的 Enter 订阅（丢了订阅就不触发，所以得存着）。
    _ai_settings_subs: Vec<Subscription>,
    /// AI 三项配置的**当前想要值**（与 `settings` 里「已经写下去的那一份」分开）。
    ///
    /// 与 `theme_name` 同一个道理：`save_settings` 靠「比一比」决定要不要写盘，
    /// 比较的右边是 `desired_settings()` —— 直接把新值写进 `self.settings` 的话，
    /// 那边比出来永远相等，于是**界面上改了、磁盘上没写**（填完 Key 重启就没了）。
    ai_cfg: AiConfig,
    /// 交互式终端（alacritty + pty）。整个生命周期只在 UI 线程用，
    /// PTY 的读写线程由 alacritty 的 EventLoop 自己持有。
    shell_terminal: Option<Rc<terminal::Terminal>>,
    /// 终端自己的焦点：它得能拿到键盘输入
    terminal_focus: FocusHandle,
    /// 终端的输入法状态（中文输入时的预编辑文本）
    terminal_ime: Entity<terminal_view::ImeState>,
    /// 终端面板是否可见（Ctrl+4 切换）
    shell_visible: bool,
    /// 三块面板与两条栏的显隐（「视图」菜单里勾）。
    ///
    /// 录出来的画面**就是窗口本身** —— 所以「要录出干净画面」= 把这些关掉，
    /// 不需要另外做一个「录屏模式」。
    show_tree: bool,
    show_editor: bool,
    show_preview: bool,
    show_toolbar: bool,
    show_statusbar: bool,
    /// 正在录的那一份（`None` = 没在录）。
    rec: Option<record::Recorder>,
    /// 录制时长（每秒刷一次，给按钮上的计时）。
    rec_elapsed: Duration,
    /// 收尾（拼接 / 画中画合成）正在后台跑。
    rec_finalizing: bool,
    /// 录屏轮询任务起了没有（只允许一个，与 `disk_polling` 一个路子）。
    rec_polling: bool,
    /// 录屏时录麦克风 / 录摄像头画中画。
    rec_mic: bool,
    rec_cam: bool,
    /// 设备名缓存（第一次录时才枚举一次 dshow）。
    rec_mic_device: Option<String>,
    rec_cam_device: Option<String>,
    /// `--rec-selftest`：要录多少秒（`Some` = 正在自测）。
    rec_selftest: Option<u64>,
    /// 自测已经开过录没有（免得每帧都去开一次）。
    rec_selftest_started: bool,
    /// 主题名（「视图 → 主题」里勾选的那一个）。
    /// 状态栏那个下拉框已经搬进菜单，所以这里只留名字。
    /// 本文档定义的名字（`#let` / `#import`）—— 补全候选的一路来源。
    /// 与补全 provider 共享同一份（`Rc`），所以刷新一次两边都看得见。
    local_defs: completion::SharedNames,
    /// 编辑器底色的共享状态：错误行 + 配对括号 + 配色（见 `editor_marks`）。
    marks: Rc<RefCell<MarkState>>,
    /// 预览区背景样式。
    preview_bg: PreviewBg,
    /// 跟随光标：光标停下后自动把预览滚到对应位置。
    follow_cursor: bool,
    /// 跟随循环是否已经在跑（只允许一个，关了之后自己退出并复位）。
    follow_polling: bool,
    /// 跟随用：上一次的光标偏移（变了才跳）。
    follow_last_cursor: usize,
    /// 跟随的静默期：用户手动滚过预览之后就暂停一会儿再跟随
    /// （否则「用户想往上翻，程序每 400ms 把他拽回去」）。
    follow_pause: Option<Instant>,
    /// 编辑器里是否开着代码折叠（库默认就是开）。
    folding: bool,
    /// 上一次重编译拿到的诊断（**已映射到行**的那一份）。
    /// 错误面板、状态栏的错误计数、编辑器的错误行底色都用它。
    diags: Vec<lang::Diagnostic>,
    /// 目录树里有几个条目（0 → 面板上给一句「这里没东西」的提示）。
    tree_entries: usize,
    /// 最近打开过的文件夹（菜单「文件 → 最近文件夹」）。
    recent_dirs: Vec<PathBuf>,
    /// 「打开文件夹…」的浮层状态；`None` = 没开。
    folder_picker: Option<FolderPicker>,
    /// 选择文件夹浮层的焦点（Esc 要能用）。
    folder_focus: FocusHandle,
    /// 状态栏是否显示性能指标（默认关，状态栏保持精简）。
    show_metrics: bool,
    /// 目录树正在扫（后台扫盘时给用户一句反馈）。
    tree_scanning: bool,
    status: Status,
    _sub: Subscription,
}

impl Previewer {
    fn new(
        source: String,
        main_path: PathBuf,
        explicit_file: bool,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = main_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut engine = EngineWorld::new(
            embedded_and_system_fonts(),
            EntryState::new(root, &main_path),
        )
        // 包源：本地找不着就联网取官方源（`@preview/...`）。
        // 下载落在 typst 自己的缓存目录，与 CLI 共用一份。
        .with_packages(Packages::with_downloads());
        let main_id = engine.entry().main();

        // 把初始文本放进内存覆盖层 + 语法树，这样磁盘上有没有这个文件都无所谓。
        engine
            .vfs_mut()
            .map_shadow(&main_path, Bytes::from_string(source.clone()));
        engine.sources().feed_memory(main_id, &source);

        let split_state = cx.new(|_| ResizableState::default());

        // 磁盘上那一份就是刚读进来的 `source`（内置示例文档也一样：它没有真文件，
        // 但「磁盘上那份」这个概念得有值，否则第一拍就会觉得「变了」）。
        let saved_text = source.clone();
        // 指纹也要一开始就记下：不记的话第一次轮询会把「我们自己刚读进来的这份」
        // 当成外部改动，白白重读重排一次（还会在状态栏闪一句「已重新加载」）。
        let disk_stamp = disk_watch::Stamp::of(&main_path);

        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .code_editor("typst")
                // 软换行保持 gpui-component 的默认（开）—— 长行折行显示是这个
                // 应用想要的；「显得窄」是宽度问题，得从分区尺寸上解决。
                .default_value(source)
        });

        let sub = cx.subscribe_in(&editor, window, |this, _, event, _window, cx| {
            match event {
                InputEvent::Change => this.on_editor_change(cx),
                // 「现在焦点在编辑器里吗」。外部改动落地时要靠它决定
                // 要不要把光标也放回去（见 `apply_disk_reload`）。
                InputEvent::Focus => this.editor_focused = true,
                InputEvent::Blur => this.editor_focused = false,
                _ => {}
            }
        });

        // ── 编辑器能力：补全 + 底色标记 ──────────────────────────
        //
        // 两者都挂在 gpui-component **公开**的钩子上（`InputState.lsp` 是 pub 字段），
        // 不需要 fork 库：
        // - `completion_provider`：候选来自引擎（内置名字 + 本文档 let 出来的名字）
        // - `document_color_provider`：错误行整行染色 + 配对括号（画成文字底下的色块）
        let local_defs: completion::SharedNames = Rc::new(RefCell::new(Vec::new()));
        let marks = Rc::new(RefCell::new(MarkState::default()));
        editor.update(cx, |state, _cx| {
            state.lsp.completion_provider = Some(Rc::new(TypstCompletion::new(local_defs.clone())));
            state.lsp.document_color_provider = Some(Rc::new(EditorMarks {
                state: marks.clone(),
            }));
        });

        // 折叠：库的默认是开，设置里明确关过才需要重新设一遍。
        let folding = settings.folding.unwrap_or(true);
        if !folding {
            editor.update(cx, |state, cx| state.set_folding(false, window, cx));
        }

        // 设置里记着上次用的主题就先装上（没有就保持 gpui-component 的默认）
        let theme_name = settings.theme.clone();
        if let Some(name) = settings.theme.clone() {
            match themes::apply(&name, window, cx) {
                Some(dark) => logln!("[typst-live] {} 主题 {name}", describe_theme(dark, cx)),
                None => logln!("[typst-live] 设置里的主题 {name:?} 不在注册表里，继续用默认"),
            }
        }

        // 目录树状态（扫描在 `refresh_tree` 里做，这里只建壳）
        let tree_state = cx.new(|cx| TreeState::new(cx));
        // Markdown 视图（打开 .md 时喂源码）
        let markdown = cx.new(MarkdownView::new);

        // 目录树的展开状态得记下来：刷新（重建条目）时要按它恢复，
        // 否则每点一次「刷新」整棵树都塌回去。
        let tree_sub = cx.subscribe_in(
            &tree_state,
            window,
            |this, _, event: &TreeEvent, _window, cx| {
                match event {
                    TreeEvent::Expanded(id) => {
                        this.tree_expanded.insert(id.to_string());
                    }
                    TreeEvent::Collapsed(id) => {
                        this.tree_expanded.remove(id.as_ref());
                    }
                }
                cx.notify();
            },
        );

        // 交互式终端：Windows 上用 PowerShell，其它平台按 $SHELL 找。
        let terminal_focus = cx.focus_handle();
        let terminal_ime = cx.new(|_| terminal_view::ImeState { marked_text: None });
        let shell_terminal = {
            let shell = default_shell();
            // `root` 已经被 `EntryState::new` 拿走了，这里重新算一次父目录
            let start_dir = main_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            match terminal::Terminal::new(&shell, Some(start_dir), 100, 30) {
                Ok(term) => {
                    logln!("[typst-live] 终端已启动：{shell}");
                    Some(Rc::new(term))
                }
                Err(err) => {
                    // 终端起不来不该影响编辑器：记一条，继续跑
                    logln!("[typst-live] 终端启动失败：{err}");
                    None
                }
            }
        };

        // AI 编辑的要求输入框（与快速打开同一个套路：常驻，开关只切可见性）
        let ai_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("比如：把这段改得更简洁 ／ 译成英文 ／ 改成表格")
                .submit_on_enter(true)
        });
        let ai_sub = cx.subscribe_in(&ai_input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.start_ai(window, cx);
            }
        });

        // 「AI 设置」的三个输入框（端点 / 模型 / Key）。
        //
        // 以前这三项只能手改 `settings.conf` —— 界面上一个入口都没，「Ctrl+K 出来的
        // AI 不能用」最常见的原因就是没处填 Key。三个框都 Enter 存盘（填完顺手一敲），
        // Esc 关掉（见 `render_ai_settings`）。
        let ai_settings_form = AiSettingsForm {
            base_url: cx.new(|cx| InputState::new(window, cx).submit_on_enter(true)),
            model: cx.new(|cx| InputState::new(window, cx).submit_on_enter(true)),
            key: cx.new(|cx| InputState::new(window, cx).submit_on_enter(true)),
        };
        let mut ai_settings_subs = Vec::new();
        for input in [
            &ai_settings_form.base_url,
            &ai_settings_form.model,
            &ai_settings_form.key,
        ] {
            ai_settings_subs.push(
                cx.subscribe_in(input, window, |this, _, event, _window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.save_ai_settings(cx);
                    }
                }),
            );
        }
        let ai_cfg = AiConfig::from_settings(&settings);

        // 目录树扫哪个目录、以及设置里那三项 UI 偏好。
        //
        // ⚠️ 树根**不等于**排版根：排版根必须是主文件所在目录（`#include "x.typ"`
        // 相对它解析），而用内置演示文档时那个目录是 `%TEMP%` —— 拿它当树根，
        // 「目录」面板里就是一堆临时文件/看着像空的。所以演示文档用**当前工作目录**。
        let root = if explicit_file {
            main_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        let preview_bg = PreviewBg::from_setting(settings.preview_bg.as_deref());
        let follow_cursor = settings.follow_cursor.unwrap_or(false);
        // 最近文件夹与性能指标都是「上次怎么用的就怎么来」
        let recent_dirs = settings.recent_dirs.clone();
        let show_metrics = settings.show_metrics.unwrap_or(false);
        // 自动保存：默认**开**（用户要求）。老设置文件里没这项 → 也是开。
        let autosave = settings.autosave.unwrap_or(true);
        // 面板/栏的显隐：默认都显示（老设置文件里没这几项）。
        let show_tree = settings.show_tree.unwrap_or(true);
        let show_editor = settings.show_editor.unwrap_or(true);
        let show_preview = settings.show_preview.unwrap_or(true);
        let show_toolbar = settings.show_toolbar.unwrap_or(true);
        let show_statusbar = settings.show_statusbar.unwrap_or(true);
        // 录屏：默认录麦克风 + 录摄像头画中画（都是用户点名要的能力）。
        let rec_mic = settings.record_mic.unwrap_or(true);
        let rec_cam = settings.record_cam.unwrap_or(true);
        let rec_selftest = match REC_SELFTEST.load(Ordering::Relaxed) {
            0 => None,
            secs => Some(secs),
        };

        let this = Self {
            engine,
            main_path: main_path.clone(),
            split_state,
            layout_reported: false,
            editor,
            doc: None,
            // 上次的缩放从设置里恢复（还要夹一次，手改过的设置可能越界）；
            // 但启动时默认仍走「适应宽度」—— 用户要的是「页面始终充满展示区」
            zoom: settings.zoom.unwrap_or(1.0).clamp(ZOOM_MIN, ZOOM_MAX),
            zoom_fit: true,
            scale_factor: window.scale_factor(),
            outline: Vec::new(),
            compile_errors: Vec::new(),
            error_count: 0,
            stats: lang::TextStats::default(),
            dirty: false,
            autosave,
            saved_text,
            autosave_writes: 0,
            disk_stamp,
            pending_reload: None,
            disk_polling: false,
            disk_reloads: 0,
            disk_problem: None,
            editor_focused: true,
            close_prompt: false,
            close_confirmed: false,
            close_focus: cx.focus_handle(),
            message: None,
            root,
            bitmaps: Vec::new(),
            page_sizes: Vec::new(),
            live_range: None,
            scroll: ScrollHandle::new(),
            current_page: 0,
            texture_bytes: 0,
            last_text: String::new(),
            index: None,
            index_usable: false,
            index_builds: 0,
            index_ms: 0.0,
            flash: None,
            flash_seq: 0,
            pending_forward: false,
            settings,
            pending_window: None,
            settings_dirty: false,
            settings_writes: 0,
            explicit_file,
            first_compile: true,
            right: RightPane::Preview,
            tree_state,
            _tree_sub: tree_sub,
            tree_expanded: HashSet::new(),
            tree_root: None,
            markdown,
            image: None,
            ai_focus: cx.focus_handle(),
            ai_input,
            _ai_sub: ai_sub,
            ai: AiEdit::default(),
            ai_history: Vec::new(),
            ai_settings_form,
            ai_settings_open: false,
            _ai_settings_subs: ai_settings_subs,
            ai_cfg,
            shell_terminal,
            terminal_focus,
            terminal_ime,
            // 终端默认收起：它是「按需叫出来」的东西，一开窗就占半屏反而吵
            shell_visible: false,
            show_tree,
            show_editor,
            show_preview,
            show_toolbar,
            show_statusbar,
            rec: None,
            rec_elapsed: Duration::ZERO,
            rec_finalizing: false,
            rec_polling: false,
            rec_mic,
            rec_cam,
            rec_mic_device: None,
            rec_cam_device: None,
            rec_selftest,
            rec_selftest_started: false,
            theme_name,
            local_defs,
            marks,
            preview_bg,
            follow_cursor,
            follow_polling: false,
            follow_last_cursor: 0,
            follow_pause: None,
            folding,
            diags: Vec::new(),
            tree_entries: 0,
            tree_scanning: false,
            recent_dirs,
            folder_picker: None,
            folder_focus: cx.focus_handle(),
            show_metrics,
            status: Status::default(),
            _sub: sub,
        };
        // 启动就把焦点给编辑器：否则用户打不了字，
        // 而且 gpui 的动作派发**从聚焦节点开始**，没有焦点时
        // Ctrl+S 之类的全局快捷键根本不会触达 on_action。
        this.editor.update(cx, |state, cx| state.focus(window, cx));

        // 终端事件泵：终端在建世界时就起来了，泵等实体建好再挂
        if let Some(term) = this.shell_terminal.clone() {
            this.spawn_terminal_pump(term, cx);
        }

        // **不在这里编译**：45 页冷编译 200+ ms，同步做的话窗口要等它排完
        // 才出现。推迟到第一帧画完（见 `render` 里的 `first_compile`）。
        this
    }

    /// 编辑器内容变化。**先挡一道**：内容没真的变就不排版。
    fn on_editor_change(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        // 光标位置**每次都记**（配对括号高亮与「跟随光标」都要用它）。
        // 这一行必须在下面「文本没变就返回」之前 —— 纯移动光标不改文本，
        // 否则那两个功能就只在打字时才动。
        self.marks.borrow_mut().cursor = self.editor.read(cx).cursor();
        if text == self.last_text {
            return;
        }
        self.mark_edited(cx);
        self.recompile_text(text, cx);
    }

    /// 「文本变了」的唯一记法：置脏 + 给自动保存上闹钟。
    ///
    /// 四处内容变化的入口都走它（编辑器输入 / 格式化 / 工具栏插入 / AI 应用）。
    /// 不直接写 `self.dirty = true` 是为了**不给第五条入口留机会** ——
    /// 漏了自动保存那一半，用户看到的会是「明明写着自动保存，磁盘上却是旧的」。
    fn mark_edited(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self.touch_autosave(cx);
    }

    /// 一次完整的「编辑 → 重排版 → 重新出图」循环。
    ///
    /// 同步执行：排版只要几毫秒，开线程反而引入调度开销与状态同步的麻烦。
    fn recompile(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        self.recompile_text(text, cx);
    }

    /// 同上，但文本由调用方交进来。
    ///
    /// 区别只在**读了几次编辑器**：`InputEvent::Change` 那条路上
    /// `value()` 是整篇复制（大文档每键几十 KB），原来这里又读了一遍 ——
    /// 现在一次读取、一路传下去。
    fn recompile_text(&mut self, text: String, cx: &mut Context<Self>) {
        let main_id = self.engine.entry().main();

        // ① 未保存文本进覆盖层
        self.engine
            .vfs_mut()
            .map_shadow(&self.main_path, Bytes::from_string(text.clone()));
        // ② 增量重解析（不是重建语法树）
        let fed = self.engine.sources().feed_memory(main_id, &text);
        self.status.reparsed = fed.reparsed.map(|r| r.len()).unwrap_or(0);
        self.status.text_bytes = text.len();

        // ③ 排版。「上一次成功的结果」由**引擎**记着 —— 失败时 `compiled.doc`
        //    就是那份旧的，外壳不需要再自己维护一份副本。
        let compiled = self.engine.compile();
        self.status.compile_ms = compiled.elapsed.as_secs_f64() * 1000.0;
        self.status.compiles = self.engine.compile_attempts();

        // 排版结果换了 → 跳转索引作废（下次真的跳的时候再惰性重建）。
        //
        // 失败时索引**不能用**：预览留在屏幕上的是上一次成功的排版，
        // 而源码已经改了 —— 两边的字节偏移对不上，跳过去就是错的行。
        self.index = None;
        self.index_usable = compiled.fresh;
        self.flash = None;

        match compiled.doc {
            Some(doc) => {
                self.status.pages = doc.pages().len();
                // 只有文档真的换了才重新出图。失败时拿到的是同一个 Arc，
                // 重新光栅化纯属浪费。
                let changed = !self.doc.as_ref().is_some_and(|old| Arc::ptr_eq(old, &doc));
                self.doc = Some(doc);
                if changed {
                    self.rerasterize();
                }
            }
            None => {
                self.doc = None;
                self.bitmaps.clear();
                self.texture_bytes = 0;
            }
        }

        // 取包（尤其下载）是同步阻塞的，就卡在这次排版里 —— 把耗时报出来，
        // 「为什么这一下慢」不该靠猜。缓存命中是零点几毫秒，真下载几百毫秒起。
        for fetch in self.engine.packages().take_fetches() {
            let ms = fetch.elapsed.as_secs_f64() * 1000.0;
            logln!(
                "[typst-live] 取包 {}：{ms:.1} ms → {}",
                fetch.spec,
                fetch.path.display()
            );
            if ms > 50.0 {
                self.message = Some(format!("取包 {}（{ms:.0} ms）", fetch.spec));
            }
        }

        if compiled.fresh {
            self.compile_errors.clear();
        } else {
            // 终端里也要能看见为什么失败：预览上的错误框只有开窗的人看得到
            let first = compiled
                .errors
                .first()
                .map(|err| err.message.to_string())
                .unwrap_or_else(|| "（没有诊断信息）".to_owned());
            logln!(
                "[typst-live] 排版失败：{} 条错误，第一条：{first}",
                compiled.errors.len()
            );
            self.compile_errors = compiled.errors;
        }

        // ④ 语法服务。刻意用**引擎里那棵增量维护的树**
        //    （`feed_memory` 刚就地重解析过），而不是 `Source::detached(text)`
        //    再全量 parse 一遍。
        self.refresh_language_services(cx);

        // 记在最后：上面几处只需要借用，原来那份 clone 出来是为了这里 ——
        // 直接把文本交出去（少一次整篇复制）。
        self.last_text = text;

        cx.notify();
    }

    /// 大纲 + 波浪线。两者都基于同一个 `Source`，所以一起算。
    fn refresh_language_services(&mut self, cx: &mut Context<Self>) {
        let Ok(source) = typst::World::source(&self.engine, self.engine.entry().main()) else {
            return;
        };

        self.outline = lang::outline(&source);
        self.stats = lang::text_stats(source.text());

        // 语法错误（不编译就有）与编译错误**形状相同**，所以
        // 交给同一个渲染器，不需要区分来源。
        let mut diags = lang::syntax_diagnostics(&source);
        diags.extend(lang::compile_diagnostics(&source, &self.compile_errors));
        // 同一个问题可能既被解析器报、又被编译器报（`#` 出现在代码里这种），
        // 去重前它会在错误面板里写两遍、状态栏还会把 1 个问题数成 2 个。
        dedupe_diags(&mut diags);
        self.error_count = diags.iter().filter(|d| d.is_error).count();
        // 存一份给错误面板与状态栏用（点一条错误要能跳到那一行）
        self.diags = diags.clone();

        // 本文档定义的名字 → 补全候选（补全得能补出**刚写的那个函数**）
        *self.local_defs.borrow_mut() = lang::definitions(&source);
        // 错误行 / 警告行 → 编辑器里的底色标记；配色从当前主题取
        // （`editor_marks` 不该知道主题长什么样）。
        {
            let theme = cx.theme();
            let mut marks = self.marks.borrow_mut();
            marks.error_lines = diag_lines(&diags, true);
            marks.warning_lines = diag_lines(&diags, false);
            marks.error_color = Some(theme.danger.opacity(0.16));
            marks.warning_color = Some(theme.warning.opacity(0.14));
            // 三档括号底色按嵌套深度轮换（都很淡：它们是背景，不该抢文字）
            marks.bracket_active = Some(theme.primary.opacity(0.38));
        }

        let rope = self.editor.read(cx).text().clone();
        self.editor.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&rope);
                for d in &diags {
                    let Some(start) = d.line_col else { continue };
                    let end = squiggle_end(&source, d, start);
                    let severity = if d.is_error {
                        DiagnosticSeverity::Error
                    } else {
                        DiagnosticSeverity::Warning
                    };
                    set.push(
                        Squiggle::new(
                            Position::new(start.line as u32, start.col as u32)
                                ..Position::new(end.line as u32, end.col as u32),
                            d.message.clone(),
                        )
                        .with_severity(severity),
                    );
                }
            }
            cx.notify();
        });
    }

    /// 切到某个主题并记住它。
    fn use_theme(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        match themes::apply(&name, window, cx) {
            Some(dark) => {
                logln!("[typst-live] 切主题：{} → {name}", describe_theme(dark, cx));
                self.message = Some(format!("主题 → {name}"));
                self.theme_name = Some(name);
                self.touch_settings(cx);
            }
            None => {
                self.message = Some(format!("不认识的主题：{name}"));
                logln!("[typst-live] 不认识的主题：{name}");
            }
        }
        cx.notify();
    }

    // ── 设置持久化 ────────────────────────────────────

    /// 状态变了 → 起一个计时器，停下来之后再写盘。
    ///
    /// 同时存在多个计时器也无所谓：它们做的都是同一件事（把当前状态写下去），
    /// 而且 gpui 的任务都在同一线程上跑，不会写坏文件。
    fn touch_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_dirty = true;
        cx.spawn(async move |this, cx| {
            // 最多重试几次：`Busy` 是借用竞态（可预期、下一拍就好），
            // 不能因为它把这次写盘**丢掉** —— 那一项设置就再也不会落盘了。
            for _ in 0..8 {
                cx.background_executor().timer(SETTINGS_DEBOUNCE).await;
                match safe_task_update(&this, cx, |this, cx| {
                    if this.settings_dirty {
                        this.save_settings(cx);
                    }
                }) {
                    UpdateOutcome::Busy => continue,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// 当前状态「应该写成什么」。
    fn desired_settings(&self) -> Settings {
        let mut next = self.settings.clone();
        next.window = self.pending_window.or(next.window);
        next.file = self.explicit_file.then(|| self.main_path.clone());
        next.zoom = Some(self.zoom);
        next.theme = self.theme_name.clone();
        next.preview_bg = Some(self.preview_bg.as_setting().to_string());
        next.follow_cursor = Some(self.follow_cursor);
        next.folding = Some(self.folding);
        next.show_metrics = Some(self.show_metrics);
        next.autosave = Some(self.autosave);
        next.show_tree = Some(self.show_tree);
        next.show_editor = Some(self.show_editor);
        next.show_preview = Some(self.show_preview);
        next.show_toolbar = Some(self.show_toolbar);
        next.show_statusbar = Some(self.show_statusbar);
        next.record_mic = Some(self.rec_mic);
        next.record_cam = Some(self.rec_cam);
        next.recent_dirs = self.recent_dirs.clone();
        // AI 三项：从 `ai_cfg`（当前想要值）而不是 `settings`（磁盘上那一份）取 ——
        // 否则 `save_settings` 里那句「比一比」永远相等，填的 Key 永远不落盘。
        next.ai_base_url = self.ai_cfg.base_url.clone();
        next.ai_model = self.ai_cfg.model.clone();
        next.ai_api_key = self.ai_cfg.key.clone();
        next
    }

    /// 把「现在想要的」与「已经写下去的那份」比一比，变了才写。
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_dirty = false;

        let next = self.desired_settings();
        if next == self.settings {
            return;
        }
        match next.save() {
            Ok(()) => {
                self.settings_writes += 1;
                logln!(
                    "[typst-live] 设置已写：{}（第 {} 次）",
                    settings::path().display(),
                    self.settings_writes
                );
                self.settings = next;
            }
            Err(err) => logln!("[typst-live] 设置写不进去：{err}"),
        }
        cx.notify();
    }

    /// Ctrl+S：把编辑器里的文本写到 `main_path`。
    fn save_file(&mut self, cx: &mut Context<Self>) {
        if self.write_to_disk(cx) {
            self.message = Some(format!("已保存 {}", self.main_path.display()));
        }
        cx.notify();
    }

    /// 写盘的**唯一一条路**：手动 Ctrl+S 与自动保存都走它。
    ///
    /// 两条路各写一份 `std::fs::write` 的话，迟早有一边忘了撤覆盖层、
    /// 或忘了更新 `saved_text` —— 后者的后果是每一拍都把同一份文本重写一遍。
    ///
    /// 返回写成功了没有。失败时把原因写进 `message`（状态栏那一行是用户
    /// 唯一能看到的地方），但**不动 `dirty`** —— 没写下去就是没写下去。
    fn write_to_disk(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.editor.read(cx).value().to_string();

        match std::fs::write(&self.main_path, &text) {
            Ok(()) => {
                // 磁盘与内存一致了，撤掉覆盖层 —— 从此磁盘就是真相。
                // （主文件的文本还在 SourceDb 里，所以不会因此重新解析。）
                self.engine.vfs_mut().unmap_shadow(&self.main_path);
                self.dirty = false;
                self.saved_text = text;
                // 磁盘上这一份是我们**自己**刚写的：把指纹记成它。
                // 不记的话，下一拍轮询会把这次保存当成外部改动，然后拿同一份
                // 文本重排一遍（还不算错，但白做一次工、白闪一条状态栏）。
                self.disk_stamp = disk_watch::Stamp::of(&self.main_path);
                // 与「磁盘上有新版本，没有重载」那句提示到此为止：
                // 现在磁盘与编辑器一致了（一致到我们这一侧）。
                self.disk_problem = None;
                // 关窗浮层还开着的话，它现在说的「有未保存的改动」已经不成立了：
                // 收掉它（用户想关再点一次 × 就行）。留着比收掉更误导。
                self.close_prompt = false;
                true
            }
            Err(err) => {
                self.message = Some(format!("保存失败：{err}"));
                false
            }
        }
    }

    /// 打字停下来之后自动写盘（间隔见 `autosave::DELAY`）。
    ///
    /// 骨架照抄 `touch_settings`：**多个计时器并存无所谓** —— 它们做的都是
    /// 同一件事（醒来时按**当刻**的文本决定写不写），而 gpui 的任务都在同一
    /// 线程上。所以既不需要序号，也不需要「取消上一个」：后起的那一个醒来时
    /// 看到的就是最新的文本，先起的那一个早写完了（或已被判为「没变化」）。
    fn touch_autosave(&mut self, cx: &mut Context<Self>) {
        if !self.autosave {
            return; // 关着就一个任务也不起
        }
        cx.spawn(async move |this, cx| {
            // `Busy` 是借用竞态（可预期、下一拍就好），不能因为它把这次写盘
            // **丢掉** —— 丢掉的后果是用户以为存了、磁盘上却没有。
            for _ in 0..8 {
                cx.background_executor().timer(autosave::DELAY).await;
                match safe_task_update(&this, cx, |this, cx| this.autosave_now(cx)) {
                    UpdateOutcome::Busy => continue,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// 自动保存的那一拍：**该写才写**（判据在 `autosave::should_save`）。
    fn autosave_now(&mut self, cx: &mut Context<Self>) {
        // 干净文档直接走人：下面要把整篇文本复制出来比一比，
        // 而“没改过”是绝大多数拍的状态（光标移动、窗口重绘都会吵醒计时器）。
        if !self.dirty {
            return;
        }

        let text = self.editor.read(cx).value().to_string();
        let changed = text != self.saved_text;
        if !autosave::should_save(self.autosave, self.explicit_file, changed) {
            return;
        }

        if self.write_to_disk(cx) {
            self.autosave_writes += 1;
            self.message = Some(format!("已自动保存（第 {} 次）", self.autosave_writes));
            logln!(
                "[typst-live] 自动保存：{}（第 {} 次，{}B）",
                short_label(&self.main_path),
                self.autosave_writes,
                text.len()
            );
            cx.notify();
        }
        // 失败：`dirty` 保持 true，**不重试、不再刷消息** —— 只读文件上重试
        // 就是每秒往状态栏上盖一条「保存失败」。下一次编辑会重新起计时器，
        // 那时再试一次（用户也可能就在这中间把它改可写了）。
    }

    /// 自动保存开关（`Ctrl+Alt+S` 或点状态栏那一格）。
    fn set_autosave(&mut self, on: bool, cx: &mut Context<Self>) {
        self.autosave = on;
        self.touch_settings(cx);
        if on {
            // 打开时立刻补一次：否则「刚打完字 → 打开自动保存」要等到**下次**
            // 编辑才落盘，而那可能是一个小时以后。
            self.touch_autosave(cx);
        }
        self.message = Some(
            if on {
                "自动保存：开（停下 1 秒后写盘；内置示例文档永远不写）"
            } else {
                "自动保存：关（只有 Ctrl+S 会写盘）"
            }
            .to_string(),
        );
        cx.notify();
    }

    // ── 磁盘上的外部改动 ──────────────────────────────────

    /// 外部改动的轮询循环（`disk_watch::POLL` 一拍）。
    ///
    /// 「外部改动」= 任何不是这个应用写出来的变化：别的编辑器、脚本、agent、
    /// `git checkout`。首帧之后才起（见 `render`）—— 那之前没有窗口，也没人
    /// 等着重读。窗口关掉后实体释放，循环自己退出（`safe_task_update` 报 `Gone`）。
    ///
    /// 只盯**当前打开的主文件**。被 `#include` 进来的外部文件的改动仍然靠
    /// `Ctrl+B`（那一下会把源缓存全部作废重读）。
    fn spawn_disk_watch(&mut self, cx: &mut Context<Self>) {
        if self.disk_polling {
            return;
        }
        self.disk_polling = true;

        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(disk_watch::POLL).await;

                // ① 盯的是哪个文件、我们记着磁盘上是什么样。
                //    内置示例文档（`explicit_file == false`）不走这条路。
                let watched = match safe_task_read(&weak, cx, |this, _| {
                    this.explicit_file
                        .then(|| (this.main_path.clone(), this.disk_stamp))
                }) {
                    UpdateOutcome::Done(Some(watched)) => watched,
                    // 没盯的文件，或这一拍撞上借用竞态：下一拍再来
                    UpdateOutcome::Done(None) | UpdateOutcome::Busy => continue,
                    UpdateOutcome::Gone => break,
                };
                let (path, known) = watched;

                // ② 取指纹。**放后台线程**：本地盘上几微秒，网络盘上可能几十毫秒，
                //    而这是每 500 ms 一次的常驻动作，不该压在 UI 线程上。
                let probe = path.clone();
                let now = cx
                    .background_executor()
                    .spawn(async move { disk_watch::Stamp::of(&probe) })
                    .await;

                if !disk_watch::changed(known, now) {
                    continue;
                }

                // ③ 文件不在了：记一句（只记一次），编辑器里的字一个字不动。
                //    指纹记成「没有」—— 文件真被重新创建时，那又是一次该重读的变化。
                let Some(now) = now else {
                    let _ = safe_task_update(&weak, cx, |this, cx| {
                        this.note_disk_problem(
                            format!("「{}」在磁盘上不见了（编辑器里的内容没动）", short_label(&path)),
                            None,
                            cx,
                        );
                    });
                    continue;
                };

                // ④ 防抖：等指纹连续两拍不动。编辑器保存是「临时文件 + rename」，
                //    刚发现变化时读到的可能就是半截文件。
                let mut stamp = now;
                let mut settled = false;
                for _ in 0..disk_watch::SETTLE_ROUNDS {
                    cx.background_executor().timer(disk_watch::SETTLE).await;
                    let probe = path.clone();
                    let next = cx
                        .background_executor()
                        .spawn(async move { disk_watch::Stamp::of(&probe) })
                        .await;
                    match next {
                        // 不动了：可以读了
                        Some(next) if !next.differs(&stamp) => {
                            settled = true;
                            break;
                        }
                        // 还在动（正在被写）：记下新的，再等一拍
                        Some(next) => stamp = next,
                        // 这中间被删了：交给下一拍按「文件不见了」处理
                        None => break,
                    }
                }
                if !settled {
                    continue;
                }

                // ⑤ 该不该拿回来。判据是纯函数（`disk_watch::verdict`，有单测）：
                //    本地有未保存改动时**一个字都不动** —— 那是丢数据的口子。
                match safe_task_read(&weak, cx, |this, _| {
                    disk_watch::verdict(this.explicit_file, this.dirty)
                }) {
                    UpdateOutcome::Done(disk_watch::Verdict::Reload) => {}
                    UpdateOutcome::Done(_) => {
                        let _ = safe_task_update(&weak, cx, |this, cx| {
                            this.note_disk_problem(
                                format!(
                                    "「{}」在磁盘上被改过 —— 你有未保存的修改，没有重载（保存后以你为准）",
                                    short_label(&path)
                                ),
                                Some(stamp),
                                cx,
                            );
                        });
                        continue;
                    }
                    UpdateOutcome::Busy => continue,
                    UpdateOutcome::Gone => break,
                }

                // ⑥ 读盘（同样在后台）。
                let target = path.clone();
                match cx
                    .background_executor()
                    .spawn(async move { std::fs::read_to_string(&target) })
                    .await
                {
                    Ok(text) => {
                        // ⑦ 交给编辑器 —— **不当场改**：`set_value` 要 `&mut Window`，
                        //    这里没有。挂在 `pending_reload` 上，下一帧 `render` 里装。
                        let _ = safe_task_update(&weak, cx, |this, cx| {
                            this.disk_stamp = Some(stamp);
                            this.pending_reload = Some((text, stamp));
                            cx.notify();
                        });
                    }
                    // 读不动（权限 / 不是 UTF-8）：记一句，指纹记成这一份 ——
                    // 否则每一拍都要重读同一次失败。
                    Err(err) => {
                        let _ = safe_task_update(&weak, cx, |this, cx| {
                            this.note_disk_problem(
                                format!(
                                    "磁盘上的「{}」读不了：{err}（编辑器里的内容没动）",
                                    short_label(&path)
                                ),
                                Some(stamp),
                                cx,
                            );
                        });
                    }
                }
            }

            // 走到这儿说明实体没了（窗口关了）—— 复位标志，让循环能再起一个
            // （同一个实体理论上不会再渲染，但状态别留成「已经在跑」）。
            let _ = safe_task_update(&weak, cx, |this, _cx| this.disk_polling = false);
        })
        .detach();
    }

    /// 外部改动**没能**落地时的一句话（状态栏 + 日志），并把指纹记成现在这一份。
    ///
    /// 同一句话只说一次：轮询每 500 ms 一拍，不记着就会刷屏。而「指纹记下来」
    /// 等于说「这件事我们知道了」—— 不记的话每一拍都会重新发现同一个改动、
    /// 重新防抖一轮，然后再说一遍同样的话。
    fn note_disk_problem(
        &mut self,
        problem: String,
        stamp: Option<disk_watch::Stamp>,
        cx: &mut Context<Self>,
    ) {
        self.disk_stamp = stamp;
        if self.disk_problem.as_deref() == Some(problem.as_str()) {
            return;
        }
        logln!("[typst-live] {problem}");
        self.disk_problem = Some(problem.clone());
        self.message = Some(problem);
        cx.notify();
    }

    /// 把磁盘上的新版本装进编辑器。
    ///
    /// 在 `render` 那一帧之后做（`set_value` 要 `&mut Window`，见
    /// [`Previewer::spawn_disk_watch`] 的 ⑦）。跟「打开文件」走的是同一条路：
    /// 文本进编辑器 → 覆盖层与语法树跟着更新（`recompile`）→ 重排版。
    fn apply_disk_reload(
        &mut self,
        text: String,
        stamp: disk_watch::Stamp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 从「发现变化」到「这一帧」之间用户可能又敲了键。那一刻起**本地才是
        // 更新的那一份** —— 让给磁盘就是把他刚敲的字删了。轮询里判过一次，
        // 这里再判一次：真正动文本的是这一处。
        if self.dirty {
            self.note_disk_problem(
                format!(
                    "「{}」在磁盘上被改过 —— 你有未保存的修改，没有重载（保存后以你为准）",
                    short_label(&self.main_path)
                ),
                Some(stamp),
                cx,
            );
            return;
        }

        // 光标与视图：外部改动挪动了字节偏移，按字节对位没有意义，但「光标跳回
        // 第一行、视图滚到顶」会让正在读的人丢掉位置。按行列还回去，越界夹住。
        let (position, offset) = {
            let state = self.editor.read(cx);
            let cursor = state.cursor();
            (
                state.text().offset_to_position(cursor),
                state.scroll_offset(),
            )
        };
        let (line, character) =
            disk_watch::clamp_position(position.line, position.character, &text);

        // `set_value` 走的是**静默路径**（不发 `Change` 事件），所以重排要自己发
        // （下面那句 `recompile`）—— 不然编辑器里是新文本、预览还是旧的。
        self.saved_text = text.clone();
        self.editor
            .update(cx, |state, cx| state.set_value(text, window, cx));

        if self.editor_focused {
            // 焦点在编辑器里：光标也放回去。`set_cursor_position` 内部会
            // `scroll_to`，于是视图跟着回到光标附近。
            let position = Position::new(line, character);
            self.editor.update(cx, |state, cx| {
                state.set_cursor_position(position, window, cx)
            });
        } else {
            // 焦点在终端 / AI 浮层上：**不碰光标** —— `set_cursor_position` 会
            // 把焦点抢到编辑器里，用户正打在终端里的字就跑到文档里去了。
            // 只把视图滚回原处（`set_value` 会把它推到顶部）。
            self.editor
                .update(cx, |state, cx| state.set_scroll_offset(offset, cx));
        }

        // 被 `#include` 进来的外部文件多半也在这批改动里：一起作废、下次重排
        // 重新读盘（`Ctrl+B` 做的就是这件事）。
        self.engine.sources().invalidate_all();

        self.dirty = false;
        self.disk_stamp = Some(stamp);
        self.disk_problem = None;
        self.disk_reloads += 1;
        self.recompile(cx);

        self.message = Some(format!(
            "已重新加载「{}」（磁盘上有新版本，第 {} 次）",
            short_label(&self.main_path),
            self.disk_reloads
        ));
        logln!(
            "[typst-live] 外部改动 → 重读 {}（{}B，第 {} 次）",
            short_label(&self.main_path),
            self.saved_text.len(),
            self.disk_reloads
        );
        cx.notify();
    }

    // ── 关窗 ──────────────────────────────────────────

    /// 平台的关窗请求。返回 `true` 才真的关。
    ///
    /// 这个应用**除了保存不往磁盘写字**（写盘只有保存与导出；保存又分手动
    /// `Ctrl+S` 与自动保存两条路）—— 所以「关窗」仍然可能丢掉刚敲、还没来得及
    /// 落盘的东西（自动保存关着时，或那 1 秒的空闲还没到）。有未保存改动就先
    /// 拦住，把提示浮层画出来（见 `render_close_prompt`）—— 与 `wu` 那次
    /// 「输入时光标跳回第一行」同一类教训：**别让用户在不被告知的情况下丢东西**。
    fn on_close_requested(&mut self, cx: &mut Context<Self>) -> bool {
        if self.close_confirmed || !self.dirty {
            // ⚠️ 这里**不设** `close_confirmed`：它只该由「用户在浮层上明确选过」
            // 的那两条路径（保存并关闭 / 放弃改动）置位。否则「没改动 → 放行」这一
            // 次如果因为别的原因没真的关掉，用户接着又改了东西，下一次关窗就会被
            // 这一位直接放行 —— 那正是这个钩子要防的事。
            // 关窗前先把录像收干净（同步收尾 —— 见 `finish_recording_on_close`）
            self.finish_recording_on_close();
            self.shutdown_terminal();
            return true;
        }

        if !self.close_prompt {
            self.close_prompt = true;
            logln!("[typst-live] 关窗请求：有未保存改动，先问一句");
            cx.notify();
        }
        false
    }

    /// 收掉交互式终端（真的关窗时调）。
    ///
    /// `Msg::Shutdown` 让 alacritty 的 `EventLoop` 退出（`process_events` 返回 false），
    /// 它持有的 ConPTY 随之析构 —— 关闭伪控制台会结束附着在上面的子进程。
    /// 不调的话，子进程与 pty 线程就指望「进程退出」替我们收拾。
    fn shutdown_terminal(&mut self) {
        if let Some(term) = &self.shell_terminal {
            term.shutdown();
            logln!("[typst-live] 终端已收（关窗）");
        }
    }

    /// 关窗提示：保存并关闭。
    fn close_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_file(cx);
        if self.dirty {
            // 保存失败（只读 / 被占 / 没权限）：**不关**，并把浮层收掉 ——
            // 状态栏上那条「保存失败：…」是用户唯一能看到的原因，浮层压着它反而读不到。
            // 文字还在编辑器里，人没有丢东西。
            self.close_prompt = false;
            cx.notify();
            return;
        }
        self.close_prompt = false;
        self.close_confirmed = true;
        self.shutdown_terminal();
        window.remove_window();
    }

    /// 关窗提示：放弃改动并关闭。
    fn close_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        logln!(
            "[typst-live] 关窗：放弃未保存的改动（{}）",
            short_label(&self.main_path)
        );
        self.dirty = false;
        self.close_prompt = false;
        self.close_confirmed = true;
        self.shutdown_terminal();
        window.remove_window();
        cx.notify();
    }

    /// 关窗提示：取消（继续编辑）。
    fn close_cancel(&mut self, cx: &mut Context<Self>) {
        self.close_prompt = false;
        cx.notify();
    }

    // ── 编辑器与预览的显示开关 ──────────────────────────

    /// 终端的显示/收起。`Ctrl+4` 与状态栏那个「终端」按钮共用这一份。
    fn toggle_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell_visible = !self.shell_visible;
        // 显示时把焦点交给终端，否则得先点一下才能打字
        if self.shell_visible {
            self.terminal_focus.focus(window, cx);
        }
        cx.notify();
    }

    // ------------------------------------------------------------------
    // 录屏
    // ------------------------------------------------------------------

    /// 「视图」菜单里的显隐开关。
    fn set_pane_visible(&mut self, which: PaneToggle, on: bool, cx: &mut Context<Self>) {
        match which {
            PaneToggle::Tree => self.show_tree = on,
            PaneToggle::Editor => self.show_editor = on,
            PaneToggle::Preview => self.show_preview = on,
            PaneToggle::Toolbar => self.show_toolbar = on,
            PaneToggle::Statusbar => self.show_statusbar = on,
        }
        logln!("[typst-live] 显隐：{which:?} → {on}");
        self.touch_settings(cx);
        cx.notify();
    }

    fn set_record_mic(&mut self, on: bool, cx: &mut Context<Self>) {
        self.rec_mic = on;
        self.touch_settings(cx);
        cx.notify();
    }

    fn set_record_cam(&mut self, on: bool, cx: &mut Context<Self>) {
        self.rec_cam = on;
        self.touch_settings(cx);
        cx.notify();
    }

    /// 开始 / 停止。工具栏按钮、菜单项、快捷键都走这里。
    fn toggle_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rec.is_some() {
            self.stop_recording(cx);
        } else {
            self.start_recording(window, cx);
        }
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rec_finalizing {
            self.message = Some("上一段的收尾还在跑，等它完".into());
            cx.notify();
            return;
        }
        let region = match self.capture_region(window) {
            Ok(region) => region,
            Err(why) => {
                self.message = Some(why);
                cx.notify();
                return;
            }
        };

        // 设备名枚举一次就缓存：枚举要起一个 ffmpeg（几百毫秒），不能每秒都来一遍。
        if self.rec_mic_device.is_none() || self.rec_cam_device.is_none() {
            let devices = record::list_devices();
            self.rec_mic_device = devices.audio.first().cloned();
            self.rec_cam_device = devices.video.first().cloned();
        }
        let mic = if self.rec_mic {
            self.rec_mic_device.clone()
        } else {
            None
        };
        let cam = if self.rec_cam {
            self.rec_cam_device.clone()
        } else {
            None
        };

        let (dir, dir_note) = record::out_dir();
        match record::Recorder::start(&dir, region, mic.clone(), cam.clone(), record::FPS) {
            Ok(rec) => {
                self.rec_elapsed = Duration::ZERO;
                self.rec = Some(rec);
                self.message = Some(
                    match (dir_note, mic.is_none(), cam.is_none() && self.rec_cam) {
                        (Some(note), ..) => note,
                        (None, true, _) => "录制中（没找到麦克风设备 → 只有画面）".into(),
                        (None, _, true) => "录制中（没找到摄像头设备 → 没有画中画）".into(),
                        _ => "录制中".into(),
                    },
                );
                logln!(
                    "[typst-live] 开始录屏：区域 {}x{} @({}, {}) → {}",
                    region.w,
                    region.h,
                    region.x,
                    region.y,
                    dir.display()
                );
                self.spawn_rec_poll(cx);
            }
            Err(err) => self.message = Some(format!("录不了：{err}")),
        }
        // 自测里起不来就别挂着：把原因打出来退出，不然一条命令就卡在那儿了。
        if self.rec.is_none() && self.rec_selftest.is_some() {
            let text = self.message.clone().unwrap_or_else(|| "没起来".into());
            self.selftest_report(&text, cx);
        }
        cx.notify();
    }

    /// 窗口矩形 → 采集区域。所有「这块屏 / 这个窗口行不行」的判断都在这里。
    fn capture_region(&self, window: &Window) -> Result<record::Rect, String> {
        let hwnd = record::hwnd_of(window).ok_or("拿不到窗口句柄，这次录不了")?;
        let (monitor, primary) =
            record::monitor_rect(hwnd).ok_or("问不出显示器范围，这次录不了")?;
        if !primary {
            return Err("录屏只支持主显示器（gdigrab 的桌面以主屏左上角为原点）".into());
        }
        let win = record::window_rect(hwnd).ok_or("量不出窗口大小，这次录不了")?;
        record::crop_region(win, monitor, record::MIN_SIDE).ok_or_else(|| {
            format!(
                "窗口太小 / 露出屏幕太少（{}x{}），至少 {}x{} 才录",
                win.w,
                win.h,
                record::MIN_SIDE,
                record::MIN_SIDE
            )
        })
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(mut rec) = self.rec.take() else {
            return;
        };
        self.rec_elapsed = rec.elapsed();
        match rec.stop() {
            Ok(plan) => {
                // 拼接 / 合成可能要几秒到几十秒（画中画那一步要重编码）——放后台，
                // 界面上继续能干活。
                self.rec_finalizing = true;
                self.message = Some("收尾中（拼接 / 画中画合成）…".into());
                let weak = cx.entity().downgrade();
                cx.spawn(async move |_this, cx| {
                    let done = cx
                        .background_executor()
                        .spawn(async move { plan.run() })
                        .await;
                    let _ = safe_task_update(&weak, cx, |this, cx| this.on_finalize_done(done, cx));
                })
                .detach();
            }
            Err(err) => {
                self.rec_finalizing = false;
                self.message = Some(format!("收尾失败：{err}"));
            }
        }
        cx.notify();
    }

    fn on_finalize_done(&mut self, done: Result<record::Done, String>, cx: &mut Context<Self>) {
        self.rec_finalizing = false;
        self.message = Some(match done {
            Ok(done) => {
                let mut text = format!("已保存 {}", done.path.display());
                for note in done.notes {
                    text.push_str(&format!("（{note}）"));
                }
                logln!("[typst-live] {text}");
                text
            }
            Err(err) => format!("收尾失败：{err}"),
        });
        if let Some(text) = self.message.clone() {
            self.selftest_report(&text, cx);
        }
        cx.notify();
    }

    /// `--rec-selftest` 的收尾：打印一句然后退出（不在自测里就什么都不做）。
    fn selftest_report(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.rec_selftest.take().is_none() {
            return;
        }
        println!("[rec-selftest] {text}");
        cx.quit();
    }

    /// 暂停 / 继续。
    fn toggle_record_pause(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.rec.as_mut() else {
            return;
        };
        let outcome = match rec.state() {
            record::State::Recording => rec.pause(),
            record::State::Paused => rec.resume(record::FPS),
            record::State::Finalizing | record::State::Idle => Ok(()),
        };
        // 暂停是真的「把这一段收掉了」：继续时开新的一段，停止时按段拼接，
        // 所以暂停处不会在成品里变成一个洞。
        self.message = Some(match outcome {
            Ok(()) if rec.state() == record::State::Paused => {
                "已暂停（继续时接着录，拼接处不留洞）".into()
            }
            Ok(()) => "录制中".into(),
            Err(err) => format!("暂停 / 继续失败：{err}"),
        });
        cx.notify();
    }

    /// 每秒一拍：刷计时 + 看 ffmpeg 是不是偷偷退了。
    fn spawn_rec_poll(&mut self, cx: &mut Context<Self>) {
        if self.rec_polling {
            return;
        }
        self.rec_polling = true;
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(REC_POLL).await;
                let outcome = safe_task_update(&weak, cx, |this, cx| {
                    let rec = this.rec.as_mut()?;
                    let fresh = rec.tick();
                    let elapsed = rec.elapsed();
                    let changed = elapsed != this.rec_elapsed;
                    this.rec_elapsed = elapsed;
                    if fresh.is_some() || changed {
                        cx.notify();
                    }
                    fresh
                });
                match outcome {
                    UpdateOutcome::Done(Some(note)) => {
                        let _ = safe_task_update(&weak, cx, |this, cx| {
                            this.message = Some(note);
                            cx.notify();
                        });
                    }
                    UpdateOutcome::Done(None) | UpdateOutcome::Busy => {}
                    UpdateOutcome::Gone => break,
                }
            }
        })
        .detach();
    }

    /// 关窗前把录像收干净（**同步**跑完收尾）。
    ///
    /// 为什么不丢给后台执行器：任务会随进程一起消失，而用户完全可能刚录完就关窗 ——
    /// 那时只给他一堆分段文件就是丢东西。这里卡的只是「关窗」这一步，界面本来就要
    /// 没了，卡一下比丢录像强（没开画中画时通常就是一次 rename，瞬时）。
    fn finish_recording_on_close(&mut self) {
        let Some(mut rec) = self.rec.take() else {
            return;
        };
        match rec.stop() {
            Ok(plan) => match plan.run() {
                Ok(done) => logln!("[typst-live] 关窗前收尾完成：{}", done.path.display()),
                Err(err) => logln!(
                    "[typst-live] 关窗前收尾失败：{err}（分段文件留在 {}）",
                    rec.work_dir().display()
                ),
            },
            Err(err) => logln!("[typst-live] 关窗前停止录制失败：{err}"),
        }
    }

    /// 状态栏要不要显示性能指标（排版/光栅化/重解析/纹理……）。
    fn set_show_metrics(&mut self, on: bool, cx: &mut Context<Self>) {
        self.show_metrics = on;
        self.touch_settings(cx);
        self.message = Some(
            if on {
                "状态栏：显示性能指标"
            } else {
                "状态栏：精简模式"
            }
            .to_string(),
        );
        cx.notify();
    }

    // ── 文件夹 ──────────────────────────────────────────

    /// 打开「选择文件夹」浮层（从当前树根开始）。
    fn open_folder_picker(&mut self, cx: &mut Context<Self>) {
        self.folder_picker = Some(FolderPicker::new(self.root.clone()));
        cx.notify();
    }

    fn folder_picker_into(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if let Some(picker) = self.folder_picker.as_mut() {
            picker.enter_dir(dir);
        }
        cx.notify();
    }

    fn folder_picker_up(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = self.folder_picker.as_mut() {
            picker.up();
        }
        cx.notify();
    }

    fn folder_picker_cancel(&mut self, cx: &mut Context<Self>) {
        self.folder_picker = None;
        cx.notify();
    }

    /// 把选择器里当前这个目录定为树根（并记进「最近文件夹」）。
    fn folder_picker_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.folder_picker.take() else {
            return;
        };
        self.set_root_dir(picker.cwd, cx);
    }

    /// 换目录树根：重扫目录 + 记进最近列表 + 提示一句。
    ///
    /// **不改排版根**：`#include "x.typ"` 是相对主文件所在目录解析的，
    /// 跟着树根走会把已经跑通的文档编译搞坏。这里换的是「工作区目录」。
    fn set_root_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !dir.is_dir() {
            self.message = Some(format!(
                "{} 不是一个目录",
                short_path(&dir.to_string_lossy())
            ));
            cx.notify();
            return;
        }

        self.root = dir.clone();
        self.push_recent_dir(dir.clone());
        self.refresh_tree(cx);
        self.message = Some(format!(
            "工作区目录：{}",
            short_path(&dir.to_string_lossy())
        ));
        cx.notify();
    }

    /// 记一条最近文件夹（同一个只留最新那次，最多 `MAX_RECENT_DIRS` 条）。
    fn push_recent_dir(&mut self, dir: PathBuf) {
        self.recent_dirs.retain(|known| known != &dir);
        self.recent_dirs.insert(0, dir);
        self.recent_dirs.truncate(MAX_RECENT_DIRS);
    }

    /// 用「最近文件夹」里的第 `index` 条。
    fn use_recent_dir(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(dir) = self.recent_dirs.get(index).cloned() else {
            return;
        };
        if !dir.is_dir() {
            // 目录没了（U 盘拔了、文件夹改名了）：从列表里摘掉，别留着点了没反应
            self.recent_dirs.remove(index);
            self.message = Some(format!(
                "{} 已经不存在了，从最近列表里去掉",
                short_path(&dir.to_string_lossy())
            ));
            self.touch_settings(cx);
            cx.notify();
            return;
        }
        self.set_root_dir(dir, cx);
    }

    /// 代码折叠开关（库默认开着；这里给它一个键盘/菜单入口）。
    ///
    /// 折叠区间是库自动从语法树里提取的（任何跨 ≥2 行的节点），
    /// 但**折叠图标只在鼠标悬停行号列、或光标所在行、或已折叠时才画** ——
    /// 所以「看不见箭头」是正常的，把鼠标移到行号列上就出来了。
    fn set_folding(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.folding = on;
        self.editor
            .update(cx, |state, cx| state.set_folding(on, window, cx));
        self.touch_settings(cx);
        self.message = Some(
            if on {
                "代码折叠：开（鼠标移到行号列，箭头出现；点它折叠）"
            } else {
                "代码折叠：关"
            }
            .to_string(),
        );
        cx.notify();
    }

    /// 预览区背景样式：纯色 / 网格。
    fn set_preview_bg(&mut self, style: PreviewBg, cx: &mut Context<Self>) {
        self.preview_bg = style;
        self.touch_settings(cx);
        self.message = Some(format!("预览背景：{}", style.label()));
        cx.notify();
    }

    /// 跟随光标开关：开着时光标一停，预览就滚到对应位置。
    fn set_follow_cursor(&mut self, on: bool, cx: &mut Context<Self>) {
        self.follow_cursor = on;
        self.touch_settings(cx);
        if on {
            self.follow_last_cursor = self.editor.read(cx).cursor();
            self.spawn_follow_poll(cx);
        }
        self.message = Some(
            if on {
                "跟随光标：开（光标停下 → 预览跟着滚；手动滚动会暂停 3 秒）"
            } else {
                "跟随光标：关"
            }
            .to_string(),
        );
        cx.notify();
    }

    /// 跟随光标的轮询循环（400ms 一次）。
    ///
    /// 只在开关打开时存在：关掉后循环自己退出并把标志复位 ——
    /// **不跟随 = 零空转**（与 `wu` 的跟随同一个套路）。
    fn spawn_follow_poll(&mut self, cx: &mut Context<Self>) {
        if self.follow_polling {
            return;
        }
        self.follow_polling = true;

        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;

                match safe_task_update(&weak, cx, |this, cx| {
                    if !this.follow_cursor {
                        return false; // 关了：收工
                    }
                    // 用户刚手动滚过预览：静默期内不抢他的滚动位置
                    if this
                        .follow_pause
                        .is_some_and(|since| since.elapsed() < FOLLOW_SILENCE)
                    {
                        return true;
                    }
                    let cursor = this.editor.read(cx).cursor();
                    if cursor != this.follow_last_cursor {
                        this.follow_last_cursor = cursor;
                        this.follow_cursor_now(cx);
                    }
                    true
                }) {
                    UpdateOutcome::Done(true) => {}
                    // 借用竞态：下一拍再来；实体没了也收工
                    UpdateOutcome::Busy => {}
                    _ => break,
                }
            }

            // 复位标志，下次开启还能再起一个循环
            let _ = safe_task_update(&weak, cx, |this, _cx| this.follow_polling = false);
        })
        .detach();
    }

    /// Ctrl+B：手动重新编译。
    ///
    /// 不只是「再排一次」—— 它先把源文件缓存**全部作废**，
    /// 这样被 `#include` 的文件如果被外部改过也能重新读进来
    /// （自动重读（`disk_watch`）只盯当前主文件，所以这一下是**被 include 的文件**
    /// 那条路上的补丁）。
    /// 主文件的未保存编辑不会丢：它的文本同时存在于 VFS 覆盖层里。
    fn recompile_now(&mut self, cx: &mut Context<Self>) {
        self.engine.sources().invalidate_all();
        self.message = Some("已重新编译（源文件缓存已作废）".to_owned());
        self.recompile(cx);
    }

    /// Ctrl+Shift+F：进程内格式化。
    fn format_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();

        match typst_engine::format::format(&text) {
            Ok(formatted) if formatted == text => {
                self.message = Some("已经是格式化状态".to_owned());
            }
            Ok(formatted) => {
                // `set_value` 刻意不发 Change 事件（`emit_events = false`），
                // 所以得自己触发一次重排。
                self.editor
                    .update(cx, |state, cx| state.set_value(formatted, window, cx));
                self.mark_edited(cx);
                self.message = Some("已格式化".to_owned());
                self.recompile(cx);
            }
            Err(err) => {
                self.message = Some(format!("格式化失败（语法可能不完整）：{err}"));
            }
        }
        cx.notify();
    }

    /// Ctrl+E：导出 PDF。
    ///
    /// 手上已经有 `PagedDocument`，所以不重新排版、不重读文件、不起子进程。
    fn export_pdf(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else {
            self.message = Some("还没有可导出的排版结果".to_owned());
            cx.notify();
            return;
        };

        let out = self.main_path.with_extension("pdf");
        self.message = Some(match export_pdf(&doc) {
            Ok(bytes) => match std::fs::write(&out, bytes) {
                Ok(()) => format!("已导出 {}", out.display()),
                Err(err) => format!("写 PDF 失败：{err}"),
            },
            Err(errors) => format!("导出失败：{} 条诊断", errors.len()),
        });
        cx.notify();
    }

    /// 大纲点击：把光标移到那一行。
    fn jump_to_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.set_cursor_position(Position::new(line as u32, 0), window, cx);
        });
        self.message = Some(format!("已跳到第 {} 行", line + 1));
        cx.notify();
    }

    // ── 工具栏 ────────────────────────────────────────

    /// 工具栏按钮：按 [`Markup`] 改一次编辑器内容。
    ///
    /// 只有空范围的插入需要先挪光标（`insert` 在光标处插）；
    /// 包住选区那种直接用 `replace` 顶掉当前选区即可。
    ///
    /// 注意 `insert` / `replace` 走的是 gpui-component 的**静默**路径
    /// （不发 `InputEvent::Change`），所以这里得自己触发一次重排 ——
    /// `format_document` 也踩过同一条。
    fn apply_markup(&mut self, kind: Markup, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selection) = {
            let state = self.editor.read(cx);
            (state.text().to_string(), state.selected_range())
        };
        let edit = markup::edit(&text, selection, kind);

        self.editor.update(cx, |state, cx| {
            if edit.range.is_empty() {
                let position = state.text().offset_to_position(edit.range.start);
                state.set_cursor_position(position, window, cx);
                state.insert(edit.text.clone(), window, cx);
            } else {
                state.replace(edit.text.clone(), window, cx);
            }
            // 光标落点自己定：插空壳时要夹在中间，不然接着打字会打到壳外面
            let position = state.text().offset_to_position(edit.cursor);
            state.set_cursor_position(position, window, cx);
        });

        self.mark_edited(cx);
        self.recompile(cx);
        self.message = Some(format!("已插入{}", kind.label()));
        cx.notify();
    }

    // ── 终端 ──────────────────────────────────────────

    /// 终端事件泵：**自适应轮询**。
    ///
    /// 有输出时 50ms（回显跟手），连续空闲 2 秒后降到 120ms ——
    /// `wu` 特意从固定 50ms 改过来的：固定间隔在空闲时也是 20Hz 常量唤醒，
    /// 白烧 CPU；120ms 上限又保证空闲后第一次敲键的回显延迟肉眼不可感。
    fn spawn_terminal_pump(&self, term: Rc<terminal::Terminal>, cx: &mut Context<Self>) {
        let view = cx.entity();
        cx.spawn(async move |_weak, cx| {
            let mut idle_rounds: u32 = 0;
            loop {
                let interval = if idle_rounds > 40 { 120 } else { 50 };
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;

                let events = term.drain_events();
                if events.is_empty() {
                    idle_rounds = idle_rounds.saturating_add(1);
                    continue;
                }
                idle_rounds = 0;

                let has_wakeup = events
                    .iter()
                    .any(|event| matches!(event, terminal::TermEvent::Wakeup));
                let exited = events.iter().find_map(|event| match event {
                    terminal::TermEvent::ChildExit(code) => Some(*code),
                    _ => None,
                });

                // 视图没了（关窗）就收工。原来这里把 `Err` 吞了 ——
                // 循环会一直 50/120ms 空转到进程结束，白烧 CPU。
                let outcome = safe_entity_update(&view, cx, |this, cx| {
                    if let Some(code) = exited {
                        this.message = Some(format!("终端已退出（退出码 {code:?}）"));
                    }
                    if has_wakeup {
                        cx.notify();
                    }
                });
                if matches!(outcome, UpdateOutcome::Gone) {
                    break;
                }

                // 子进程走了就再没有事件了（pty 已 EOF），留着也是空转。
                if exited.is_some() {
                    break;
                }
            }
        })
        .detach();
    }

    // ── AI 编辑（Ctrl+K）─────────────────────────────────

    /// Ctrl+K：打开 AI 编辑。有选区就改选区，没有就改整篇。
    fn open_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 有选中就改选中那段，没选就改**光标所在段落**（不是整篇）。
        //
        // 这个默认值是照 `wu` 的对话框定的：人按 Ctrl+K 的时候，脑子里想的
        // 是「眼前这一段」；想改全文，浮层上点一下「全文」就行（见
        // `default_ai_scope` 与 `set_ai_scope`）。
        let scope = self.default_ai_scope(cx);
        self.open_ai_overlay(scope, window, cx);
    }

    /// 目录树右键 / AI 菜单：**处理一个文件**（未必是编辑器里打开的那个）。
    fn open_ai_for_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_ai_overlay(AiScope::File(path), window, cx);
    }

    /// `Ctrl+K` 的默认范围：有选区就是选区，否则**光标段落**。
    fn default_ai_scope(&self, cx: &Context<Self>) -> AiScope {
        let selection = self.editor.read(cx).selected_range();
        if !selection.is_empty() {
            return AiScope::Selection(selection);
        }
        let doc = self.editor.read(cx).text().to_string();
        let cursor = self.editor.read(cx).cursor();
        AiScope::Paragraph(ai_scope::paragraph_range(&doc, cursor, ai_scope::PARA_PAD))
    }

    /// 菜单里五个固定任务的范围：有选中就是选区，否则**整篇**。
    ///
    /// 与 `Ctrl+K` 的默认值不同是故意的（也是 `wu` 的做法）：「校对 / 术语 /
    /// 互译」是对**文件**提的要求，默认成光标段落会让它们静默地只做一点点。
    fn current_ai_scope(&self, cx: &Context<Self>) -> AiScope {
        let selection = self.editor.read(cx).selected_range();
        if selection.is_empty() {
            AiScope::Document
        } else {
            AiScope::Selection(selection)
        }
    }

    /// 目录树里选中的那个文件 —— 有它、且**不是**当前打开的那个时返回。
    ///
    /// 「AI 处理选中的文件」这条入口用（见 AI 菜单）：与当前文档同一个就没必要
    /// 列出来（那一条就是「AI 编辑…」，同一个东西两个入口只会让人犹豫点哪个）。
    fn ai_menu_file(&self, cx: &App) -> Option<PathBuf> {
        let item = self.tree_state.read(cx).selected_item()?;
        let path = PathBuf::from(item.id.as_ref());
        (path.is_file() && path != self.main_path).then_some(path)
    }

    /// 浮层上该给哪几个范围按钮。
    ///
    /// 没有选区就不给「选区」：点了也没东西可改，摆在那里只会让人以为能点
    /// （`wu` 的对话框也是这么做的）。
    fn ai_scope_choices(&self, cx: &Context<Self>) -> Vec<ScopeChoice> {
        let mut choices = Vec::new();
        if !self.editor.read(cx).selected_range().is_empty() {
            choices.push(ScopeChoice::Selection);
        }
        choices.extend([
            ScopeChoice::Paragraph,
            ScopeChoice::Document,
            ScopeChoice::Insert,
        ]);
        choices
    }

    /// 当前范围对应哪一个按钮（用于高亮）。
    fn ai_scope_choice(&self) -> Option<ScopeChoice> {
        match self.ai.scope {
            AiScope::Selection(_) => Some(ScopeChoice::Selection),
            AiScope::Paragraph(_) => Some(ScopeChoice::Paragraph),
            AiScope::Document => Some(ScopeChoice::Document),
            AiScope::Insert { .. } => Some(ScopeChoice::Insert),
            AiScope::File(_) => None,
        }
    }

    /// 点浮层上的范围按钮：按**此刻**的编辑器状态重算范围。
    ///
    /// 重算而不是存一份旧的：浮层开着的时候光标/选区理论上不会动（它盖在
    /// 编辑区上面），但别的路径（外部改动自动重读、菜单任务）会动它 ——
    /// 现取一次比「相信旧偏移」安全（`wu` 也是每次切换都重算）。
    fn set_ai_scope(&mut self, choice: ScopeChoice, cx: &mut Context<Self>) {
        let scope = match choice {
            ScopeChoice::Selection => {
                let selection = self.editor.read(cx).selected_range();
                if selection.is_empty() {
                    return; // 选区没了就不切（下一帧按钮自己会消失）
                }
                AiScope::Selection(selection)
            }
            ScopeChoice::Paragraph => {
                let doc = self.editor.read(cx).text().to_string();
                let cursor = self.editor.read(cx).cursor();
                AiScope::Paragraph(ai_scope::paragraph_range(&doc, cursor, ai_scope::PARA_PAD))
            }
            ScopeChoice::Document => AiScope::Document,
            ScopeChoice::Insert => AiScope::Insert {
                at: self.editor.read(cx).cursor(),
            },
        };

        self.ai.scope = scope;
        self.ai.scope_label = self.ai_scope_label(&self.ai.scope, cx);
        // 切了范围就重新发过：上一轮的 diff 与错误现在说的都是别的范围了。
        self.ai.error = None;
        if self.ai.stage == AiStage::Review {
            self.ai = AiEdit {
                stage: AiStage::Ask,
                scope: self.ai.scope.clone(),
                scope_label: self.ai.scope_label.clone(),
                ..AiEdit::default()
            };
        }
        cx.notify();
    }

    /// 浮层上那句「AI 在改哪儿 / 结果落在哪儿」。
    fn ai_scope_label(&self, scope: &AiScope, cx: &Context<Self>) -> String {
        let doc = self.editor.read(cx).text().to_string();
        let count = |text: &str| text.chars().count();

        match scope {
            AiScope::Selection(range) => {
                let picked = doc.get(range.clone()).unwrap_or_default();
                let preview: String = picked.chars().take(24).collect();
                let more = if picked.chars().count() > 24 {
                    "…"
                } else {
                    ""
                };
                format!("选区 {} 字：{preview}{more} → 替换选区", count(picked))
            }
            AiScope::Paragraph(range) => {
                let (from, to) = ai_scope::line_span(&doc, range);
                let picked = doc.get(range.clone()).unwrap_or_default();
                format!(
                    "光标段落（第 {from}–{to} 行，{} 字）→ 替换这一段",
                    count(picked)
                )
            }
            AiScope::Document => format!("全文 {} 字 → 覆盖全文", count(&doc)),
            AiScope::Insert { at } => format!(
                "插入到光标（第 {} 行）· 上下文用光标段落，原文不动",
                ai_scope::line_at(&doc, *at)
            ),
            AiScope::File(path) => {
                let name = short_label(path);
                if *path == self.main_path {
                    format!("打开着的文件 {name} → 覆盖全文")
                } else {
                    format!("文件 {name} → 写回该文件")
                }
            }
        }
    }

    /// 开浮层（三个入口共用的那一段）。
    fn open_ai_overlay(&mut self, scope: AiScope, window: &mut Window, cx: &mut Context<Self>) {
        let scope_label = self.ai_scope_label(&scope, cx);
        self.ai = AiEdit {
            stage: AiStage::Ask,
            scope,
            scope_label,
            ..AiEdit::default()
        };
        self.message = None;
        // 没配 Key 就**一开始**把话说清楚（而不是等用户敲完要求、按了 Enter
        // 才告诉他这注定发不出去）—— 「Ctrl+K 出来的 AI 不能用」很多时候就是
        // 这么来的：点了没反应，因为问题在别处。
        if ai::api_key(self.ai_cfg.key.as_deref()).is_none()
            && !ai::is_local_endpoint(&ai::resolve_base_url(self.ai_cfg.base_url.as_deref()))
        {
            self.ai.error = Some(
                "没配 API Key —— 菜单「AI → AI 设置…」填一个，或用环境变量 AI_API_KEY".to_string(),
            );
        }

        self.ai_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    // ── 「AI 设置」浮层 ──────────────────────────────────

    /// 菜单「AI → AI 设置…」：把现在**真正在用**的三项填进输入框。
    ///
    /// 填的是解析后的值（环境变量优先）而不是设置文件里那几个 —— 否则
    /// 「设了环境变量、框里却是空的」会让人以为自己设错了地方。
    fn open_ai_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = [
            (
                self.ai_settings_form.base_url.clone(),
                ai::resolve_base_url(self.ai_cfg.base_url.as_deref()),
            ),
            (
                self.ai_settings_form.model.clone(),
                ai::resolve_model(self.ai_cfg.model.as_deref()),
            ),
            (
                self.ai_settings_form.key.clone(),
                self.ai_cfg.key.clone().unwrap_or_default(),
            ),
        ];
        for (input, value) in values {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.ai_settings_form
            .key
            .update(cx, |state, cx| state.focus(window, cx));
        self.ai_settings_open = true;
        cx.notify();
    }

    fn close_ai_settings(&mut self, cx: &mut Context<Self>) {
        self.ai_settings_open = false;
        cx.notify();
    }

    /// 存：写进「当前想要值」，再由 `touch_settings` 防抖落盘。
    ///
    /// 空串 = 没配（那一项回退到默认）—— 与设置文件那边的约定一致。
    /// **等于内置默认值的也存成「没配」**：否则「点开设置看一眼、直接保存」就会把
    /// 默认端点 / 默认模型钉进设置文件，以后改默认值就轮不到它生效了。
    fn save_ai_settings(&mut self, cx: &mut Context<Self>) {
        let read = |input: &Entity<InputState>, cx: &Context<Self>| {
            let text = input.read(cx).value().trim().to_string();
            (!text.is_empty()).then_some(text)
        };
        self.ai_cfg = AiConfig {
            base_url: read(&self.ai_settings_form.base_url, cx)
                .filter(|value| *value != ai::resolve_base_url(None)),
            model: read(&self.ai_settings_form.model, cx)
                .filter(|value| *value != ai::resolve_model(None)),
            key: read(&self.ai_settings_form.key, cx),
        };
        self.touch_settings(cx);
        self.ai_settings_open = false;
        // 浮层上那句「没配 Key」现在不成立了：存完就把它摘掉，
        // 否则用户填完 Key 回来看到的还是那句红字。
        self.ai.error = None;
        self.message = Some(format!("AI 设置已保存：{}", self.ai_cfg.summary()));
        logln!("[typst-live] AI 设置已保存：{}", self.ai_cfg.summary());
        cx.notify();
    }

    /// Esc / 点空白：关掉 AI 浮层（生成中就顺手取消）。
    fn cancel_ai(&mut self, cx: &mut Context<Self>) {
        self.ai.cancel.store(true, Ordering::Relaxed);
        self.ai = AiEdit::default();
        self.message = Some("已取消 AI 编辑".to_string());
        cx.notify();
    }

    fn ai_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.ai.hunks.is_empty() {
            return;
        }
        let last = self.ai.hunks.len() - 1;
        let next = self.ai.selected as isize + delta;
        self.ai.selected = next.clamp(0, last as isize) as usize;
        cx.notify();
    }

    /// 切换当前块的接受/拒绝 —— AI 的修改往往散在几处，该能逐块要。
    fn ai_toggle_hunk(&mut self, cx: &mut Context<Self>) {
        let Some(accepted) = self.ai.accepted.get_mut(self.ai.selected) else {
            return;
        };
        *accepted = !*accepted;
        let state = if self.ai.accepted[self.ai.selected] {
            "接受"
        } else {
            "拒绝"
        };
        self.message = Some(format!("第 {} 块已{state}", self.ai.selected + 1));
        cx.notify();
    }

    /// 发请求：把「上下文 + 历史 + 本轮要求」交给模型。
    ///
    /// **生成在 std 线程里**（见 `AiEdit` 的注释），界面只按 100ms 轮询回执。
    fn start_ai(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.stage != AiStage::Ask {
            return;
        }
        let instruction = self.ai_input.read(cx).value().trim().to_string();
        if instruction.is_empty() {
            self.message = Some("先说一句要改什么".to_string());
            cx.notify();
            return;
        }

        // 上下文：有选区就只给选区（省钱、也更准），否则给截断过的全文；
        // 文件范围则从磁盘读那个文件（它可能根本没在编辑器里打开）
        let raw_context = match self.ai_scope_text(cx) {
            Ok(text) => text,
            Err(why) => {
                self.ai.error = Some(why.clone());
                self.message = Some(why);
                cx.notify();
                return;
            }
        };
        let (context, truncated) = ai::truncate_context(&raw_context, ai::AI_EDIT_CONTEXT_LIMIT);
        if truncated {
            logln!(
                "[typst-live] AI 上下文超限，已截断到 {} 字",
                ai::AI_EDIT_CONTEXT_LIMIT
            );
        }

        // 插入模式要额外说一句：上下文只是位置参照，别把上下文重写一遍插进去。
        // （普通编辑的提示词说的是「给出修改后的完整片段」—— 那句话在插入模式下
        // 会被理解成「重写上下文」，插进去就是一坨重复内容。）
        let instruction = if matches!(self.ai.scope, AiScope::Insert { .. }) {
            format!("{}\n\n{}", ai::INSERT_HINT, instruction)
        } else {
            instruction
        };

        let messages = ai::edit_messages(&context, &self.ai_history, &instruction);
        self.ai_history.push((true, instruction.clone()));
        self.spawn_ai(messages, raw_context, self.ai.scope.clone(), cx);
    }

    /// 当前作用范围里的那段文字。
    ///
    /// 三种范围各取各的来源，而且是**每次都现取**：
    /// - `Selection` / `Document`：编辑器里的当前文本（用户可能在浮层开着的时候
    ///   又回去敲了几个字 —— 那也该算进去）
    /// - `File`：磁盘上的那个文件；
    ///   但如果它正是编辑器里打开的那个，仍旧用编辑器里那一份（未保存的改动在里面）。
    ///   与 `ai_scope::landing` 是同一条规矩的两面：**编辑器里那一份才是真相**。
    fn ai_scope_text(&self, cx: &Context<Self>) -> Result<String, String> {
        let doc = || self.editor.read(cx).text().to_string();

        match &self.ai.scope {
            AiScope::Document => Ok(doc()),
            // 选区与光标段落都是「文档里的一段」：直接按范围取。段落范围是我们
            // 自己刚算的，所以不会失配；选区可能已经被改过（外部改动重读、
            // 或用户手快）—— 那就明说，而不是静默拿错位置。
            AiScope::Selection(range) | AiScope::Paragraph(range) => doc()
                .get(range.clone())
                .map(|s| s.to_string())
                .ok_or_else(|| "那段文字已经变了（或选区失效），重新选一次再试".to_string()),
            // 插入：上下文同样用「光标段落」—— 让模型知道写到哪儿了。
            AiScope::Insert { at } => {
                let doc = doc();
                Ok(doc[ai_scope::paragraph_range(&doc, *at, ai_scope::PARA_PAD)].to_string())
            }
            AiScope::File(path) if *path == self.main_path => Ok(doc()),
            AiScope::File(path) => std::fs::read_to_string(path)
                .map_err(|err| format!("读不了「{}」：{err}", short_label(path))),
        }
    }

    /// 把一组 messages 交给模型（起 std 线程 + 轮询回执）。三个入口共用：
    /// `Ctrl+K` 的自由指令、菜单里的固定任务、目录树里选中的那个文件。
    fn spawn_ai(
        &mut self,
        messages: Vec<(String, String)>,
        original: String,
        scope: AiScope,
        cx: &mut Context<Self>,
    ) {
        let base_url = ai::resolve_base_url(self.ai_cfg.base_url.as_deref());
        let model = ai::resolve_model(self.ai_cfg.model.as_deref());
        let key = ai::api_key(self.ai_cfg.key.as_deref());

        // 云端端点没 Key 是**等会儿必然失败**：不把注定 401 的请求发出去（那只会让人
        // 等到超时，然后对着一句服务端报错猜），而是当场说清楚去哪儿填。
        // 本地端点（Ollama 之类）本来就不需要 Key，照发。
        if key.is_none() && !ai::is_local_endpoint(&base_url) {
            self.ai.stage = AiStage::Ask; // 退回输入阶段，刚敲的要求不会丢
            self.ai.error = Some(format!(
                "没配 API Key（当前端点 {base_url}）—— AI 菜单 → 「AI 设置…」填一个，\
                 或设环境变量 AI_API_KEY"
            ));
            self.message = Some("AI 没配 Key：AI 菜单 → AI 设置…".to_string());
            logln!("[typst-live] AI 未发请求：没配 Key（{base_url}）");
            cx.notify();
            return;
        }

        logln!(
            "[typst-live] AI 请求：{model} @ {base_url}（{} 条消息，原文 {} 字）",
            messages.len(),
            original.chars().count()
        );

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = self.ai.cancel.clone();
        let progress = self.ai.progress.clone();
        std::thread::spawn(move || {
            let result = ai::chat_messages(
                &base_url,
                &model,
                key.as_deref(),
                &messages,
                &cancel,
                &progress,
            );
            // 接收端可能已经走了（用户按了 Esc），发不出去就算了
            let _ = tx.send(result);
        });

        self.ai.rx = Some(rx);
        self.ai.stage = AiStage::Running;
        self.ai.error = None;
        self.ai.original = original;
        self.ai.scope = scope;

        // 轮询回执：100ms 一次，顺带把进度刷到界面上
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;

                match safe_task_update(&this, cx, |this, cx| this.take_ai_result(cx)) {
                    UpdateOutcome::Done(true) => break,
                    UpdateOutcome::Done(false) => {}
                    // 借用竞态：下一拍接着读回执（后台线程还在跑，回执丢不了）
                    UpdateOutcome::Busy => {}
                    UpdateOutcome::Gone => break,
                }
            }
        })
        .detach();

        cx.notify();
    }

    /// 菜单里的固定任务（语法修复 / 校对 / 术语 / 互译）。
    ///
    /// 「语法检查并修复」的诊断来自**本项目引擎**（`typst-engine` 的语法诊断 +
    /// 编译诊断），不像 `wu` 那样 shell 调外部 `typst` CLI —— 我们本来就有
    /// 一棵增量维护的语法树，没必要为了拿诊断再起进程。
    fn run_ai_task(
        &mut self,
        task: AiTask,
        scope: AiScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = self.ai_scope_label(&scope, cx);
        self.ai = AiEdit {
            stage: AiStage::Running,
            scope,
            scope_label: format!("{} · {label}", task.label()),
            ..AiEdit::default()
        };
        self.ai_history.clear();

        let source = match self.ai_scope_text(cx) {
            Ok(text) => text,
            Err(why) => {
                self.ai = AiEdit::default();
                self.message = Some(why);
                cx.notify();
                return;
            }
        };

        if source.trim().is_empty() {
            self.ai = AiEdit::default();
            self.message = Some("没有可处理的文字".to_string());
            cx.notify();
            return;
        }

        let errors = self.diagnostic_summary(cx);
        let prompt = ai::task_prompt(task, &source, &errors);
        let (context, _) = ai::truncate_context(&source, ai::AI_EDIT_CONTEXT_LIMIT);
        let messages = vec![
            (
                "system".to_string(),
                "你是专业的 Typst 写作助手，只输出结果本身。".to_string(),
            ),
            ("user".to_string(), format!("【当前文本】\n{context}")),
            ("user".to_string(), prompt),
        ];

        self.message = Some(format!("AI 任务：{}（{label}）", task.label()));
        let _ = window;
        self.spawn_ai(messages, source, self.ai.scope.clone(), cx);
        cx.notify();
    }

    /// 当前诊断的简短摘要（给「语法检查并修复」当输入）。
    fn diagnostic_summary(&self, cx: &Context<Self>) -> String {
        let Ok(source) = typst::World::source(&self.engine, self.engine.entry().main()) else {
            return String::new();
        };

        let mut diagnostics = lang::syntax_diagnostics(&source);
        diagnostics.extend(lang::compile_diagnostics(&source, &self.compile_errors));

        let mut out = String::new();
        for diag in diagnostics.iter().take(20) {
            let where_ = diag
                .line_col
                .map(|p| format!("第 {} 行：", p.line + 1))
                .unwrap_or_default();
            out.push_str(&format!("{where_}{}\n", diag.message));
        }
        let _ = cx;
        out
    }

    /// 看一眼后台线程有没有回执。返回「这条请求是否已经结束」。
    fn take_ai_result(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(rx) = &self.ai.rx else {
            return true;
        };

        let received = match rx.try_recv() {
            Ok(value) => Some(value),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("AI 线程意外结束".to_string()))
            }
        };

        let Some(result) = received else {
            // 还没回来：只重绘，好让进度数字动起来
            cx.notify();
            return false;
        };

        self.ai.rx = None;
        match result {
            Ok(reply) => {
                let cleaned = ai::clean_source(&reply);
                // 空回复按失败处理，否则会拿一个空串去「应用」掉整段文字
                if cleaned.trim().is_empty() {
                    self.ai.error = Some("AI 返回为空".to_string());
                    self.ai.stage = AiStage::Ask;
                } else {
                    // 插入模式没有「原文本 → 回复」这回事：回复本来就是**新**内容，
                    // 不是对上下文的改写，算 diff 只会得到一屏「删掉上下文、换成回复」。
                    // 所以这种模式下不切块，Review 那一屏直接把回复整段摆出来。
                    let inserting = matches!(self.ai.scope, AiScope::Insert { .. });
                    self.ai.hunks = if inserting {
                        Vec::new()
                    } else {
                        diff::hunks(&self.ai.original, &cleaned)
                    };
                    self.ai.accepted = vec![true; self.ai.hunks.len()];
                    self.ai.selected = 0;
                    self.ai.result = cleaned.clone();
                    self.ai.stage = AiStage::Review;
                    self.ai_history.push((false, cleaned.clone()));
                    logln!(
                        "[typst-live] AI 返回 {} 字{}",
                        cleaned.chars().count(),
                        if inserting {
                            "（插入模式：不改原文，直接在光标处插入）".to_string()
                        } else {
                            format!("，切成 {} 个可逐块接受的改动", self.ai.hunks.len())
                        }
                    );
                }
            }
            Err(err) => {
                self.ai.error = Some(err.clone());
                self.ai.stage = AiStage::Ask;
                logln!("[typst-live] AI 失败：{err}");
            }
        }

        cx.notify();
        true
    }

    /// 应用：按接受标记重建那段文字，再落回它该去的地方。
    ///
    /// 落点由 `ai_scope::landing` 判（纯函数，有单测）：编辑器里的（选区 /
    /// 光标段落 / 全文 / 插入 / 打开的正是那个文件）走编辑器，于是**置脏、
    /// 重排、自动保存、关窗保护全部照旧**；其它文件则直接写回磁盘。
    fn apply_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.stage != AiStage::Review {
            return;
        }

        let landing = ai_scope::landing(&self.ai.scope, &self.main_path);
        // 插入模式没有块（回复整段都是新的），直接用原文；其它模式按逐块接受
        // 的标记重建（拒绝的块逐字节保持原文）。
        let applied = match landing {
            ai_scope::Landing::Insert { .. } => self.ai.result.clone(),
            _ => diff::apply_hunks(&self.ai.original, &self.ai.result, &self.ai.accepted),
        };
        let kept = self
            .ai
            .accepted
            .iter()
            .filter(|accepted| **accepted)
            .count();
        let total = self.ai.hunks.len();
        let inserted = matches!(landing, ai_scope::Landing::Insert { .. });
        // 写文件那条分支自己会说一句话（成功/失败都说），所以这里的兜底不覆盖它
        let writes_file = matches!(landing, ai_scope::Landing::File(_));

        match landing {
            ai_scope::Landing::Replace(range) => {
                let doc = self.editor.read(cx).text().to_string();
                // 错位保护：从「按这个范围取的上下文」到「现在」之间那段文字可能
                // 已经被改过（外部改动自动重读、浮层开着时用户手快）。宁可什么都不做 ——
                // 拿旧偏移去拼就是改坏文档。
                if doc.get(range.clone()) != Some(self.ai.original.as_str()) {
                    self.ai.error = Some(
                        "那段文字已经变了（或已被外部改动覆盖），这次没有应用 —— Esc 关掉重来"
                            .to_string(),
                    );
                    self.message = Some("AI：目标文字已变化，未应用".to_string());
                    logln!("[typst-live] AI 未应用：目标文字与取上下文时不一致");
                    cx.notify();
                    return;
                }

                let (next, caret) = ai_scope::splice_at(&doc, range, &applied);
                self.install_editor_text(next, caret, window, cx);
                self.mark_edited(cx);
                self.recompile(cx);
            }

            ai_scope::Landing::WholeDocument => {
                let doc = self.editor.read(cx).text().to_string();
                // 整篇换掉：旧偏移在新文本里没有对应位置，夹到长度里就行
                // （比把光标扔回第一行强）。
                let (next, fallback) = ai_scope::splice_at(&doc, 0..doc.len(), &applied);
                let caret = self.editor.read(cx).cursor().min(fallback);
                self.install_editor_text(next, caret, window, cx);
                self.mark_edited(cx);
                self.recompile(cx);
            }

            ai_scope::Landing::Insert { at } => {
                let doc = self.editor.read(cx).text().to_string();
                // 插入：原文一个字不动，新内容放在光标处，光标跟到新内容之后
                // —— 接着往下写就是从这里开始。
                let (next, caret) = ai_scope::splice_at(&doc, at..at, &applied);
                self.install_editor_text(next, caret, window, cx);
                self.mark_edited(cx);
                self.recompile(cx);
            }

            ai_scope::Landing::File(path) => {
                match std::fs::write(&path, &applied) {
                    Ok(()) => {
                        // 这个文件很可能正被 `#include`：源缓存作废，否则重排读到的
                        // 还是旧内容（`Ctrl+B` 做的是同一件事）。
                        self.engine.sources().invalidate_all();
                        self.recompile(cx);
                        logln!(
                            "[typst-live] AI 写回文件：{}（{} 块，{}B）",
                            path.display(),
                            kept,
                            applied.len()
                        );
                        self.message = Some(format!(
                            "已写回「{}」（{kept}/{total} 块）—— Ctrl+B 可重排全文",
                            short_label(&path)
                        ));
                    }
                    Err(err) => {
                        self.message = Some(format!("写回「{}」失败：{err}", short_label(&path)));
                    }
                }
            }
        }

        self.ai = AiEdit::default();
        if !writes_file {
            self.message = Some(if inserted {
                format!("已插入到光标处（{} 字）", applied.chars().count())
            } else {
                format!("AI 改动已应用（{kept}/{total} 块）")
            });
        }
        if inserted {
            logln!("[typst-live] AI 插入到光标：{} 字", applied.chars().count());
        } else {
            logln!("[typst-live] AI 应用：接受 {kept}/{total} 块");
        }
        cx.notify();
    }

    /// 把整篇新文本装进编辑器，并把光标放到 `caret`（字节偏移）处。
    ///
    /// 两个坑都在 `set_value` 上：① 它是**静默路径**（不发 `Change` 事件），
    /// 所以拒绝的调用方得自己 `recompile`；② 它把光标与滚动位置清成 0 ——
    /// 不自己还原的话，每用一次 AI 光标就跳回第一行。
    fn install_editor_text(
        &mut self,
        text: String,
        caret: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            let position = state.text().offset_to_position(caret);
            state.set_cursor_position(position, window, cx);
        });
    }

    // ── 双向跳转（双击）──────────────────────────────────────

    // ── 快速跳转（Ctrl+P）────────────────────────────────────────────

    /// 在编辑器里打开一个文本文件（目录树点击走这里）。
    ///
    /// **只有一份文档**（标签页功能已按用户要求删除）：换文件就是换编辑器里的内容，
    /// 所以有未保存改动时**不换**，先提示保存 —— 以前改动还在别的标签里躺着，
    /// 现在换掉就等于把改动丢了。
    fn open_in_editor(
        &mut self,
        full: PathBuf,
        text: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dirty {
            self.message = Some(format!(
                "「{}」还有未保存的修改：先 Ctrl+S 保存（或 Ctrl+Z 撤到干净）再打开别的文件",
                short_label(&self.main_path)
            ));
            logln!("[typst-live] 拒绝打开 {label}：当前文档有未保存的修改");
            cx.notify();
            return;
        }

        self.load_document(full, text, window, cx);
        self.message = Some(format!("打开 {label}"));
    }

    /// 把一份文档装进编辑器并重新编译。
    fn load_document(
        &mut self,
        path: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let started = Instant::now();

        // 换入口：字体留着，入口 / 覆盖层 / 源缓存 / 上次成功结果都重置。
        self.engine.reopen(&self.root, &path);
        self.main_path = path.clone();
        self.explicit_file = true;
        self.touch_settings(cx);
        self.doc = None;
        self.bitmaps.clear();
        self.texture_bytes = 0;
        self.current_page = 0;
        // 刚打开的文档是干净的（没有标签页，也就没有「别的标签里的未保存改动」）
        self.dirty = false;
        // 「磁盘上那一份」= 刚读进来的那份：不更新它，自动保存第一拍就会
        // 认为「变了」，然后原样把同一篇文本重写一遍。
        self.saved_text = text.clone();
        // 置空是为了让下面的 recompile 不受「文本没变就不排」的干扰
        self.last_text = String::new();
        // 磁盘上这一份的指纹：只有它变了才值得重读。`None`（磁盘上还没有这个
        // 文件）也要记下来，这样它一被创建就会被发现。
        self.disk_stamp = disk_watch::Stamp::of(&path);
        // 上一个文件留下的待装文本 / 提示，跟着一起清掉。
        self.pending_reload = None;
        self.disk_problem = None;
        self.right = RightPane::Preview;

        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
        self.recompile(cx);

        logln!(
            "[typst-live] 打开 → {}（{:.0} ms，{} 页）",
            short_label(&path),
            started.elapsed().as_secs_f64() * 1000.0,
            self.status.pages
        );
    }
    // ── 目录树 / 左栏 / 右侧主区 ─────────────────────────

    /// 重新扫描并重建目录树。
    ///
    /// **扫盘放后台线程**：项目目录里几万个文件时，同步扫会卡住一整帧
    /// （用户看到的是「点了没反应」）。建 gpui 元素（`TreeItem`）必须回主线程做，
    /// 所以后台只负责扫，回到主线程再建条目。
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let expanded = self.tree_expanded.clone();
        let started = Instant::now();
        logln!("[typst-live] 扫描目录树：{}", root.display());
        self.tree_root = Some(root.clone());
        self.tree_scanning = true;
        self.message = Some(format!("正在扫描 {}…", short_path(&root.to_string_lossy())));
        cx.notify();

        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let nodes = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    async move { tree::scan_dir(&root) }
                })
                .await;

            let count = tree::count_nodes(&nodes);
            let items = tree::build_file_items(&root, nodes, &expanded);
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;

            // 扫完回来更新 UI。
            let _ = safe_task_update(&weak, cx, |this, cx| {
                this.tree_entries = count;
                this.tree_scanning = false;
                this.tree_state
                    .update(cx, |state, cx| state.set_items(items, cx));
                logln!(
                    "[typst-live] 目录树：{count} 个条目，用时 {elapsed:.1} ms（{}，后台扫描）",
                    root.display()
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// 打开一个文件（目录树点击走这里）：按类型决定显示到哪里。
    ///
    /// - 图片 → 右侧图片视图
    /// - `.md` → 右侧 Markdown 渲染（**可选中可复制**，位图预览做不到）
    /// - 其它文本 → 编辑器（右侧回到排版预览）
    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if image_view::is_image_path(&path) {
            self.image = Some(cx.new(|cx| ImageView::new(path.clone(), cx)));
            self.right = RightPane::Image;
            self.message = Some(format!(
                "图片 {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            cx.notify();
            return;
        }

        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let label = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let is_markdown = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"));

                if is_markdown {
                    let full = path.to_string_lossy().to_string();
                    self.markdown
                        .update(cx, |view, cx| view.set_source(full, text, cx));
                    self.right = RightPane::Markdown;
                    self.message = Some(format!("Markdown {label}"));
                    cx.notify();
                } else {
                    self.open_in_editor(path, text, label, window, cx);
                }
            }
            Err(err) => {
                self.message = Some(format!("打不开 {}：{err}", path.display()));
                cx.notify();
            }
        }
    }
}

/// 默认 shell：Windows 上 PowerShell，其它平台看 `$SHELL`。
fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// 去掉重复诊断（同一处、同一句话只留一条）。
///
/// 语法诊断与编译诊断是**两份来源**：同一个问题（比如 `#` 出现在代码里）
/// 可能两边都报。不去重的话错误面板会把同一句话写两遍，状态栏还会把
/// 1 个问题数成 2 个错误。
fn dedupe_diags(diags: &mut Vec<lang::Diagnostic>) {
    // 「同一处、同一句话」的指纹。写成别名是为了让下面那句读得下去
    type Fingerprint = (bool, Option<(usize, usize)>, String);
    let mut seen: HashSet<Fingerprint> = HashSet::new();
    diags.retain(|d| {
        let key = (
            d.is_error,
            d.line_col.as_ref().map(|pos| (pos.line, pos.col)),
            d.message.clone(),
        );
        seen.insert(key)
    });
}

/// 诊断 → 行号区间（`wanted_error` 为真取错误、否则取警告）。
///
/// 引擎给的诊断里行号是 0 起的（`line_col`），照抄即可。
fn diag_lines(diags: &[lang::Diagnostic], wanted_error: bool) -> Vec<(usize, usize)> {
    diags
        .iter()
        .filter(|d| d.is_error == wanted_error)
        .filter_map(|d| d.line_col.as_ref().map(|pos| (pos.line, pos.line)))
        .collect()
}

/// 大纲里最多画几条错误（面板不高，多了也看不完）。
const MAX_ERROR_ROWS: usize = 6;

/// 在整块画布上画网格：48px 一格，每 4 格一条更深的线。
///
/// 用 `canvas` + `Window::paint_quad` 直接画线 —— gpui 没有「背景图案」这种东西；
/// 铺几百个 1px 的 div 也行，但画一次比铺一堆元素便宜得多。
fn paint_grid(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    const STEP: f32 = 48.0;
    const MAJOR_EVERY: usize = 4;

    let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
    let (x0, y0) = (bounds.origin.x.as_f32(), bounds.origin.y.as_f32());
    let major = color.opacity(0.85);
    let minor = color.opacity(0.40);

    let mut x = 0.0;
    let mut index = 0usize;
    while x <= w {
        let line = if index.is_multiple_of(MAJOR_EVERY) {
            major
        } else {
            minor
        };
        window.paint_quad(gpui::fill(
            Bounds::new(point(px(x0 + x), px(y0)), size(px(1.), px(h))),
            line,
        ));
        x += STEP;
        index += 1;
    }

    let mut y = 0.0;
    let mut index = 0usize;
    while y <= h {
        let line = if index.is_multiple_of(MAJOR_EVERY) {
            major
        } else {
            minor
        };
        window.paint_quad(gpui::fill(
            Bounds::new(point(px(x0), px(y0 + y)), size(px(w), px(1.))),
            line,
        ));
        y += STEP;
        index += 1;
    }
}

/// 载入要编辑的文档：命令行参数 > 设置里的上次文件 > 内置示例。
///
/// 第三个返回值是「这是不是用户自己的文件」—— 只有它值得记进设置里的
/// 「上次打开的文件」（内置示例落在临时目录，记下来没有意义）。
fn load_document(arg: Option<String>, remembered: Option<&Path>) -> (String, PathBuf, bool) {
    let candidates = arg
        .map(PathBuf::from)
        .into_iter()
        .chain(remembered.map(Path::to_path_buf));

    for path in candidates {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                logln!("打开 {}", path.display());
                return (text, path, true);
            }
            Err(err) => logln!("读不了 {}：{err}", path.display()),
        }
    }

    // 示例文档落在临时目录。注意：磁盘上存不存在都无所谓 ——
    // 引擎用的是内存覆盖层，这正是实时编译的前提。
    let path = std::env::temp_dir().join("typst-live-demo.typ");
    logln!("使用内置示例文档（虚拟路径 {}）", path.display());
    (DEMO_DOC.to_owned(), path, false)
}

/// `--ai-selftest [insert]` 的实现：真发一句请求并把模型的回复打出来。
///
/// 退出码：0 通、1 不通（脚本里可以直接判）。
fn ai_selftest(insert: bool) -> i32 {
    let settings = Settings::load();
    let base_url = ai::resolve_base_url(settings.ai_base_url.as_deref());
    let model = ai::resolve_model(settings.ai_model.as_deref());
    let key = ai::api_key(settings.ai_api_key.as_deref());

    println!("端点 : {base_url}");
    println!("模型 : {model}");
    println!(
        "Key  : {}",
        if key.is_some() {
            "已配（来自环境变量 AI_API_KEY 或 settings.conf）"
        } else if ai::is_local_endpoint(&base_url) {
            "无（本地端点，不需要）"
        } else {
            "**没配** → 菜单「AI → AI 设置…」填一个"
        }
    );

    let messages = if insert {
        // 与 `start_ai` 拼出来的一模一样：光标段落的上下文 + 插入模式的补充说明
        let context = "= 第一章 概述\n\n本节介绍了系统的总体架构与部署方式，\n说明各模块之间的调用关系与数据流。\n";
        let instruction = format!(
            "{}\n\n在光标处补一句关于「部署环境要求」的说明",
            ai::INSERT_HINT
        );
        ai::edit_messages(context, &[], &instruction)
    } else {
        vec![
            ("system".to_string(), "只回一个字。".to_string()),
            ("user".to_string(), "ping".to_string()),
        ]
    };
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    match ai::chat_messages(
        &base_url,
        &model,
        key.as_deref(),
        &messages,
        &cancel,
        &progress,
    ) {
        Ok(reply) => {
            println!("回复 : {}", ai::clean_source(&reply));
            println!("\n→ AI 可用。");
            0
        }
        Err(err) => {
            println!("失败 : {err}");
            println!("\n→ 不通。先对着上面三行看：没配 Key 就填 Key；有 Key 就看服务端那句话。");
            1
        }
    }
}

/// `--rec-selftest` 要录多少秒（0 = 不是自测）。
static REC_SELFTEST: AtomicU64 = AtomicU64::new(0);

fn main() {
    // 崩溃也要留痕：发布版是 GUI 子系统，stderr 没人接得住（见 `diag` 模块）。
    diag::install_panic_hook();

    let mut path_arg = std::env::args().nth(1);

    // `--rec-selftest [秒数]`：开窗 → 自动录一段带声音的 → 收尾 → 打印成品路径并退出。
    //
    // 录屏这条链路全是平台行为（ffmpeg 子进程 + dshow 设备名 + gdigrab 的物理坐标 +
    // 窗口句柄），单测覆盖不到；留一条一行命令的端到端验证，以后回归不用靠手点。
    // 它**必须开窗**（采集区域来自窗口本身），所以不能像 `--ai-selftest` 那样早退。
    if path_arg.as_deref() == Some("--rec-selftest") {
        let secs = std::env::args()
            .nth(2)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);
        REC_SELFTEST.store(secs, Ordering::Relaxed);
        path_arg = None;
    }

    // `--ai-selftest`：不开窗，真发一次最小的请求，把结果打在终端上。
    //
    // 「AI 不能用」这句话里有三种可能：没配 Key / 端点写错 / 模型名不对。
    // 这三种得能分开定位 —— 否则人只能对着界面上那句「失败：…」猜。
    // 它走的就是界面用的那一条路（`ai::chat_messages`：curl 子进程 + SSE 解析），
    // 不是另写一份探测代码。
    if path_arg.as_deref() == Some("--ai-selftest") {
        // 带 `insert` 时改成跑一遍**插入模式**的提示词：那一句提示词决定了
        // 回复是「新内容」还是「把上下文重写一遍」，值得能单独验一下。
        let insert = std::env::args().nth(2).as_deref() == Some("insert");
        std::process::exit(ai_selftest(insert));
    }

    // 设置先读：窗口开在哪儿、上次开的是哪个文件、上次缩放多少，都在这儿。
    let settings = Settings::load();
    logln!(
        "[typst-live] 读设置 {}：窗口 {:?}，缩放 {:?}，上次文件 {:?}",
        settings::path().display(),
        settings.window,
        settings.zoom,
        settings.file,
    );
    match Packages::with_downloads().cache_dir() {
        Some(dir) => logln!(
            "[typst-live] 包源：本地目录 + 官方源（下载缓存 {}）",
            dir.display()
        ),
        None => logln!("[typst-live] 包源：只认本地目录（系统缓存目录不可用）"),
    }

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        // 用 gpui-component 的任何东西之前必须先 init。
        gpui_component::init(cx);
        register_typst_language();

        cx.bind_keys([
            KeyBinding::new("ctrl-=", ZoomIn, None),
            KeyBinding::new("ctrl-+", ZoomIn, None),
            KeyBinding::new("ctrl--", ZoomOut, None),
            KeyBinding::new("ctrl-0", ZoomReset, None),
            KeyBinding::new("ctrl-s", SaveFile, None),
            KeyBinding::new("ctrl-b", RecompileNow, None),
            KeyBinding::new("ctrl-shift-f", FormatDocument, None),
            KeyBinding::new("ctrl-e", ExportPdf, None),
            // 前向跳转的键盘版（双击是鼠标版；“双击选词”不该是唯一入口）
            KeyBinding::new("ctrl-alt-j", SyncToPreview, None),
            // 显示开关：折叠（Ctrl+Alt+F）、跟随光标（Ctrl+Alt+L）
            KeyBinding::new("ctrl-alt-f", ToggleFolding, None),
            KeyBinding::new("ctrl-alt-l", ToggleFollow, None),
            // 菜单里不放了，但功能得有入口
            KeyBinding::new("ctrl-alt-g", TogglePreviewBg, None),
            KeyBinding::new("ctrl-alt-m", ToggleMetrics, None),
            KeyBinding::new("ctrl-alt-s", ToggleAutosave, None),
            // 录屏：只录本软件窗口那一块画面。`Ctrl+Alt+R` 开始/停止、`Ctrl+Alt+P` 暂停/继续。
            KeyBinding::new("ctrl-alt-r", ToggleRecording, None),
            KeyBinding::new("ctrl-alt-p", ToggleRecordPause, None),
            // AI 编辑。Enter/Tab/空格/Esc 只在浮层内生效（按键上下文限定），
            // 否则会把编辑器自己的按键抢走。
            KeyBinding::new("ctrl-k", AiEditOpen, None),
            // AI 设置浮层的 Esc（与 AI 浮层同一个套路：按键上下文限定，
            // 否则会把编辑器自己的 Esc 抢走）。Enter 不在这里绑 —— 三个输入框
            // 自己开了 `submit_on_enter`，由它们的订阅存盘（见 `new`）。
            KeyBinding::new("escape", AiSettingsCancel, Some("AiSettings")),
            // 终端显隐（与 wu 同一个键位）
            KeyBinding::new("ctrl-4", ToggleShell, None),
            KeyBinding::new("enter", AiSubmit, Some("AiEdit")),
            KeyBinding::new("tab", AiNextHunk, Some("AiEdit")),
            KeyBinding::new("shift-tab", AiPrevHunk, Some("AiEdit")),
            KeyBinding::new("space", AiToggleHunk, Some("AiEdit")),
            KeyBinding::new("escape", AiCancel, Some("AiEdit")),
            KeyBinding::new("pageup", PrevPage, None),
            KeyBinding::new("pagedown", NextPage, None),
            KeyBinding::new("ctrl-home", FirstPage, None),
            KeyBinding::new("ctrl-end", LastPage, None),
            // 快速打开。↑↓ / Esc 必须限定在浮层的按键上下文里，
            // 否则会抢走编辑器里的方向键。
        ]);

        let (source, path, explicit_file) = load_document(path_arg, settings.file.as_deref());
        let restore = settings.clone();

        cx.spawn(async move |cx| {
            // 上次的窗口几何。逻辑像素取整存过，回来时直接用。
            let bounds = restore.window.map_or_else(
                || {
                    Bounds::new(
                        point(px(80.), px(60.)),
                        Size {
                            width: px(1320.),
                            height: px(880.),
                        },
                    )
                },
                |(x, y, w, h)| {
                    Bounds::new(
                        point(px(x as f32), px(y as f32)),
                        Size {
                            width: px(w as f32),
                            height: px(h as f32),
                        },
                    )
                },
            );

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("typst-engine · 实时预览".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(Size {
                    width: px(900.),
                    height: px(600.),
                }),
                ..Default::default()
            };

            let source = source.clone();
            let path = path.clone();
            let settings = restore.clone();
            let opened = cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| {
                    Previewer::new(
                        source.clone(),
                        path.clone(),
                        explicit_file,
                        settings.clone(),
                        window,
                        cx,
                    )
                });

                // 关窗钩子。这个应用除了保存与导出不写盘，所以「关窗」的默认
                // 后果仍然是丢掉没落盘的东西 —— 必须拦一道。
                // 返回 `true` 才真的关（见 `Previewer::on_close_requested`）。
                let weak = view.downgrade();
                window.on_window_should_close(cx, move |_window, cx| {
                    match safe_task_update(&weak, cx, |this, cx| this.on_close_requested(cx)) {
                        UpdateOutcome::Done(allow) => allow,
                        // 借用竞态：这一次没读到状态。**拦住**，让用户再点一次关闭 ——
                        // 宁可多问一遍，也不能靠猜把别人的字丢掉。
                        UpdateOutcome::Busy => {
                            logln!("[typst-live] 关窗请求撞上借用竞态，已拦下（再点一次关闭即可）");
                            false
                        }
                        // 视图已经没了：没什么可拦的
                        UpdateOutcome::Gone => true,
                    }
                });

                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            });

            // 开窗失败 = 这个进程一个窗口都没有。别 panic（GUI 子系统下用户
            // 什么都看不见），记一条日志然后退出。
            if let Err(err) = opened {
                logln!("[typst-live] 打开窗口失败：{err}");
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    });
}
