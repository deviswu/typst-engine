//! 实时 Typst 预览器 —— `typst-engine` 的 GPUI 外壳。
//!
//! 左边敲字，右边在**几毫秒内**跟着变。中间没有子进程、没有 IPC、
//! 没有存盘：编辑器把文本喂给引擎的内存覆盖层，引擎在同一个进程里重排版。
//!
//! ```bash
//! cargo run -p typst-live              # 用内置示例文档
//! cargo run -p typst-live -- doc.typ   # 打开指定文件
//! ```
//!
//! **排版与光栅化是两层**：缩放只重做光栅化，不重新排版。状态栏上
//! 「排版 N 次 / 光栅化 M 次」两个计数会把这件事直接显示出来 ——
//! 按 Ctrl+= 缩放时你只会看到后一个数在涨。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::tree::{Tree, TreeEvent, TreeState};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Root, RopeExt as _, Selectable as _, h_flex,
    v_flex,
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
use image_view::ImageView;
use markdown_view::MarkdownView;
use markup::Markup;
use settings::Settings;
use term_colors::TerminalPalette;
use themes::ThemeItem;

mod ai;
mod coords;
mod diff;
mod finder;
mod image_view;
mod markdown_view;
mod markup;
mod settings;
mod term_colors;
mod terminal;
mod terminal_view;
mod themes;
mod tree;

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
        QuickOpen,
        QuickPrev,
        QuickNext,
        QuickCancel,
        SyncToPreview,
        AiEditOpen,
        AiSubmit,
        AiCancel,
        AiNextHunk,
        AiPrevHunk,
        AiToggleHunk,
        ToggleShell,
    ]
);

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEP: f32 = 1.25;

/// 扫描候选文件的上限。再多就不适合靠打字找了。
const MAX_SCAN_FILES: usize = 5000;

/// 快速打开列表里最多显示多少条。
const MAX_QUICK_MATCHES: usize = 50;

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

/// 进程起点。给「首帧」「首次排版」这类一次性事件打时间戳 ——
/// 「窗口多久出来」「排版多久」得是能对上的数字，不是感觉。
fn since_start_ms() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// 前向跳转时把目标行放在视口顶部往下多少逻辑像素处 ——
/// 上面留一点，好看见「这是哪一段」。
const SYNC_MARGIN: f32 = 80.0;

const DEMO_DOC: &str = r#"#set page(width: 15cm, height: auto, margin: 1.8cm)
#set text(size: 11pt)

= 实时编译演示

在*左边随便改点什么*，右边会在几毫秒内跟着变。

没有子进程，没有存盘，没有 IPC —— 编译器直接读内存里未保存的文本。

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

/// 「选中文字 → AI 编辑」的全部状态。
///
/// 设计要点：**生成在 std 线程里跑，界面只轮询**。
/// 不用 gpui 的 background_executor 跑这个阻塞调用 —— 那个池子里还跑着
/// 设置防抖、终端事件轮询等定时任务，一次 60 秒的 curl 会把它们一起饿死。
struct AiEdit {
    stage: AiStage,
    /// 作用范围（字节）。空范围 = 整篇。
    scope: std::ops::Range<usize>,
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
            scope: 0..0,
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

/// 左栏显示什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sidebar {
    /// 文档大纲（本文档的标题树）。
    Outline,
    /// 项目目录树。
    Tree,
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
    editor: Entity<InputState>,
    /// 排版结果。缩放时原样不动 —— 这是「排版 / 光栅化」两层的分界。
    doc: Option<Arc<PagedDocument>>,
    /// 缩放倍率。1.0 = 屏幕上「实际大小」（96 dpi）。
    zoom: f32,
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
    dirty: bool,
    /// 一次性的操作反馈（已保存 / 已格式化 / 导出到哪）。
    message: Option<String>,

