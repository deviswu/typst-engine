//! 端到端验证：真排版一次 → 建跳转索引 → 两个方向都对得上。
//!
//! 单测（`jump.rs` 里的）只覆盖纯函数（仿射、字符对齐、空索引）。
//! 「字形 span 真的能映射回源码」「点回字形中心真的回到同一个字」
//! 这两件事只有**真编译一次**才能确认 —— 所以这里跑真 World。

use std::sync::Arc;
use std::time::Instant;

use typst::syntax::Source;
use typst_engine::jump::LayoutIndex;
use typst_engine::syntax as lang;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

/// 排一次版，拿回（源码, 排版结果）。
fn layout(text: &str) -> (Source, Arc<PagedDocument>) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.typ");
    std::fs::write(&main, text).unwrap();

    let mut world = EngineWorld::new(
        embedded_and_system_fonts(),
        EntryState::new(dir.path(), &main),
    );
    let outcome = world.compile();
    assert!(outcome.fresh, "样例文档该能编译：{:?}", outcome.errors);
    let source = typst::World::source(&world, world.entry().main()).unwrap();
    (source, outcome.doc.expect("有排版结果"))
}

/// 每一段都独占一页的文档：页号与段落一一对应，断言才好写。
fn paged_document(paragraphs: usize) -> String {
    let mut s = String::from("= 跳转测试\n\n");
    for i in 0..paragraphs {
        s.push_str(&format!("这是第 {i} 段的正文内容。\n\n"));
        if i + 1 < paragraphs {
            s.push_str("#pagebreak()\n\n");
        }
    }
    s
}

/// 源码里所有「是字符开头」的字节偏移（跳过空行与纯空白）。
fn char_starts(source: &Source) -> Vec<usize> {
    let text = source.text();
    (0..text.len())
        .filter(|&i| text.is_char_boundary(i))
        .filter(|&i| !text[i..].starts_with('\n') && !text[i..].starts_with('\r'))
        .collect()
}

#[test]
fn a_heading_maps_to_the_top_of_the_first_page() {
    let (source, doc) = layout("= 标题\n\n正文正文正文。\n");
    let index = LayoutIndex::build(&doc, &source);

    let of_heading = source.text().find('标').unwrap();
    let anchor = index.forward(of_heading).expect("标题该有字形");

    assert_eq!(anchor.page, 0);
    assert_eq!(
        anchor.byte,
        of_heading..of_heading + '标'.len_utf8(),
        "命中该是这个字本身"
    );

    let [x0, y0, x1, y1] = anchor.rect;
    let page = doc.pages()[0].frame.size();
    assert!(x0 >= 0.0 && y0 >= 0.0, "页内坐标不该是负的：{anchor:?}");
    assert!(
        x1 <= page.x.to_pt() as f32 && y1 <= page.y.to_pt() as f32,
        "不该跑出页面：{anchor:?} 页面 {page:?}"
    );
    assert!(x1 > x0 && y1 > y0, "字形框该有面积：{anchor:?}");
    assert!(y0 < 100.0, "标题该在页面上部，实际 y0={y0}");
}

/// ★ 整条链路的根据：**往返闭环**。
///
/// 对正文里抽样到的字符：前向找到它的位置，再把「位置中心」喂回反向，
/// 必须回到**同一个字**。
///
/// 这条同时守住了好几件事：字节对齐、y 轴方向（文字空间 y 向上 /
/// 帧空间 y 向下）、字形推进量、页内坐标到页面坐标的变换。
///
/// 只抽样正文（不是标记）：`#pagebreak()`、`=` 这些字节本来就没有字形，
/// 那种情况走的是「就近吸附」，有另一条测试守。
#[test]
fn forward_then_inverse_comes_back_to_the_same_character() {
    let bodies = [
        "第一段正文，用来验证前向与反向能不能闭环。",
        "第二段正文，字数不多但足以跨到第二页上去。",
        "第三段正文，中文的字形与拉丁字母的推进量不同。",
        "第四段正文，混一点 latin words 和 12345 数字。",
        "第五段正文，这里要凑足够多的字，好让抽样多起来。",
        "第六段正文，最后一段了，到这里就该收尾。",
    ];

    let mut text = String::from("= 跳转测试\n\n");
    for (i, body) in bodies.iter().enumerate() {
        text.push_str(body);
        text.push_str("\n\n");
        if i + 1 < bodies.len() {
            text.push_str("#pagebreak()\n\n");
        }
    }

    let (source, doc) = layout(&text);
    let index = LayoutIndex::build(&doc, &source);

    // 每段正文在源码里的字节范围
    let mut runs = Vec::new();
    for body in bodies {
        let start = source.text().find(body).expect("正文该在源码里");
        runs.push(start..start + body.len());
    }

    let mut checked = 0;
    for run in &runs {
        // 每 2 个字符抽一个
        let mut byte = run.start;
        while byte < run.end {
            let ch = source.text()[byte..].chars().next().unwrap();
            let Some(anchor) = index.forward(byte) else {
                panic!("正文里的字节 {byte} 该有字形");
            };
            let (cx, cy) = anchor.center();
            let back = index
                .inverse(anchor.page, cx, cy)
                .unwrap_or_else(|| panic!("位置中心的点该能反查：{anchor:?}"));

            assert_eq!(
                back.byte,
                byte..byte + ch.len_utf8(),
                "字节 {byte}（{ch:?}）前向到 {anchor:?}，反查却回了 {back:?}"
            );
            assert_eq!(back.page, anchor.page, "页号也不该变");
            checked += 1;
            byte += ch.len_utf8() * 2;
        }
    }

    assert!(checked > 40, "抽样太少（{checked} 个），测试是空的");
}

