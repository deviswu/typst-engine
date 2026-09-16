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
//! 状态栏上那四个数字就是「实时」的证据：
//! - 编译耗时（引擎真正花在排版上的时间）
//! - 重解析字节 / 全文字节（增量到底省了多少）
//! - 页数
//! - 第几次编译（敲一次键加一）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui::*;
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Root, h_flex, v_flex};

use typst::foundations::Bytes;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

/// 预览里一页的显示宽度（点）。SVG 是矢量的，缩放不会糊。
const PAGE_WIDTH: f32 = 640.0;

const DEMO_DOC: &str = r#"#set page(width: 15cm, height: auto, margin: 1.8cm)
#set text(size: 11pt)

= 实时编译演示

在**左边随便改点什么**，右边会在几毫秒内跟着变。

没有子进程，没有存盘，没有 IPC —— 编译器直接读内存里未保存的文本。

== 为什么能这么快

+ 覆盖式虚拟文件系统：编译器看到的就是你正在敲的内容
+ `Source::replace` 的增量重解析：敲一个字只重解析几十字节
+ comemo 记忆化排版：没变的部分不重算

看状态栏：*编译耗时* 与 *重解析字节*。

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
    compile_ms: f64,
    reparsed: usize,
    text_bytes: usize,
    pages: usize,
    compiles: usize,
    errors: Vec<String>,
}

struct Previewer {
    engine: EngineWorld,
    main_path: PathBuf,
    editor: Entity<InputState>,
    /// 最近一次成功编译的页面（SVG 源码）。
    svg_pages: Vec<String>,
    /// 转成 GPUI 能画的位图缓存。
    bitmaps: Vec<Arc<RenderImage>>,
    /// 是否已经把「解码完成」播报过一次（只用于控制台日志）。
    rasterize_reported: bool,
    status: Status,
    _sub: Subscription,
}

impl Previewer {
    fn new(
        source: String,
        main_path: PathBuf,
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
        );
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
                this.recompile(cx);
            }
        });

        let mut this = Self {
            engine,
            main_path,
            editor,
            svg_pages: Vec::new(),
            bitmaps: Vec::new(),
            rasterize_reported: false,
            status: Status::default(),
            _sub: sub,
        };
        this.recompile(cx);
        this
    }

    /// 一次完整的「编辑 → 重排版」循环。同步执行 —— 因为它只有几毫秒，
    /// 开线程反而会引入调度开销和状态同步麻烦。
    fn recompile(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let main_id = self.engine.entry().main();

        // ① 未保存文本进覆盖层
        self.engine
            .vfs_mut()
            .map_shadow(&self.main_path, Bytes::from_string(text.clone()));
        // ② 增量重解析（不是重建语法树）
        let outcome = self.engine.sources().feed_memory(main_id, &text);

        // ③ 排版
        let t = Instant::now();
        let result = typst::compile::<PagedDocument>(&self.engine);
        let elapsed = t.elapsed();

        self.status.compile_ms = elapsed.as_secs_f64() * 1000.0;
        self.status.reparsed = outcome.reparsed.map(|r| r.len()).unwrap_or(0);
        self.status.text_bytes = text.len();
        self.status.compiles += 1;

        match result.output {
            Ok(doc) => {
                self.status.pages = doc.pages().len();
                self.svg_pages = typst_engine::export::page_svgs(&doc);
                self.status.errors.clear();
            }
            Err(errors) => {
                // 编译失败：**不动 svg_pages**，右边继续显示上一次成功的结果。
                self.status.errors = errors
                    .iter()
                    .take(6)
                    .map(|e| e.message.to_string())
                    .collect();
            }
        }

        cx.notify();
    }

    fn render_statusbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let mut bar = h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_5()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                div()
                    .text_color(theme.primary)
                    .child(format!("⏱ {:.1} ms", self.status.compile_ms)),
            )
            .child(format!(
                "重解析 {} B / 全文 {} B",
                self.status.reparsed, self.status.text_bytes
            ))
            .child(format!("{} 页", self.status.pages))
            .child(format!("第 {} 次编译", self.status.compiles));

        if self.status.errors.is_empty() {
            bar = bar.child(div().text_color(theme.success).child("✓ 无错误"));
        } else {
            bar = bar.child(div().text_color(theme.danger).child(format!(
                "✗ {} 个错误（预览保留上次成功结果）",
                self.status.errors.len()
            )));
        }
        bar
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
            .children(self.status.errors.iter().map(|e| div().child(e.clone())))
    }
}