    /// 项目根目录。快速打开从这里往下找文件。
    root: PathBuf,
    /// 候选文件（相对 root）。每次开快速打开时重扫。
    files: Vec<PathBuf>,
    quick_input: Entity<InputState>,
    /// 当前匹配结果（已排序）。
    quick_matches: Vec<PathBuf>,
    quick_selected: usize,
    quick_visible: bool,
    _quick_sub: Subscription,
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
    /// 左栏模式（大纲 / 目录树）。
    sidebar: Sidebar,
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
    /// 交互式终端（alacritty + pty）。整个生命周期只在 UI 线程用，
    /// PTY 的读写线程由 alacritty 的 EventLoop 自己持有。
    shell_terminal: Option<Rc<terminal::Terminal>>,
    /// 终端自己的焦点：它得能拿到键盘输入
    terminal_focus: FocusHandle,
    /// 终端的输入法状态（中文输入时的预编辑文本）
    terminal_ime: Entity<terminal_view::ImeState>,
    /// 终端面板是否可见（Ctrl+4 切换）
    shell_visible: bool,
    /// 状态栏那个主题下拉框。
    theme_select: Entity<SelectState<Vec<ThemeItem>>>,
    _theme_sub: Subscription,
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

        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .code_editor("typst")
                .default_value(source)
        });

        let sub = cx.subscribe_in(&editor, window, |this, _, event, _window, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_editor_change(cx);
            }
        });

        // 设置里记着上次用的主题就先装上（没有就保持 gpui-component 的默认）
        let theme_name = settings.theme.clone();
        if let Some(name) = settings.theme.clone() {
            match themes::apply(&name, window, cx) {
                Some(dark) => println!("[typst-live] {} 主题 {name}", describe_theme(dark, cx)),
                None => println!("[typst-live] 设置里的主题 {name:?} 不在注册表里，继续用默认"),
            }
        }

        // 目录树状态（扫描在 `refresh_tree` 里做，这里只建壳）
        let tree_state = cx.new(|cx| TreeState::new(cx));
        // Markdown 视图（打开 .md 时喂源码）
        let markdown = cx.new(MarkdownView::new);

        // 主题下拉框。列表在启动时抓一次：主题文件改了要重启才看得见。
        let current_theme = cx.theme().theme_name().to_string();
        let theme_items = ThemeItem::all(&current_theme, cx);
        let theme_path = ThemeItem::index_of(&theme_items).map(|row| IndexPath::default().row(row));
        let theme_select = cx.new(|cx| SelectState::new(theme_items, theme_path, window, cx));
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
                    println!("[typst-live] 终端已启动：{shell}");
                    Some(Rc::new(term))
                }
                Err(err) => {
                    // 终端起不来不该影响编辑器：记一条，继续跑
                    println!("[typst-live] 终端启动失败：{err}");
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

        let theme_sub = cx.subscribe_in(
            &theme_select,
            window,
            |this, _, event: &SelectEvent<Vec<ThemeItem>>, window, cx| {
                let SelectEvent::Confirm(name) = event;
                let Some(name) = name else { return };
                this.use_theme(name.to_string(), window, cx);
            },
        );

        // 快速打开的查询框。做成常驻的（不是每次开关都新建）——
        // 开关只是切一个可见性标志，不涉及 entity 与订阅的生死。
        let quick_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("文件名或路径片段 · ↑↓ 选择 · Enter 打开 · Esc 取消")
                .submit_on_enter(true)
        });
        let quick_sub =
            cx.subscribe_in(
                &quick_input,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Change => this.refresh_quick_matches(cx),
                    InputEvent::PressEnter { .. } => this.confirm_quick(window, cx),
                    _ => {}
                },
            );

        let root = main_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let this = Self {
            engine,
            main_path,
            editor,
            doc: None,
            // 上次的缩放从设置里恢复（还要夹一次，手改过的设置可能越界）
            zoom: settings.zoom.unwrap_or(1.0).clamp(ZOOM_MIN, ZOOM_MAX),
            scale_factor: window.scale_factor(),
            outline: Vec::new(),
            compile_errors: Vec::new(),
            error_count: 0,
            stats: lang::TextStats::default(),
            dirty: false,
            message: None,
            root,
            files: Vec::new(),
            quick_input,
            quick_matches: Vec::new(),
            quick_selected: 0,
            quick_visible: false,
            _quick_sub: quick_sub,
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
            sidebar: Sidebar::Outline,
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
            shell_terminal,
            terminal_focus,
            terminal_ime,
            // 终端默认收起：它是「按需叫出来」的东西，一开窗就占半屏反而吵
            shell_visible: false,
            theme_name,
            theme_select,
            _theme_sub: theme_sub,
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
        if text == self.last_text {
            return;
        }
        self.dirty = true;
        self.recompile(cx);
    }

    /// 一次完整的「编辑 → 重排版 → 重新出图」循环。
    ///
    /// 同步执行：排版只要几毫秒，开线程反而引入调度开销与状态同步的麻烦。
    fn recompile(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        self.last_text = text.clone();
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
            println!(
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
            println!(
                "[typst-live] 排版失败：{} 条错误，第一条：{first}",
                compiled.errors.len()
            );
            self.compile_errors = compiled.errors;
        }

        // ④ 语法服务。刻意用**引擎里那棵增量维护的树**
        //    （`feed_memory` 刚就地重解析过），而不是 `Source::detached(text)`
        //    再全量 parse 一遍。
        self.refresh_language_services(cx);

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
        self.error_count = diags.iter().filter(|d| d.is_error).count();

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

    /// 从已排版的 `doc` 重新出图。**不碰编译器**。
    /// 排版或缩放变了 —— 重建页尺寸并丢掉全部纹理，等下一帧按视口重新出图。
    ///
    /// **不再一次性光栅化全部页**。原因：A4 单页在 100% 下就占 7.7 MiB 纹理，
    /// 100 页就是 770 MiB；400% 时每页 54 MiB，直接爆。现在只出可见的几页。
    fn rerasterize(&mut self) {
        let Some(doc) = self.doc.clone() else {
            self.bitmaps.clear();
            self.page_sizes.clear();
            self.texture_bytes = 0;
            self.live_range = None;
            return;
        };

        // 页的**逻辑**尺寸（布局用）。它等于 pt × 96/72 × zoom ——
        // 屏幕缩放系数在这里恰好抵消，因为逻辑尺寸本就不该依赖显示器。
        let base = typst_engine::export::BASE_PIXEL_PER_PT * self.zoom;
        self.page_sizes = doc
            .pages()
            .iter()
            .map(|p| {
                let s = p.frame.size();
                (s.x.to_pt() as f32 * base, s.y.to_pt() as f32 * base)
            })
            .collect();

        // 全部丢成「未光栅化」占位。布局靠 page_sizes，不靠纹理，
        // 所以占位不会让页面尺寸变化 —— 滚到底、翻页、跳大纲都不受影响。
        self.bitmaps = vec![None; self.page_sizes.len()];
        self.texture_bytes = 0;
        self.live_range = None;
    }

    /// 按当前视口把该出图的页出了，把不该留的丢掉。
    ///
    /// 只在**可见页范围真的变了**的时候动手 —— 滚动过程中每帧都重算
    /// 会让滚动发涩。
    fn sync_visible_pages(&mut self) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let count = doc.pages().len();
        if count == 0 || self.bitmaps.len() != count {
            return;
        }

        // `top_item`/`bottom_item` 是 gpui 给的「当前滚进视口的子项下标」——
        // 我们的子项恰好就是页。还没布局时它们安全地返回 0。
        let first = self.scroll.top_item().min(count - 1);
        let last = self.scroll.bottom_item().min(count - 1);
        let lo = first.saturating_sub(PAGE_PREFETCH);
        let hi = (last + PAGE_PREFETCH).min(count - 1);

        self.current_page = first;
        if self.live_range == Some((lo, hi)) {
            return;
        }
        self.live_range = Some((lo, hi));

        let ppp = pixel_per_pt_for_zoom(self.zoom) * self.scale_factor;
        let t = Instant::now();
        let mut rasterized = 0usize;
        let mut dropped = 0usize;

        // ① 卸载：范围外的纹理丢掉（这是显存真正被释放的地方）
        for (i, slot) in self.bitmaps.iter_mut().enumerate() {
            if (i < lo || i > hi) && slot.is_some() {
                *slot = None;
                dropped += 1;
            }
        }

        // ② 补缺：范围内还没出图的页
        for i in lo..=hi {
            if self.bitmaps[i].is_some() {
                continue;
            }
            let texture = to_texture(rasterize_page(&doc.pages()[i], ppp));
            self.bitmaps[i] = texture;
            rasterized += 1;
        }

        if rasterized > 0 || dropped > 0 {
            self.status.raster_ms = t.elapsed().as_secs_f64() * 1000.0;
            self.status.rasters += 1;
        }
        self.texture_bytes = self
            .bitmaps
            .iter()
            .flatten()
            .map(|b| {
                let s = b.size(0);
                s.width.0 as usize * s.height.0 as usize * 4
            })
            .sum();

        let live = self.bitmaps.iter().filter(|b| b.is_some()).count();
        println!(
            "[typst-live] 视口 {}–{} 页 / 共 {count}：新出图 {rasterized}，卸载 {dropped}；\
             常驻纹理 {live} 页 {:.1} MiB，用时 {:.1} ms",
            lo + 1,
            hi + 1,
            self.texture_bytes as f64 / (1024.0 * 1024.0),
            self.status.raster_ms,
        );
    }

    /// 切到某个主题并记住它。
    fn use_theme(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        match themes::apply(&name, window, cx) {
            Some(dark) => {
                println!("[typst-live] 切主题：{} → {name}", describe_theme(dark, cx));
                self.message = Some(format!("主题 → {name}"));
                self.theme_name = Some(name);
                self.touch_settings(cx);
            }
            None => {
                self.message = Some(format!("不认识的主题：{name}"));
                println!("[typst-live] 不认识的主题：{name}");
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
            cx.background_executor().timer(SETTINGS_DEBOUNCE).await;
            _ = this.update(cx, |this, cx| {
                if this.settings_dirty {
                    this.save_settings(cx);
                }
            });
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
                println!(
                    "[typst-live] 设置已写：{}（第 {} 次）",
                    settings::path().display(),
                    self.settings_writes
                );
                self.settings = next;
            }
            Err(err) => println!("[typst-live] 设置写不进去：{err}"),
        }
        cx.notify();
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = zoom;
        // 只重做出图，不重新排版 —— 这就是缩放能任意清晰的原因。
        self.rerasterize();
        // 缩放是要记住的（下次开窗应该还是这个大小）
        self.touch_settings(cx);
        cx.notify();
    }

    /// 缩放并把当前页重新锚回预览顶部。
    fn set_zoom_anchored(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let before = self.zoom;
        self.set_zoom(zoom, cx);
        if (self.zoom - before).abs() > f32::EPSILON {
            self.scroll.scroll_to_top_of_item(self.current_page);
        }
    }

    /// 把第 `page` 页（0 起）滚到预览顶部。越界会被夹到合法范围。
    fn go_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.bitmaps.is_empty() {
            return;
        }
        let page = page.min(self.bitmaps.len() - 1);
        self.current_page = page;
        self.scroll.scroll_to_top_of_item(page);
        self.message = Some(format!("第 {} / {} 页", page + 1, self.bitmaps.len()));
        cx.notify();
    }

    /// Ctrl+S：把编辑器里的文本写到 `main_path`。
    fn save_file(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();

        match std::fs::write(&self.main_path, &text) {
            Ok(()) => {
                // 磁盘与内存一致了，撤掉覆盖层 —— 从此磁盘就是真相。
                // （主文件的文本还在 SourceDb 里，所以不会因此重新解析。）
                self.engine.vfs_mut().unmap_shadow(&self.main_path);
                self.dirty = false;
                self.message = Some(format!("已保存 {}", self.main_path.display()));
            }
            Err(err) => self.message = Some(format!("保存失败：{err}")),
        }
        cx.notify();
    }

    /// Ctrl+B：手动重新编译。
    ///
    /// 不只是「再排一次」—— 它先把源文件缓存**全部作废**，
    /// 这样被 `#include` 的文件如果被外部改过也能重新读进来
    /// （我们没有文件监听，所以这是手动补上那个缺口）。
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
                self.dirty = true;
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

        self.dirty = true;
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

                view.update(cx, |this, cx| {
                    if let Some(code) = exited {
                        this.message = Some(format!("终端已退出（退出码 {code:?}）"));
                    }
                    if has_wakeup {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// 终端面板：标题栏 + 终端本体。与 `wu` 同一种摆法。
    fn render_shell(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let body: AnyElement = match &self.shell_terminal {
            Some(term) => {
                // 终端跟随应用主题的亮/暗（这与「用固定深色终端」是个取舍）：
                // 浅色主题下若还配深色终端，界面会像贴了块补丁；
                // 而浅色底必须配 `TerminalPalette::light()` —— ANSI 经典黄/亮黄
                // 在近白底上对比度不到 1.5:1，基本看不清（`term_colors` 里有说明）。
                let (fg, bg, palette) = if theme.is_dark() {
                    (
                        gpui::white(),
                        gpui::rgb(0x1e1e1e).into(),
                        TerminalPalette::dark(),
                    )
                } else {
                    (
                        gpui::rgb(0x24292e).into(),
                        gpui::rgb(0xfbfbfb).into(),
                        TerminalPalette::light(),
                    )
                };
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .child(
                        terminal_view::TerminalElement::new(
                            term.clone(),
                            self.terminal_focus.clone(),
                            self.terminal_ime.clone(),
                        )
                        .colors(fg, bg)
                        .palette(palette)
                        .min_contrast(4.5)
                        .track_focus(&self.terminal_focus),
                    )
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("终端没起来（启动时失败，看终端日志）")
                .into_any_element(),
        };

        v_flex()
            .w_full()
            .h(px(260.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .h(px(24.))
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .text_xs()
                    .child("终端")
                    .child(
                        div().ml_auto().child(
                            Button::new("shell-close")
                                .ghost()
                                .label("关闭")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.shell_visible = false;
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    // ── AI 编辑（Ctrl+K）─────────────────────────────────

    /// Ctrl+K：打开 AI 编辑。有选区就改选区，没有就改整篇。
    fn open_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.editor.read(cx).selected_range();
        let (scope, label) = if selection.is_empty() {
            (0..0, "整篇文档".to_string())
        } else {
            let text = self.editor.read(cx).text().to_string();
            let picked = text
                .get(selection.clone())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let preview: String = picked.chars().take(24).collect();
            let more = if picked.chars().count() > 24 {
                "…"
            } else {
                ""
            };
            (
                selection.clone(),
                format!("选区 {} 字：{preview}{more}", picked.chars().count()),
            )
        };

        self.ai = AiEdit {
            stage: AiStage::Ask,
            scope,
            scope_label: label,
            ..AiEdit::default()
        };
        self.message = None;

        self.ai_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
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

        // 上下文：有选区就只给选区（省钱、也更准），否则给截断过的全文
        let full = self.editor.read(cx).text().to_string();
        let raw_context = if self.ai.scope.is_empty() {
            full.clone()
        } else {
            full.get(self.ai.scope.clone())
                .unwrap_or_default()
                .to_string()
        };
        let (context, truncated) = ai::truncate_context(&raw_context, ai::AI_EDIT_CONTEXT_LIMIT);
        if truncated {
            println!(
                "[typst-live] AI 上下文超限，已截断到 {} 字",
                ai::AI_EDIT_CONTEXT_LIMIT
            );
        }

        let messages = ai::edit_messages(&context, &self.ai_history, &instruction);
        self.ai_history.push((true, instruction.clone()));
        self.spawn_ai(messages, raw_context, self.ai.scope.clone(), cx);
    }

    /// 把一组 messages 交给模型（起 std 线程 + 轮询回执）。两个入口共用：
    /// `Ctrl+K` 的自由指令，与菜单里的固定任务。
    fn spawn_ai(
        &mut self,
        messages: Vec<(String, String)>,
        original: String,
        scope: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let base_url = ai::resolve_base_url(self.settings.ai_base_url.as_deref());
        let model = ai::resolve_model(self.settings.ai_model.as_deref());
        let key = ai::api_key(&base_url, self.settings.ai_api_key.as_deref());

        // 云端端点没 Key 是**等会儿必然失败**，不如当场说清楚怎么配；
        // 本地端点（Ollama 之类）本来就不需要 Key。
        if key.is_none() && !ai::is_local_endpoint(&base_url) {
            self.message = Some(
                "AI 需要鉴权：设环境变量 AI_API_KEY，或在 settings.conf 里写 ai_api_key"
                    .to_string(),
            );
        }

        println!(
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

                let done = this
                    .update(cx, |this, cx| this.take_ai_result(cx))
                    .unwrap_or(true);
                if done {
                    break;
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
    fn run_ai_task(&mut self, task: AiTask, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.editor.read(cx).selected_range();
        let (scope, label, source) = if selection.is_empty() {
            let text = self.editor.read(cx).text().to_string();
            (0..0, "整篇文档".to_string(), text)
        } else {
            let text = self.editor.read(cx).text().to_string();
            let picked = text.get(selection.clone()).unwrap_or_default().to_string();
            (
                selection.clone(),
                format!("选区 {} 字", picked.chars().count()),
                picked,
            )
        };

        if source.trim().is_empty() {
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

        self.ai = AiEdit {
            stage: AiStage::Running,
            scope,
            scope_label: format!("{} · {label}", task.label()),
            ..AiEdit::default()
        };
        self.ai_history.clear();
        self.message = Some(format!("AI 任务：{}", task.label()));
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
                    self.ai.hunks = diff::hunks(&self.ai.original, &cleaned);
                    self.ai.accepted = vec![true; self.ai.hunks.len()];
                    self.ai.selected = 0;
                    self.ai.result = cleaned.clone();
                    self.ai.stage = AiStage::Review;
                    self.ai_history.push((false, cleaned.clone()));
                    println!(
                        "[typst-live] AI 返回 {} 字，切成 {} 个可逐块接受的改动",
                        cleaned.chars().count(),
                        self.ai.hunks.len()
                    );
                }
            }
            Err(err) => {
                self.ai.error = Some(err.clone());
                self.ai.stage = AiStage::Ask;
                println!("[typst-live] AI 失败：{err}");
            }
        }

        cx.notify();
        true
    }

    /// 应用：按接受标记重建那段文字，再拼回全文。
    fn apply_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.stage != AiStage::Review {
            return;
        }

        let applied = diff::apply_hunks(&self.ai.original, &self.ai.result, &self.ai.accepted);
        let full = self.editor.read(cx).text().to_string();

        let next = if self.ai.scope.is_empty() {
            applied
        } else {
            // 选区模式：把应用后的片段拼回全文
            let start = self.ai.scope.start.min(full.len());
            let end = self.ai.scope.end.min(full.len());
            format!("{}{}{}", &full[..start], applied, &full[end..])
        };

        let kept = self
            .ai
            .accepted
            .iter()
            .filter(|accepted| **accepted)
            .count();
        let total = self.ai.hunks.len();

        // `set_value` 走的是静默路径（不发 Change 事件），所以自己重排一遍
        self.editor
            .update(cx, |state, cx| state.set_value(next, window, cx));
        self.ai = AiEdit::default();
        self.dirty = true;
        self.recompile(cx);

        self.message = Some(format!("AI 改动已应用（{kept}/{total} 块）"));
        println!("[typst-live] AI 应用：接受 {kept}/{total} 块");
        cx.notify();
    }

    // ── 双向跳转（双击）──────────────────────────────────────

    /// 按需建索引，并按**排版结果**缓存。
    ///
    /// 「排版 → 索引 → 光栅化」三层里中间那层的全部实现就在这里：
    /// 排版结果没换（`Arc` 指针相同）就直接复用，所以**缩放、滚动、
    /// 翻页都不会重建它**。状态栏的「索引 N 次」会把这件事直接显示出来。
    fn rebuild_index(&mut self) {
        let Some(doc) = self.doc.clone() else { return };
        if self
            .index
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(&c.doc, &doc))
        {
            return;
        }
        if !self.index_usable {
            // 上一次排版没成功：预览里是旧结果，源码已经变了，对不上。
            return;
        }
        let Ok(source) = typst::World::source(&self.engine, self.engine.entry().main()) else {
            return;
        };

        let started = Instant::now();
        let index = LayoutIndex::build(&doc, &source);
        self.index_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.index_builds += 1;

        println!(
            "[typst-live] 建跳转索引：{} 页 / {} 个字形，用时 {:.1} ms（第 {} 次）",
            index.page_count(),
            index.glyph_count(),
            self.index_ms,
            self.index_builds,
        );
        self.index = Some(IndexCache { doc, index });
    }

    /// 源码第 `byte` 个字节 → 显示区的位置。
    fn forward_anchor(&mut self, byte: usize) -> Option<Anchor> {
        self.rebuild_index();
        self.index.as_ref()?.index.forward(byte)
    }

    /// 显示区第 `page` 页的 `(x, y)` pt → 源码的位置。
    fn inverse_anchor(&mut self, page: usize, x: f32, y: f32) -> Option<Anchor> {
        self.rebuild_index();
        self.index.as_ref()?.index.inverse(page, x, y)
    }

    /// 显示区 → 编辑区：把光标放到那一处。
    ///
    /// `set_cursor_position` 内部会 focus 并滚到光标上，所以不需要
    /// 我们管编辑器的滚动。
    fn jump_to_source(
        &mut self,
        page: usize,
        x: f32,
        y: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self.inverse_anchor(page, x, y) else {
            self.message = Some(format!("第 {} 页这里没有可定位的文字", page + 1));
            cx.notify();
            return;
        };

        let rope = self.editor.read(cx).text().clone();
        let position = rope.offset_to_position(anchor.byte.start);
        let line = position.line + 1;
        self.editor.update(cx, |state, cx| {
            state.set_cursor_position(position, window, cx);
        });

        println!(
            "[typst-live] 反向跳转：第 {} 页 ({x:.1}, {y:.1}) pt → 第 {line} 行（字节 {}..{}）",
            anchor.page + 1,
            anchor.byte.start,
            anchor.byte.end,
        );
        self.flash_on(anchor.page, anchor.rect, cx);
        self.message = Some(format!(
            "显示区 → 第 {line} 行（第 {} 页）",
            anchor.page + 1
        ));
        cx.notify();
    }

    /// 编辑区 → 显示区：把光标所在处滚进预览。
    fn jump_to_preview(&mut self, cx: &mut Context<Self>) {
        let byte = self.editor.read(cx).cursor();
        let Some(anchor) = self.forward_anchor(byte) else {
            self.message = Some(
                if self.doc.is_none() {
                    "还没排过版，没地方可跳"
                } else if self.index_usable {
                    "这一份文档里没有可定位的文字"
                } else {
                    "上一次排版没成功，跳转暂时关掉（预览是旧结果）"
                }
                .to_owned(),
            );
            cx.notify();
            return;
        };

        // 页内 pt → 逻辑像素 → 窗口坐标。
        //
        // 页框位置直接问 `ScrollHandle` 要，而不是自己把上面所有页的高度
        // 加起来 —— 那样得同时算对页间距、内边距、缩放，迟早会错位。
        if let Some(now) = self.page_pt_to_window(anchor.page, anchor.rect[0], anchor.rect[1]) {
            let viewport = self.scroll.bounds();
            let desired = viewport.origin.y + px(SYNC_MARGIN);
            let offset = self.scroll.offset();
            self.scroll
                .set_offset(point(offset.x, offset.y + (desired - now.y)));
        }
        self.current_page = anchor.page;

        let rope = self.editor.read(cx).text().clone();
        let line = rope.offset_to_position(anchor.byte.start).line + 1;
        println!(
            "[typst-live] 前向跳转：第 {line} 行（字节 {byte}）→ 第 {} 页，页内 y={:.1} pt",
            anchor.page + 1,
            anchor.rect[1],
        );
        self.flash_on(anchor.page, anchor.rect, cx);
        self.message = Some(format!("第 {line} 行 → 第 {} 页", anchor.page + 1));
        cx.notify();
    }

    /// 窗口坐标 → 页内 pt。
    ///
    /// 算术在 [`coords`] 里（纯函数 + 单测）。这里只负责把 gpui 的类型拆开。
    fn window_to_page_pt(&self, position: Point<Pixels>, page: usize) -> Option<(f32, f32)> {
        let bounds = self.scroll.bounds_for_item(page)?;
        let offset = self.scroll.offset();
        Some(coords::page_pt_from_window(
            (position.x.as_f32(), position.y.as_f32()),
            (bounds.origin.x.as_f32(), bounds.origin.y.as_f32()),
            (offset.x.as_f32(), offset.y.as_f32()),
            self.zoom,
        ))
    }

    /// 页内 pt → 窗口坐标（[`Self::window_to_page_pt`] 的反函数）。
    ///
    /// 前向跳转靠它算出「目标现在画在哪」，再据此定新的滚动偏移。
    fn page_pt_to_window(&self, page: usize, x: f32, y: f32) -> Option<Point<Pixels>> {
        let bounds = self.scroll.bounds_for_item(page)?;
        let offset = self.scroll.offset();
        let (wx, wy) = coords::window_from_page_pt(
            (x, y),
            (bounds.origin.x.as_f32(), bounds.origin.y.as_f32()),
            (offset.x.as_f32(), offset.y.as_f32()),
            self.zoom,
        );
        Some(point(px(wx), px(wy)))
    }

    /// Ctrl+单击：点在链接上就打开它。
    ///
    /// 为什么加 Ctrl：普通单击不能直接开浏览器 —— 双击跳转的**第一次点击**
    /// 也会先报一次 `click_count == 1`，那就会把链接顺手开掉。
    fn open_link_at(&mut self, page: usize, x: f32, y: f32, cx: &mut Context<Self>) {
        self.rebuild_index();

        // 先把命中结果收成自有数据，再做副作用 —— 不然索引的借用与
        // `self.message = ...` 的可变借用会打架。
        let hit = self.index.as_ref().and_then(|cache| {
            cache
                .index
                .links(page)
                .iter()
                .find(|link| {
                    x >= link.rect[0] && x <= link.rect[2] && y >= link.rect[1] && y <= link.rect[3]
                })
                .cloned()
        });

        self.message = Some(match hit.map(|link| link.dest) {
            Some(Destination::Url(url)) => {
                let url = url.as_str().to_owned();
                println!("[typst-live] 打开链接：{url}");
                cx.open_url(&url);
                format!("打开 {url}")
            }
            Some(_) => "这是文档内部的链接，暂时打不开（目前只支持网址）".to_owned(),
            None => "这里没有链接".to_owned(),
        });
        cx.notify();
    }

    /// 在某一页留下一块会淡出的高亮。
    ///
    /// 跨页跳过去之后，没有这个框人得自己找位置 —— 它是「跳过去了」的
    /// 唯一视觉证据。
    fn flash_on(&mut self, page: usize, rect: [f32; 4], cx: &mut Context<Self>) {
        self.flash_seq += 1;
        self.flash = Some(Flash {
            page,
            rect,
            seq: self.flash_seq,
        });
        cx.notify();
    }

    // ── 快速跳转（Ctrl+P）────────────────────────────────────────────

    /// Ctrl+P：扫描项目目录并弹出快速打开。
    fn show_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 每次重扫：文件是会被外部增删的，缓一份可能会看不到新建的文件。
        self.files = finder::scan(&self.root, MAX_SCAN_FILES);
        self.quick_visible = true;
        self.message = None;

        self.quick_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        // 空 query → 列全部（已排序），所以这里不用再调 refresh
        self.quick_matches = self.files.iter().take(MAX_QUICK_MATCHES).cloned().collect();
        self.quick_selected = 0;
        cx.notify();
    }

    /// 查询变化 → 重新排序。
    fn refresh_quick_matches(&mut self, cx: &mut Context<Self>) {
        if !self.quick_visible {
            return;
        }
        let query = self.quick_input.read(cx).value().trim().to_string();

        self.quick_matches = if query.is_empty() {
            self.files.iter().take(MAX_QUICK_MATCHES).cloned().collect()
        } else {
            finder::rank(&query, &self.files, MAX_QUICK_MATCHES)
        };
        self.quick_selected = 0;
        cx.notify();
    }

    /// ↑↓：在候选里移动。到头就停住，不循环 —— 循环会让人分不清首尾。
    fn quick_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.quick_matches.is_empty() {
            return;
        }
        let last = self.quick_matches.len() - 1;
        let next = self.quick_selected as isize + delta;
        self.quick_selected = next.clamp(0, last as isize) as usize;
        cx.notify();
    }

    /// Enter：打开选中的文件。
    fn confirm_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rel) = self.quick_matches.get(self.quick_selected).cloned() else {
            self.cancel_quick(cx);
            return;
        };
        let full = self.root.join(&rel);

        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(err) => {
                self.message = Some(format!("读不了 {}：{err}", full.display()));
                cx.notify();
                return;
            }
        };

        self.quick_visible = false;
        let label = rel.to_string_lossy().replace('\\', "/");
        self.open_in_editor(full, text, label, window, cx);
    }

    /// 在编辑器里打开一个文本文件。目录树点击与快速打开共用它。
    fn open_in_editor(
        &mut self,
        full: PathBuf,
        text: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if full == self.main_path {
            self.message = Some(format!("已经在编辑 {label}"));
            cx.notify();
            return;
        }

        // 换文档：字体留着，入口 / 覆盖层 / 源缓存 / 上次成功结果都重置。
        self.engine.reopen(&self.root, &full);
        self.main_path = full;
        // 打开的是用户自己的文件 —— 从此它就是「上次打开的文件」
        self.explicit_file = true;
        self.touch_settings(cx);
        self.doc = None;
        self.bitmaps.clear();
        self.texture_bytes = 0;
        self.current_page = 0;
        self.dirty = false;
        // 置空是为了让下面的 recompile 不受「文本没变就不排」的干扰
        self.last_text = String::new();

        self.right = RightPane::Preview;
        self.message = Some(format!("打开 {label}"));

        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
        self.recompile(cx);
    }

    // ── 目录树 / 左栏 / 右侧主区 ─────────────────────────

    fn set_sidebar(&mut self, mode: Sidebar, cx: &mut Context<Self>) {
        self.sidebar = mode;
        if mode == Sidebar::Tree {
            self.refresh_tree(cx);
        }
        cx.notify();
    }

    fn set_right(&mut self, mode: RightPane, cx: &mut Context<Self>) {
        self.right = mode;
        cx.notify();
    }

    /// 扫项目目录 → 重建目录树条目（按 `tree_expanded` 恢复展开状态）。
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let started = Instant::now();
        let nodes = tree::scan_dir(&root);
        let count = tree::count_nodes(&nodes);
        let items = tree::build_file_items(&root, nodes, &self.tree_expanded);
        self.tree_root = Some(root.clone());
        self.tree_state
            .update(cx, |state, cx| state.set_items(items, cx));

        println!(
            "[typst-live] 目录树：{count} 个条目，用时 {:.1} ms（{}）",
            started.elapsed().as_secs_f64() * 1000.0,
            root.display()
        );
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

    /// Esc / 点空白：关掉，不动已打开的文件。
    fn cancel_quick(&mut self, cx: &mut Context<Self>) {
        self.quick_visible = false;
        self.quick_matches.clear();
        self.quick_selected = 0;
        cx.notify();
    }

    /// 顶部菜单条（文件 / 视图）。
    ///
    /// 用 gpui-component 的 `dropdown_menu` —— 与 `wu` 同一个路子。
    fn render_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let view = cx.entity();

        let file_menu = {
            let view = view.clone();
            Button::new("menu-file")
                .ghost()
                .compact()
                .label("文件")
                .dropdown_menu(move |menu, window, _cx| {
                    let view = view.clone();
                    menu.min_w(220.)
                        .item(PopupMenuItem::new("快速打开…（Ctrl+P）").on_click(
                            window.listener_for(&view, |this, _, window, cx| {
                                this.show_quick_open(window, cx);
                            }),
                        ))
                        .separator()
                        .item(
                            PopupMenuItem::new("保存（Ctrl+S）").on_click(window.listener_for(
                                &view,
                                |this, _, _window, cx| {
                                    this.save_file(cx);
                                },
                            )),
                        )
                        .item(PopupMenuItem::new("重新编译（Ctrl+B）").on_click(
                            window.listener_for(&view, |this, _, _window, cx| {
                                this.recompile_now(cx);
                            }),
                        ))
                        .item(PopupMenuItem::new("导出 PDF（Ctrl+E）").on_click(
                            window.listener_for(&view, |this, _, _window, cx| {
                                this.export_pdf(cx);
                            }),
                        ))
                        .separator()
                        .item(PopupMenuItem::new("AI 编辑…（Ctrl+K）").on_click(
                            window.listener_for(&view, |this, _, window, cx| {
                                this.open_ai(window, cx);
                            }),
                        ))
                        .separator()
                        .item(PopupMenuItem::new("退出").on_click(|_, _, cx: &mut App| cx.quit()))
                })
        };

        let view_menu =
            {
                let view = view.clone();
                Button::new("menu-view")
                    .ghost()
                    .compact()
                    .label("视图")
                    .dropdown_menu(move |menu, window, _cx| {
                        let view = view.clone();
                        menu.min_w(220.)
                            .item(PopupMenuItem::new("大纲").on_click(
                                window.listener_for(&view, |this, _, _window, cx| {
                                    this.set_sidebar(Sidebar::Outline, cx)
                                }),
                            ))
                            .item(PopupMenuItem::new("目录树").on_click(
                                window.listener_for(&view, |this, _, _window, cx| {
                                    this.set_sidebar(Sidebar::Tree, cx)
                                }),
                            ))
                            .item(PopupMenuItem::new("终端（Ctrl+4）").on_click(
                                window.listener_for(&view, |this, _, window, cx| {
                                    this.shell_visible = !this.shell_visible;
                                    if this.shell_visible {
                                        this.terminal_focus.focus(window, cx);
                                    }
                                    cx.notify();
                                }),
                            ))
                            .separator()
                            .item(PopupMenuItem::new("排版预览").on_click(
                                window.listener_for(&view, |this, _, _window, cx| {
                                    this.set_right(RightPane::Preview, cx)
                                }),
                            ))
                            .item(
                                PopupMenuItem::new("刷新目录树").on_click(window.listener_for(
                                    &view,
                                    |this, _, _window, cx| {
                                        this.refresh_tree(cx);
                                        cx.notify();
                                    },
                                )),
                            )
                            .separator()
                            .item(PopupMenuItem::new("放大（Ctrl+=）").on_click(
                                window.listener_for(&view, |this, _, _window, cx| {
                                    let next = this.zoom * ZOOM_STEP;
                                    this.set_zoom(next, cx);
                                }),
                            ))
                            .item(PopupMenuItem::new("缩小（Ctrl+-）").on_click(
                                window.listener_for(&view, |this, _, _window, cx| {
                                    let next = this.zoom / ZOOM_STEP;
                                    this.set_zoom(next, cx);
                                }),
                            ))
                            .item(PopupMenuItem::new("实际大小（Ctrl+0）").on_click(
                                window.listener_for(&view, |this, _, _window, cx| {
                                    this.set_zoom(1.0, cx);
                                }),
                            ))
                    })
            };

        let ai_menu = {
            let view = view.clone();
            Button::new("menu-ai")
                .ghost()
                .compact()
                .label("AI")
                .dropdown_menu(move |menu, window, _cx| {
                    let view = view.clone();
                    let mut menu = menu.min_w(220.);
                    menu = menu.item(PopupMenuItem::new("AI 编辑…（Ctrl+K）").on_click(
                        window.listener_for(&view, |this, _, window, cx| this.open_ai(window, cx)),
                    ));
                    menu = menu.separator();
                    for (label, task) in [
                        ("语法检查并修复", AiTask::SyntaxFix),
                        ("中文校对", AiTask::Proofread),
                        ("术语一致性", AiTask::Terminology),
                        ("中译英", AiTask::TranslateToEnglish),
                        ("英译中", AiTask::TranslateToChinese),
                    ] {
                        menu = menu.item(PopupMenuItem::new(label).on_click(window.listener_for(
                            &view,
                            move |this, _, window, cx| {
                                this.run_ai_task(task, window, cx);
                            },
                        )));
                    }
                    menu
                })
        };

        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_2()
            .py_0p5()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(file_menu)
            .child(view_menu)
            .child(ai_menu)
            .child(
                div()
                    .ml_auto()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.main_path.display().to_string()),
            )
    }

    /// 左栏：大纲 / 目录树（顶部两个页签切换）。
    fn render_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let tab = |label: &'static str, mode: Sidebar, cx: &Context<Self>| {
            Button::new(label)
                .ghost()
                .compact()
                .label(label)
                .selected(self.sidebar == mode)
                .on_click(cx.listener(move |this, _, _window, cx| this.set_sidebar(mode, cx)))
        };

        let body = match self.sidebar {
            Sidebar::Outline => self.render_outline_body(cx),
            Sidebar::Tree => self.render_tree_body(cx),
        };

        v_flex()
            .w(px(240.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(tab("大纲", Sidebar::Outline, cx))
                    .child(tab("目录", Sidebar::Tree, cx))
                    .child(
                        div().ml_auto().child(
                            Button::new("tree-refresh")
                                .ghost()
                                .compact()
                                .icon(IconName::Redo)
                                .tooltip("重新扫描项目目录")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.refresh_tree(cx);
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .child(body)
    }

    /// 目录树本体：点文件 → 按类型显示到右侧。
    fn render_tree_body(&self, cx: &Context<Self>) -> AnyElement {
        if self.tree_root.is_none() {
            return div()
                .flex_1()
                .p_3()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("还没有扫描目录 —— 点右上角的刷新")
                .into_any_element();
        }

        let view = cx.entity();
        Tree::new(
            &self.tree_state,
            move |ix, entry, _selected, _window, cx| {
                // `render_item` 拿到的是 `&mut App`，要 `Context<Self>` 才能挂点击回调；
                // 借 `view.update` 换一个上下文出来（gpui-component 自己的 story 也这么写）。
                view.update(cx, |_, cx| {
                    let icon: AnyElement = if entry.is_folder() {
                        let name = if entry.is_expanded() {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        };
                        Icon::from(name)
                            .text_color(cx.theme().primary)
                            .into_any_element()
                    } else {
                        tree::file_type_icon(Path::new(entry.item().id.as_ref()))
                    };

                    let path = PathBuf::from(entry.item().id.as_ref());
                    ListItem::new(ix)
                        .w_full()
                        .rounded(cx.theme().radius)
                        .px_2()
                        .pl(px(12.) * entry.depth() + px(6.))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(icon)
                                .child(entry.item().label.clone()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // 目录的展开/收起由 Tree 自己做了（它在外层包了一个
                            // mouse_down 调 toggle），这里只管打开文件
                            if path.is_dir() {
                                return;
                            }
                            this.open_path(path.clone(), window, cx);
                        }))
                })
            },
        )
        .flex_1()
        .into_any_element()
    }

    /// 右侧主区：预览 / Markdown / 图片（顶部页签切换）。
    fn render_right_pane(&self, preview: AnyElement, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let tab = |label: &'static str, mode: RightPane, cx: &Context<Self>| {
            Button::new(label)
                .ghost()
                .compact()
                .label(label)
                .selected(self.right == mode)
                .on_click(cx.listener(move |this, _, _window, cx| this.set_right(mode, cx)))
        };

        let body: AnyElement = match self.right {
            RightPane::Preview => preview,
            RightPane::Markdown => {
                if self.markdown.read(cx).is_empty() {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("在左边目录树里点一个 .md 文件")
                        .into_any_element()
                } else {
                    div()
                        .size_full()
                        .child(self.markdown.clone())
                        .into_any_element()
                }
            }
            RightPane::Image => match &self.image {
                Some(view) => div().size_full().child(view.clone()).into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("在左边目录树里点一张图片（png/jpg/webp/gif/bmp/svg）")
                    .into_any_element(),
            },
        };

        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(tab("排版预览", RightPane::Preview, cx))
                    .child(tab("Markdown", RightPane::Markdown, cx))
                    .child(tab("图片", RightPane::Image, cx)),
            )
            .child(div().flex_1().min_h_0().w_full().child(body))
    }

    /// AI 编辑浮层：输入要求 → 生成中 → 逐块确认。
    ///
    /// 三个阶段共用一层壳（标题 + 内容 + 底部提示），只在中间换内容。
    fn render_ai(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let (title, hint) = match self.ai.stage {
            AiStage::Closed => return div().into_any_element(),
            AiStage::Ask => ("AI 编辑", "Enter 发送 · Esc 取消"),
            AiStage::Running => ("AI 编辑 · 生成中", "Esc 取消"),
            AiStage::Review => (
                "AI 编辑 · 确认改动",
                "Enter 应用 · Tab/空格 切换本块 · ↑↓ 选块 · Esc 放弃",
            ),
        };

        let body: AnyElement = match self.ai.stage {
            AiStage::Closed => div().into_any_element(),

            AiStage::Ask | AiStage::Running => {
                let mut column = v_flex().w_full().gap_2();
                if self.ai.stage == AiStage::Running {
                    let generated = self.ai.progress.load(Ordering::Relaxed);
                    column = column.child(
                        div()
                            .text_sm()
                            .text_color(theme.primary)
                            .child(format!("已生成 {generated} 字…")),
                    );
                }
                column = column.child(Input::new(&self.ai_input).w_full());
                if let Some(error) = &self.ai.error {
                    column = column.child(
                        div()
                            .text_xs()
                            .text_color(theme.danger)
                            .child(format!("失败：{error}")),
                    );
                }
                column.into_any_element()
            }

            AiStage::Review => {
                let mut list = v_flex()
                    .id("ai-hunks")
                    .w_full()
                    .gap_1()
                    .max_h(px(380.))
                    .overflow_y_scroll();
                for (index, hunk) in self.ai.hunks.iter().enumerate() {
                    let accepted = self.ai.accepted.get(index).copied().unwrap_or(true);
                    let selected = index == self.ai.selected;

                    let mut block = v_flex()
                        .w_full()
                        .rounded(theme.radius)
                        .px_2()
                        .py_1()
                        .gap_0p5()
                        .bg(if selected {
                            theme.accent.opacity(0.18)
                        } else {
                            theme.secondary
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .child(div().text_color(theme.primary).child(format!(
                                    "第 {} 块  +{}  -{}",
                                    index + 1,
                                    hunk.added(),
                                    hunk.removed()
                                )))
                                .child(
                                    div()
                                        .text_color(if accepted {
                                            theme.success
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(if accepted { "接受" } else { "拒绝" }),
                                ),
                        );

                    for line in &hunk.lines {
                        let (sign, color) = match line.kind {
                            diff::DiffKind::Add => ("+", theme.success),
                            diff::DiffKind::Del => ("-", theme.danger),
                        };
                        block = block.child(
                            div()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(color)
                                .child(format!("{sign} {}", line.text)),
                        );
                    }

                    list = list.child(block);
                }

                if self.ai.hunks.is_empty() {
                    list = list.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("模型返回的内容与原文本一致（没有可应用的改动）"),
                    );
                }

                list.into_any_element()
            }
        };

        let panel = v_flex()
            .key_context("AiEdit")
            .track_focus(&self.ai_focus)
            .w(px(700.))
            .max_h(px(560.))
            .gap_2()
            .p_3()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child(title))
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.ai.scope_label.clone()),
                    ),
            )
            .child(body)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(hint),
            );

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(90.))
            .bg(gpui::black().opacity(0.25))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.cancel_ai(cx)),
            )
            .child(panel)
            .into_any_element()
    }

    /// 底部状态栏：**一行**装下引擎指标 + 文档统计 + 保存/错误 + 一次性消息 + 主题。
    ///
    /// 放在窗口最底部（终端面板之下）—— 与大多数编辑器一致：
    /// 上面是「内容」，最下面一条是「状态」。
    ///
    /// 指标一多就必须想清楚**哪部分可以牺牲**：左侧指标放一个 `flex_1` +
    /// `overflow_hidden` 的容器里（窄窗口下宁可裁掉几个数字），
    /// 右侧的消息与主题下拉是**固定**的，永远看得见。
    fn render_statusbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let mut metrics = h_flex()
            .gap_3()
            .text_xs()
            .child(
                div()
                    .text_color(theme.primary)
                    .child(format!("排版 {:.1}ms", self.status.compile_ms)),
            )
            .child(format!("光栅化 {:.1}ms", self.status.raster_ms));

        if self.index_builds > 0 {
            metrics = metrics.child(format!("索引 {:.1}ms", self.index_ms));
        }

        metrics = metrics
            .child(format!(
                "重解析 {}B/{}B",
                self.status.reparsed, self.status.text_bytes
            ))
            .child(format!(
                "{:.1}MiB",
                self.texture_bytes as f64 / (1024.0 * 1024.0)
            ))
            .child(if self.status.pages == 0 {
                "0 页".to_string()
            } else {
                format!("{}/{} 页", self.current_page + 1, self.status.pages)
            })
            .child(
                div()
                    .text_color(theme.primary)
                    .child(format!("{:.0}%", self.zoom * 100.0)),
            )
            .child(format!(
                "排版{}·光栅{}·索引{}",
                self.status.compiles, self.status.rasters, self.index_builds
            ))
            .child(self.stats.summary())
            .child(format!("{} 标题", self.outline.len()))
            .child(if self.dirty {
                "● 未保存"
            } else {
                "已保存"
            })
            .child(if self.error_count > 0 {
                div()
                    .text_color(theme.danger)
                    .child(format!("✗ {} 错误", self.error_count))
                    .into_any_element()
            } else {
                div()
                    .text_color(theme.success)
                    .child("✓ 无错误")
                    .into_any_element()
            });

        let mut bar = h_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_3()
            // 贴着窗口底边，所以边框在上（原先是顶栏，边框在下）
            .border_t_1()
            .border_color(theme.border)
            .text_sm()
            .child(div().flex_1().min_w_0().overflow_hidden().child(metrics));

        if let Some(msg) = &self.message {
            bar = bar.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.primary)
                    .child(msg.clone()),
            );
        }

        bar.child(
            div()
                .flex_shrink_0()
                .child(Select::new(&self.theme_select).w(px(150.))),
        )
    }

    /// 大纲本体（左栏的标题与边框由 `render_sidebar` 统一负责）。
    fn render_outline_body(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();

        let body = v_flex()
            .id("outline-body")
            .flex_1()
            .overflow_y_scroll()
            .py_1()
            .children(if self.outline.is_empty() {
                vec![
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("（还没有标题）")
                        .into_any_element(),
                ]
            } else {
                // 先收成自有数据再建元素：避免在闭包里借用 `self.outline`。
                let rows: Vec<(usize, String, usize)> = self
                    .outline
                    .iter()
                    .map(|i| (i.depth, i.title.clone(), i.line))
                    .collect();

                rows.into_iter()
                    .map(|(depth, title, line)| {
                        div()
                            .px_3()
                            .py_0p5()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if depth == 1 {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .hover(|s| s.bg(theme.accent.opacity(0.12)))
                            .child(format!(
                                "{}{}",
                                "    ".repeat(depth.saturating_sub(1)),
                                title
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                    this.jump_to_line(line, window, cx);
                                }),
                            )
                            .into_any_element()
                    })
                    .collect()
            });

        body.into_any_element()
    }

    fn render_errors(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_1()
            .bg(theme.danger.opacity(0.08))
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.danger)
            .children(
                self.compile_errors
                    .iter()
                    .take(6)
                    .map(|e| div().child(e.message.to_string())),
            )
    }
}