#[test]
fn clicking_a_character_finds_that_character() {
    let (source, doc) = layout("第一段有一个特别的词：钻头。\n");
    let index = LayoutIndex::build(&doc, &source);

    let needle = source.text().find('钻').unwrap();
    let anchor = index.forward(needle).expect("该有字形");
    let (cx, cy) = anchor.center();

    let back = index.inverse(anchor.page, cx, cy).expect("该命中");
    assert_eq!(back.byte, needle..needle + '钻'.len_utf8());
}

/// 点在页边距上（页面上没有任何字形的地方）必须就近吸附到本页文字，
/// 而不是给 `None` —— 否则「点哪儿都能跳」就是假的。
#[test]
fn clicking_the_margin_snaps_to_the_nearest_text_on_that_page() {
    let (source, doc) = layout("= 标题\n\n正文在这一页上。\n");
    let index = LayoutIndex::build(&doc, &source);

    let body = source.text().find('正').unwrap();
    let anchor = index.forward(body).expect("该有字形");

    // 页面左上角（正文左边距之外、且高于正文）
    let back = index
        .inverse(anchor.page, 1.0, anchor.rect[1])
        .expect("页边距该吸附到最近的字");

    assert_eq!(
        lang::line_col(&source, back.byte.start).line,
        lang::line_col(&source, body).line,
        "吸附该落在同一行上"
    );
}

/// 源码里没有字形的部分（`#set`、注释）不能 panic，也不能乱跳。
#[test]
fn source_without_glyphs_degrades_to_the_nearest_glyph() {
    let text = "#set page(width: 10cm)\n\n// 这行是注释\n\n正文。\n";
    let (source, doc) = layout(text);
    let index = LayoutIndex::build(&doc, &source);

    let set_line = source.text().find("#set").unwrap();
    let anchor = index.forward(set_line).expect("该就近吸附，不该给 None");
    let hit = lang::line_col(&source, anchor.byte.start);
    assert!(
        hit.line > 0,
        "`#set` 那一行没有字形，该吸附到别处，实际 {hit:?}"
    );

    let comment = source.text().find("这行是注释").unwrap();
    let anchor = index.forward(comment).expect("注释也要能吸附");
    assert!(
        !(comment..comment + "这行是注释".len()).contains(&anchor.byte.start),
        "注释没有字形，不该命中注释自己：{anchor:?}"
    );
}

/// 未渲染的代码（`#let` 定义）在排版结果里没有字形 ——
/// 索引来自**排版**，不是来自源码本身。
#[test]
fn the_index_comes_from_layout_not_from_the_text_alone() {
    let text = "#let hidden = \"从未显示\"\n\n正文。\n";
    let (source, doc) = layout(text);
    let index = LayoutIndex::build(&doc, &source);

    let hidden = source.text().find("从未显示").unwrap();
    let anchor = index.forward(hidden).expect("该就近吸附");

    assert_ne!(
        anchor.byte.start, hidden,
        "没渲染过的字不该被当成排版结果里的位置"
    );
    assert_eq!(index.glyph_count(), 3, "只有「正文。」三个字有字形");
}

