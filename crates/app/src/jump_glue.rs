//! 源码 ⇄ 预览的**双向跳转胶水**：建索引、算落点、滚过去、画一下高亮。
//!
//! 纯算法在 `typst_engine::jump`（UI 无关、能单测）；这里只负责「外壳怎么用」。

use crate::*;

impl Previewer {
    /// 按需建索引，并按**排版结果**缓存。
    ///
    /// 「排版 → 索引 → 光栅化」三层里中间那层的全部实现就在这里：
    /// 排版结果没换（`Arc` 指针相同）就直接复用，所以**缩放、滚动、
    /// 翻页都不会重建它**。状态栏的「索引 N 次」会把这件事直接显示出来。
    pub(crate) fn rebuild_index(&mut self) {
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

        logln!(
            "[typst-live] 建跳转索引：{} 页 / {} 个字形，用时 {:.1} ms（第 {} 次）",
            index.page_count(),
            index.glyph_count(),
            self.index_ms,
            self.index_builds,
        );
        self.index = Some(IndexCache { doc, index });
    }
    /// 源码第 `byte` 个字节 → 显示区的位置。
    pub(crate) fn forward_anchor(&mut self, byte: usize) -> Option<Anchor> {
        self.rebuild_index();
        self.index.as_ref()?.index.forward(byte)
    }
    /// 显示区第 `page` 页的 `(x, y)` pt → 源码的位置。
    pub(crate) fn inverse_anchor(&mut self, page: usize, x: f32, y: f32) -> Option<Anchor> {
        self.rebuild_index();
        self.index.as_ref()?.index.inverse(page, x, y)
    }
    /// 显示区 → 编辑区：把光标放到那一处。
    ///
    /// `set_cursor_position` 内部会 focus 并滚到光标上，所以不需要
    /// 我们管编辑器的滚动。
    pub(crate) fn jump_to_source(
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

        logln!(
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
    /// 手动前向跳转（Ctrl+Alt+J、双击编辑区）：会提示、会画高亮。
    pub(crate) fn jump_to_preview(&mut self, cx: &mut Context<Self>) {
        self.jump_to_preview_impl(true, cx);
    }

    /// 跟随光标用：**只滚过去**，不提示、不画高亮、不写日志。
    ///
    /// 每 400ms 弹一条状态栏消息 + 闪一次高亮，人是没法工作的 ——
    /// 「跟随」要的是安静地跟上去。
    pub(crate) fn follow_cursor_now(&mut self, cx: &mut Context<Self>) {
        self.jump_to_preview_impl(false, cx);
    }

    fn jump_to_preview_impl(&mut self, loud: bool, cx: &mut Context<Self>) {
        let byte = self.editor.read(cx).cursor();
        let Some(anchor) = self.forward_anchor(byte) else {
            if loud {
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
            }
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

        if loud {
            let rope = self.editor.read(cx).text().clone();
            let line = rope.offset_to_position(anchor.byte.start).line + 1;
            logln!(
                "[typst-live] 前向跳转：第 {line} 行（字节 {byte}）→ 第 {} 页，页内 y={:.1} pt",
                anchor.page + 1,
                anchor.rect[1],
            );
            self.flash_on(anchor.page, anchor.rect, cx);
            self.message = Some(format!("第 {line} 行 → 第 {} 页", anchor.page + 1));
        }
        cx.notify();
    }
    /// 窗口坐标 → 页内 pt。
    ///
    /// 算术在 [`coords`] 里（纯函数 + 单测）。这里只负责把 gpui 的类型拆开。
    pub(crate) fn window_to_page_pt(
        &self,
        position: Point<Pixels>,
        page: usize,
    ) -> Option<(f32, f32)> {
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
    pub(crate) fn page_pt_to_window(&self, page: usize, x: f32, y: f32) -> Option<Point<Pixels>> {
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
    pub(crate) fn open_link_at(&mut self, page: usize, x: f32, y: f32, cx: &mut Context<Self>) {
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
                logln!("[typst-live] 打开链接：{url}");
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
    pub(crate) fn flash_on(&mut self, page: usize, rect: [f32; 4], cx: &mut Context<Self>) {
        self.flash_seq += 1;
        self.flash = Some(Flash {
            page,
            rect,
            seq: self.flash_seq,
        });
        cx.notify();
    }
}