/// 波浪线的终点。
///
/// 至少覆盖一个字符；若诊断带着可用的字节范围、且终点**在同一行**，
/// 就盖住整个病灶而不是戳一个孤零零的点（跨行的话用一个字符收尾，
/// 免得算出一段横跨多行的矩形）。
fn squiggle_end(
    source: &typst::syntax::Source,
    d: &lang::Diagnostic,
    start: lang::LineCol,
) -> lang::LineCol {
    let one_char = lang::LineCol {
        line: start.line,
        col: start.col + 1,
    };

    let Some(range) = d.range.as_ref() else {
        return one_char;
    };
    let end = lang::line_col(source, range.end);

    if end.line == start.line && end.col > start.col {
        end
    } else {
        one_char
    }
}

/// 引擎的 `RasterPage` → GPUI 的 GPU 纹理。
///
/// 这里**同步**构造 `RenderImage`，刻意不走 `Image::from_bytes` 那条路 ——
/// 后者要过 asset 系统做异步解码（`use_asset`），第一次必然拿不到结果。
/// 而且 `Image::from_bytes` 的缓存键是内容哈希，同一份内容永远复用同一张
/// 纹理，缩放就拿不到更高分辨率。自己构造就没有这两个问题。
fn to_texture(page: RasterPage) -> Option<Arc<RenderImage>> {
    let (width, height) = (page.width, page.height);
    let mut rgba = page.rgba;

    // tiny-skia 给的是 RGBA（alpha 已预乘），GPU 纹理要 BGRA。
    for pixel in rgba.as_chunks_mut::<4>().0 {
        swap_rgba_pa_to_bgra(pixel);
    }

    let buffer: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_raw(width, height, rgba)?;

    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// 快速打开的浮层。
impl Previewer {
    fn render_quick_open(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let rows: Vec<(String, usize)> = self
            .quick_matches
            .iter()
            .enumerate()
            .map(|(i, p)| (p.to_string_lossy().replace('\\', "/"), i))
            .collect();
        let total = self.quick_matches.len();

        let list = if rows.is_empty() {
            div()
                .px_3()
                .py_5()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.files.is_empty() {
                    "目录里没找到文件"
                } else {
                    "没有匹配的文件"
                })
                .into_any_element()
        } else {
            v_flex()
                .id("quick-list")
                .max_h(px(420.))
                .overflow_y_scroll()
                .children(rows.into_iter().map(|(label, i)| {
                    let selected = i == self.quick_selected;
                    let mut row = div().px_3().py_1().text_sm().cursor_pointer();
                    if selected {
                        row = row.bg(theme.accent.opacity(0.22));
                    }
                    row.hover(|s| s.bg(theme.accent.opacity(0.12)))
                        .child(label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                this.quick_selected = i;
                                this.confirm_quick(window, cx);
                            }),
                        )
                }))
                .into_any_element()
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(90.))
            .bg(gpui::black().opacity(0.25))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.cancel_quick(cx)),
            )
            .child(
                v_flex()
                    // 让 ↑↓ / Esc 的绑定只在这个子树里生效 ——
                    // 否则会跟编辑器自己的方向键打架。
                    .key_context("QuickOpen")
                    .w(px(640.))
                    .max_h(px(540.))
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .shadow_lg()
                    .overflow_hidden()
                    // 浮层内部的点击不该冒泡到外层的「点空白关闭」
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&self.quick_input).w_full())
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .border_t_1()
                            .border_color(theme.border)
                            .child(format!("{} / {} 个文件匹配", total, self.files.len())),
                    )
                    .child(list),
            )
    }
}