#[test]
fn later_bytes_land_on_later_pages() {
    let (source, doc) = layout(&paged_document(6));
    let index = LayoutIndex::build(&doc, &source);
    assert_eq!(index.page_count(), 6);

    let mut last_page = 0;
    let mut seen = 0;
    for byte in char_starts(&source) {
        let Some(anchor) = index.forward(byte) else {
            continue;
        };
        assert!(
            anchor.page >= last_page,
            "字节 {byte} 跑到前面的页去了：{} < {last_page}",
            anchor.page
        );
        if anchor.page > last_page {
            seen += 1;
        }
        last_page = anchor.page;
    }
    assert!(seen >= 4, "该跨过好几页，实际只跨了 {seen} 次");
}

#[test]
fn cjk_glyphs_map_to_their_own_bytes() {
    let (source, doc) = layout("一二三四五\n");
    let index = LayoutIndex::build(&doc, &source);

    let text = source.text();
    for (i, ch) in "一二三四五".chars().enumerate() {
        let byte = i * ch.len_utf8();
        let anchor = index
            .forward(byte)
            .unwrap_or_else(|| panic!("第 {i} 个汉字该有字形"));
        assert_eq!(
            anchor.byte,
            byte..byte + ch.len_utf8(),
            "第 {i} 个汉字的字节范围不对"
        );
        assert_eq!(&text[anchor.byte.clone()], &ch.to_string());
    }

    // 横向依次递增（中日韩没有连字，一个字一个框）
    let xs: Vec<f32> = (0..5)
        .map(|i| index.forward(i * 3).unwrap().rect[0])
        .collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "横向该递增：{xs:?}");
}

#[test]
fn a_document_without_text_is_empty_not_a_panic() {
    let (source, doc) = layout("#rect(width: 2cm, height: 1cm)\n");
    let index = LayoutIndex::build(&doc, &source);

    assert_eq!(index.glyph_count(), 0, "这个文档一个字都没有");
    assert_eq!(index.forward(0), None);
    assert_eq!(index.inverse(0, 10.0, 10.0), None);
}

/// 两个方向都要在**一帧之内**：跳转是交互动作，卡一下就能感觉到。
/// 数字打印出来看 —— 阈值只用来抓灾难性退化（与 `perf.rs` 同一条规矩）。
#[test]
fn the_index_and_the_lookups_stay_well_inside_a_frame() {
    let mut text = String::from("= 大文档\n\n");
    while text.lines().count() < 3000 {
        let i = text.lines().count();
        text.push_str(&format!(
            "== 小节 {i}\n\n这是第 {i} 段正文，用来把文档撑长。\n\n"
        ));
    }

    let (source, doc) = layout(&text);
    let pages = doc.pages().len();

    let t = Instant::now();
    let index = LayoutIndex::build(&doc, &source);
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;

    // 前向：抽 200 个字节
    let starts = char_starts(&source);
    let samples: Vec<usize> = starts.iter().step_by(starts.len() / 200).copied().collect();

    let t = Instant::now();
    let mut hits = 0;
    for byte in &samples {
        if index.forward(*byte).is_some() {
            hits += 1;
        }
    }
    let forward_us = t.elapsed().as_secs_f64() * 1_000_000.0 / samples.len() as f64;

    // 反向：对每一页的中部点一次
    let t = Instant::now();
    let mut found = 0;
    for page in 0..pages {
        let size = doc.pages()[page].frame.size();
        if index
            .inverse(
                page,
                size.x.to_pt() as f32 / 2.0,
                size.y.to_pt() as f32 / 2.0,
            )
            .is_some()
        {
            found += 1;
        }
    }
    let inverse_us = t.elapsed().as_secs_f64() * 1_000_000.0 / pages.max(1) as f64;

    println!(
        "\n=== 跳转索引：{pages} 页 / {} 行 ===",
        text.lines().count()
    );
    println!("字形数            {:>8}", index.glyph_count());
    println!("构建              {build_ms:>8.2} ms");
    println!(
        "前向查找          {forward_us:>8.1} µs   （{} 次抽样）",
        samples.len()
    );
    println!("反向查找          {inverse_us:>8.1} µs   （{pages} 个页面中点）\n");

    assert!(hits > 0, "前向一个都没命中，样本取错了");
    assert_eq!(found, pages, "每一页的中点都该吸附到文字");
    assert!(
        build_ms < 100.0,
        "建索引花了 {build_ms:.1} ms，太久了（交互动作，该在一帧内）"
    );
    assert!(
        forward_us < 1000.0 && inverse_us < 1000.0,
        "查找太慢：前向 {forward_us:.1} µs / 反向 {inverse_us:.1} µs"
    );
}