impl Render for Previewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // gpui 的图片解码是**异步**的（`ImageSource::Image` 走 `use_asset`）：
        // 第一次问必然拿不到，解码完成后 gpui 会自己 notify 让我们重画。
        // 所以这里每帧都问一遗，而不是「问一次没拿到就永久放弃」。
        // `Image::from_bytes` 的 id 是内容哈希，同一份 SVG 反复问不会重复解码。
        let decoded: Vec<Arc<RenderImage>> = self
            .svg_pages
            .iter()
            .filter_map(|svg| {
                let image = Image::from_bytes(ImageFormat::Svg, svg.clone().into_bytes());
                Arc::new(image).use_render_image(window, cx)
            })
            .collect();

        if decoded.len() == self.svg_pages.len() {
            if !decoded.is_empty() && !self.rasterize_reported {
                eprintln!(
                    "[typst-live] 已解码 {} 页（每页 {} KiB SVG）",
                    decoded.len(),
                    self.svg_pages.iter().map(|s| s.len()).sum::<usize>()
                        / decoded.len().max(1)
                        / 1024
                );
                self.rasterize_reported = true;
            }
            self.bitmaps = decoded;
        }

        let theme = cx.theme();
        let has_errors = !self.status.errors.is_empty();

        let editor_pane = div()
            .w(px(520.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(Input::new(&self.editor).h_full());

        let pages = v_flex()
            .id("pages")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .bg(theme.secondary)
            .gap_5()
            .py_5()
            .items_center()
            .children(self.bitmaps.iter().map(|bitmap| {
                div()
                    .bg(gpui::white())
                    .rounded_sm()
                    .overflow_hidden()
                    .child(img(ImageSource::Render(bitmap.clone())).w(px(PAGE_WIDTH)))
            }));

        let preview_pane = if has_errors {
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
            .child(self.render_statusbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(editor_pane)
                    .child(preview_pane),
            )
    }
}

/// 载入要编辑的文档：有参数就用它，否则用内置示例。
fn load_document(arg: Option<String>) -> (String, PathBuf) {
    if let Some(raw) = arg {
        let path = PathBuf::from(&raw);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                println!("打开 {}", path.display());
                return (text, path);
            }
            Err(err) => {
                eprintln!("读不了 {raw}：{err}；改用内置示例文档");
            }
        }
    }

    // 示例文档落在临时目录。注意：磁盘上存不存在都无所谓 ——
    // 引擎用的是内存覆盖层，这正是实时编译的前提。
    let path = std::env::temp_dir().join("typst-live-demo.typ");
    println!("使用内置示例文档（虚拟路径 {}）", path.display());
    (DEMO_DOC.to_owned(), path)
}

fn main() {
    let path_arg = std::env::args().nth(1);

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        // 用 gpui-component 的任何东西之前必须先 init。
        gpui_component::init(cx);
        register_typst_language();

        let (source, path) = load_document(path_arg);

        cx.spawn(async move |cx| {
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("typst-engine · 实时预览".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(80.), px(60.)),
                    Size {
                        width: px(1320.),
                        height: px(880.),
                    },
                ))),
                window_min_size: Some(Size {
                    width: px(900.),
                    height: px(600.),
                }),
                ..Default::default()
            };

            let source = source.clone();
            let path = path.clone();
            cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| Previewer::new(source.clone(), path.clone(), window, cx));
                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("打开窗口失败");
        })
        .detach();
    });
}