impl Render for Previewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // AI 评审阶段把焦点拿到浮层上：gpui 的动作派发**从聚焦节点开始**，
        // 没有焦点就没有起点（NEXT.md 里那个坑：所有快捷键静默失效）。
        if self.ai.stage == AiStage::Review && !self.ai_focus.is_focused(window) {
            self.ai_focus.focus(window, cx);
        }

        // 首次排版**推迟到这一帧画完之后**再跑：先让窗口出现（带一句
        // 「首次排版中…」），别让用户盯着一个还没画出来的窗口等 200+ ms。
        if self.first_compile {
            self.first_compile = false;
            println!("[typst-live] 首帧 @{:.0} ms（窗口已画）", since_start_ms());
            cx.spawn(async move |this, cx| {
                // 让出一轮执行器，确保这一帧真的画出去了再排
                cx.background_executor().timer(Duration::ZERO).await;
                _ = this.update(cx, |this, cx| {
                    println!("[typst-live] 首次排版 @{:.0} ms", since_start_ms());
                    this.recompile(cx);
                    println!(
                        "[typst-live] 首次排版完成：{:.1} ms（{} 页）@{:.0} ms",
                        this.status.compile_ms,
                        this.status.pages,
                        since_start_ms()
                    );
                });
            })
            .detach();
        }

        // 窗口尺寸/位置变了就记下来（拖动时会变很多次，所以写盘是防抖的）。
        //
        // ★ 用 `window_bounds()` 而不是 `bounds()`：前者就是「下次开窗该用哪份
        // 几何」，与我们交给 `WindowOptions` 的是同一个坐标系；`bounds()` 报的是
        // **客户区**，比请求值差一个非客户区高度（本机实测 **+11px**）——
        // 拿它存下来，窗口每开一次就在屏幕上往下爬 11px。
        if let WindowBounds::Windowed(rect) = window.window_bounds() {
            let boxed = (
                rect.origin.x.as_f32() as i32,
                rect.origin.y.as_f32() as i32,
                rect.size.width.as_f32() as i32,
                rect.size.height.as_f32() as i32,
            );
            if self.pending_window != Some(boxed) {
                self.pending_window = Some(boxed);
                self.touch_settings(cx);
            }
        }

        // 下面几项都要 `&mut cx`，而 `cx.theme()` 是不可变借用 ——
        // 所以它们必须放在拿 theme 之前（这个坑在 NEXT.md 里记着）。
        //
        // 按当前视口补出/卸载纹理。内部只在可见范围真的变了才动手，
        // 所以滚动过程中每帧调它是安全的（就是两次二分查找）。
        self.sync_visible_pages();

        // 双击编辑区后的前向跳转。放在 render 里而不是鼠标回调里，
        // 是因为此刻编辑器已经把光标放到点击处了 —— 读到的位置才准。
        if self.pending_forward {
            self.pending_forward = false;
            self.jump_to_preview(cx);
        }

        let theme = cx.theme();
        let has_errors = !self.compile_errors.is_empty();

        let sidebar_pane = self.render_sidebar(cx);

        // 工具栏：一键插入，分组画分隔条 —— 条目与分组都对着 `wu` 的工具栏。
        let toolbar = {
            let mut bar = h_flex()
                .id("toolbar")
                .w_full()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                // 按钮多，窄窗口里横向滚，别把按钮挤没
                .overflow_x_scroll();

            for (index, group) in Markup::GROUPS.iter().enumerate() {
                if index > 0 {
                    bar = bar.child(div().w(px(1.)).h(px(16.)).flex_shrink_0().bg(theme.border));
                }

                match index {
                    // 颜色：九个颜色的下拉（wu 也是下拉）
                    5 => {
                        let view = cx.entity();
                        bar = bar.child(
                            Button::new("tb-color")
                                .ghost()
                                .compact()
                                .label("颜色")
                                .tooltip("包住选中：换这个颜色")
                                .dropdown_menu(move |menu, window, _cx| {
                                    let view = view.clone();
                                    let mut menu = menu.min_w(120.);
                                    for name in markup::COLORS {
                                        menu = menu.item(
                                            PopupMenuItem::new(color_label(name)).on_click(
                                                window.listener_for(
                                                    &view,
                                                    move |this, _, window, cx| {
                                                        this.apply_markup(
                                                            Markup::TextColor(name),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            ),
                                        );
                                    }
                                    menu
                                }),
                        );
                    }

                    // AI：与顶部「AI」菜单同一套动作，这里再给一个入口
                    6 => {
                        let view = cx.entity();
                        bar = bar.child(
                            Button::new("tb-ai")
                                .ghost()
                                .compact()
                                .label("AI")
                                .dropdown_menu(move |menu, window, _cx| {
                                    let view = view.clone();
                                    let mut menu = menu.min_w(200.);
                                    menu = menu.item(
                                        PopupMenuItem::new("AI 编辑…（Ctrl+K）").on_click(
                                            window.listener_for(&view, |this, _, window, cx| {
                                                this.open_ai(window, cx)
                                            }),
                                        ),
                                    );
                                    menu = menu.separator();
                                    for task in [
                                        AiTask::SyntaxFix,
                                        AiTask::Proofread,
                                        AiTask::Terminology,
                                        AiTask::TranslateToEnglish,
                                        AiTask::TranslateToChinese,
                                    ] {
                                        menu =
                                            menu.item(PopupMenuItem::new(task.label()).on_click(
                                                window.listener_for(
                                                    &view,
                                                    move |this, _, window, cx| {
                                                        this.run_ai_task(task, window, cx)
                                                    },
                                                ),
                                            ));
                                    }
                                    menu
                                }),
                        );
                    }

                    _ => {
                        for kind in group.iter().copied() {
                            bar = bar.child(
                                // id 加前缀：工具栏的「图片」与右侧页签的「图片」会撞名
                                Button::new(format!("tb:{}", kind.label()))
                                    .ghost()
                                    .compact()
                                    .label(kind.label())
                                    .tooltip(kind.hint())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.apply_markup(kind, window, cx)
                                    })),
                            );
                        }
                    }
                }
            }

            bar
        };

        let editor_pane = v_flex()
            .w(px(520.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            // 双击编辑区 → 显示区。
            //
            // **不阻止事件**：编辑器自己的「双击选词」照常发生，
            // 两件事互不干扰（选中的词也会一并被前向跳转命中）。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    if ev.click_count < 2 {
                        return;
                    }
                    // 不当场跳：等这一轮事件跑完，编辑器把光标挪到点击处了，
                    // 那时读到的光标位置才是准的（在 render 里读）。
                    this.pending_forward = true;
                    cx.notify();
                }),
            )
            .child(toolbar)
            .child(Input::new(&self.editor).flex_1());

        // 布局尺寸一律用 `page_sizes`（按 pt 算出来的），**不用纹理的像素尺寸**。
        // 两者能对上（纹理 = pt × ppp，逻辑 = 纹理 / 屏缩放 = pt × 96/72 × zoom），
        // 但用前者才能保证「某一页还没出图」时页面尺寸不变 —— 否则
        // 滚动条会在滚到新页的瞬间跳一下。
        let page_rows: Vec<(f32, f32, Option<Arc<RenderImage>>)> = (0..self.bitmaps.len())
            .map(|i| {
                let (w, h) = self.page_sizes.get(i).copied().unwrap_or((0.0, 0.0));
                (w, h, self.bitmaps[i].clone())
            })
            .collect();

        // 高亮与缩放系数先收成自有数据，免得在子元素闭包里借用 `self`。
        let base = pixel_per_pt_for_zoom(self.zoom);
        let flash = self.flash.as_ref().map(|f| (f.page, f.rect, f.seq));

        let pages = v_flex()
            .id("pages")
            .flex_1()
            .h_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .bg(theme.secondary)
            .gap_5()
            .py_5()
            .items_center()
            .children(
                page_rows
                    .into_iter()
                    .enumerate()
                    .map(|(i, (w, h, texture))| {
                        let inner = match texture {
                            // 纹理是**物理像素**，而布局用的是**逻辑像素** —— 除回缩放系数
                            // 就是 1:1 映射，一个物理像素对一个物理像素，字才不发虚。
                            Some(texture) => img(ImageSource::Render(texture))
                                .w(px(w))
                                .h(px(h))
                                .into_any_element(),
                            // 占位：尺寸一样，所以不牵动布局；只是还没出图
                            None => div()
                                .w(px(w))
                                .h(px(h))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(gpui::black().opacity(0.18))
                                .child(format!("第 {} 页", i + 1))
                                .into_any_element(),
                        };
                        div()
                            // 高亮要绝对定位在页内，所以页框得是定位上下文
                            .relative()
                            .bg(gpui::white())
                            .shadow_md()
                            .overflow_hidden()
                            .w(px(w))
                            .h(px(h))
                            // ★ 必须禁掉收缩：flex 列容器默认 `flex-shrink: 1`，
                            // 会把超出视口高度的页全压扁塞进来 —— 结果是
                            // **容器永远不会溢出，滚轮因此没有任何东西可滚**，
                            // 而且视口光栅化会误以为「全部页都可见」而把纹理全出出来。
                            .flex_shrink_0()
                            // 双击这一页 → 光标跳到对应的源码处；
                            // Ctrl+单击 → 打开链接
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                    let Some((x, y)) = this.window_to_page_pt(ev.position, i)
                                    else {
                                        return;
                                    };
                                    if ev.click_count == 1 && ev.modifiers.control {
                                        this.open_link_at(i, x, y, cx);
                                        return;
                                    }
                                    if ev.click_count < 2 {
                                        return;
                                    }
                                    this.jump_to_source(i, x, y, window, cx);
                                }),
                            )
                            .child(inner)
                            .children(flash.and_then(|(page, rect, seq)| {
                                (page == i).then(|| {
                                    div()
                                        .absolute()
                                        .left(px(rect[0] * base))
                                        .top(px(rect[1] * base))
                                        .w(px((rect[2] - rect[0]).max(1.0) * base))
                                        .h(px((rect[3] - rect[1]).max(1.0) * base))
                                        .bg(theme.primary.opacity(0.28))
                                        // 一次性反馈，所以淡出。序号当 id：换了序号
                                        // 才是新元素、才会重新播一遍。
                                        .with_animation(
                                            ("jump-flash", seq),
                                            Animation::new(FLASH),
                                            |el, delta| el.opacity(1.0 - delta),
                                        )
                                        .into_any_element()
                                })
                            }))
                    }),
            );

        let preview_pane = if self.doc.is_none() {
            // 还没有排版结果（首次排版还在路上）—— 别给一片空白，
            // 让人知道它在干活
            v_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .bg(theme.secondary)
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("首次排版中…（大文档冷编译要几百毫秒）")
                .into_any_element()
        } else if has_errors {
            v_flex()
                .flex_1()
                .h_full()
                .child(pages)
                .child(self.render_errors(cx))
                .into_any_element()
        } else {
            pages.into_any_element()
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_menu(cx))
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(sidebar_pane)
                    .child(editor_pane)
                    .child(self.render_right_pane(preview_pane, cx)),
            )
            .children(self.shell_visible.then(|| self.render_shell(cx)))
            // 状态栏贴窗口最底：上面是内容，最下面一条是状态
            .child(self.render_statusbar(cx))
            // 浮层放在最后，才能盖在内容之上
            .children(self.quick_visible.then(|| self.render_quick_open(cx)))
            .children((self.ai.stage != AiStage::Closed).then(|| self.render_ai(cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _window, cx| {
                let next = this.zoom * ZOOM_STEP;
                this.set_zoom(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _window, cx| {
                let next = this.zoom / ZOOM_STEP;
                this.set_zoom(next, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, _window, cx| {
                this.set_zoom(1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &SaveFile, _window, cx| {
                this.save_file(cx);
            }))
            .on_action(cx.listener(|this, _: &RecompileNow, _window, cx| {
                this.recompile_now(cx);
            }))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| {
                this.format_document(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExportPdf, _window, cx| {
                this.export_pdf(cx);
            }))
            .on_action(cx.listener(|this, _: &NextPage, _window, cx| {
                this.go_to_page(this.current_page + 1, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevPage, _window, cx| {
                let prev = this.current_page.saturating_sub(1);
                this.go_to_page(prev, cx);
            }))
            .on_action(cx.listener(|this, _: &FirstPage, _window, cx| {
                this.go_to_page(0, cx);
            }))
            .on_action(cx.listener(|this, _: &LastPage, _window, cx| {
                let last = this.bitmaps.len().saturating_sub(1);
                this.go_to_page(last, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncToPreview, _window, cx| {
                this.jump_to_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &AiEditOpen, window, cx| {
                this.open_ai(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AiSubmit, window, cx| {
                if this.ai.stage == AiStage::Review {
                    this.apply_ai(window, cx);
                } else {
                    this.start_ai(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &AiCancel, _window, cx| {
                this.cancel_ai(cx);
            }))
            .on_action(cx.listener(|this, _: &AiNextHunk, _window, cx| {
                this.ai_move(1, cx);
            }))
            .on_action(cx.listener(|this, _: &AiPrevHunk, _window, cx| {
                this.ai_move(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &AiToggleHunk, _window, cx| {
                this.ai_toggle_hunk(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleShell, window, cx| {
                this.shell_visible = !this.shell_visible;
                // 显示时把焦点交给终端，否则得先点一下才能打字
                if this.shell_visible {
                    this.terminal_focus.focus(window, cx);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.show_quick_open(window, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickPrev, _window, cx| {
                this.quick_move(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickNext, _window, cx| {
                this.quick_move(1, cx);
            }))
            .on_action(cx.listener(|this, _: &QuickCancel, _window, cx| {
                this.cancel_quick(cx);
            }))
            // Ctrl + 滚轮缩放。不阻止容器自身的滚动（gpui 没有 preventDefault），
            // 所以缩完之后把当前页重新锚回顶部 —— 一举两得：既抵消了误滚，
            // 又让“缩放不跳位置”成为确定行为。
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                if !ev.modifiers.control {
                    return;
                }
                let dy = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => p.y.as_f32() / 40.0,
                };
                if dy == 0.0 {
                    return;
                }
                let next = if dy > 0.0 {
                    this.zoom * ZOOM_STEP
                } else {
                    this.zoom / ZOOM_STEP
                };
                this.set_zoom_anchored(next, cx);
            }))
    }
}

/// 颜色名 → 菜单上的中文（与 `wu` 的叫法一致）。
fn color_label(name: &str) -> &'static str {
    match name {
        "red" => "红",
        "orange" => "橙",
        "yellow" => "黄",
        "green" => "绿",
        "aqua" => "青",
        "blue" => "蓝",
        "purple" => "紫",
        "gray" => "灰",
        "black" => "黑",
        _ => "颜色",
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

/// 主题那行日志的正文。
///
/// 带上背景色的 **hsl 数字**：光看「切成了深色」是自我报告，
/// 数字才能证明真的换了（浅色主题的 l 接近 1，深色接近 0）。
fn describe_theme(dark: bool, cx: &App) -> String {
    let bg = cx.theme().background;
    format!(
        "{}（背景 hsl {:.3} {:.3} {:.3}）",
        if dark { "深色" } else { "浅色" },
        bg.h,
        bg.s,
        bg.l
    )
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
                println!("打开 {}", path.display());
                return (text, path, true);
            }
            Err(err) => eprintln!("读不了 {}：{err}", path.display()),
        }
    }

    // 示例文档落在临时目录。注意：磁盘上存不存在都无所谓 ——
    // 引擎用的是内存覆盖层，这正是实时编译的前提。
    let path = std::env::temp_dir().join("typst-live-demo.typ");
    println!("使用内置示例文档（虚拟路径 {}）", path.display());
    (DEMO_DOC.to_owned(), path, false)
}

fn main() {
    let path_arg = std::env::args().nth(1);

    // 设置先读：窗口开在哪儿、上次开的是哪个文件、上次缩放多少，都在这儿。
    let settings = Settings::load();
    println!(
        "[typst-live] 读设置 {}：窗口 {:?}，缩放 {:?}，上次文件 {:?}",
        settings::path().display(),
        settings.window,
        settings.zoom,
        settings.file,
    );
    match Packages::with_downloads().cache_dir() {
        Some(dir) => println!(
            "[typst-live] 包源：本地目录 + 官方源（下载缓存 {}）",
            dir.display()
        ),
        None => println!("[typst-live] 包源：只认本地目录（系统缓存目录不可用）"),
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
            // AI 编辑。Enter/Tab/空格/Esc 只在浮层内生效（按键上下文限定），
            // 否则会把编辑器自己的按键抢走。
            KeyBinding::new("ctrl-k", AiEditOpen, None),
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
            KeyBinding::new("ctrl-p", QuickOpen, None),
            KeyBinding::new("up", QuickPrev, Some("QuickOpen")),
            KeyBinding::new("down", QuickNext, Some("QuickOpen")),
            KeyBinding::new("escape", QuickCancel, Some("QuickOpen")),
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
            cx.open_window(options, move |window, cx| {
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
                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("打开窗口失败");
        })
        .detach();
    });
}
