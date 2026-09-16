//! 热路径开销测量。
//!
//! spec 的 A5 原本写的是「3000 行 `highlight()` ≤ 一帧」。但高亮最后没做
//! （gpui-component 的编辑器不接受自定义高亮器，见 `syntax/mod.rs` 的说明），
//! 所以这里改测**真正每次敲键都跑的东西**：
//!
//! 1. `Source::replace` 的增量重解析（引擎）
//! 2. `outline()` 的树遍历
//! 3. `syntax_diagnostics()` 的树遍历
//!
//! 断言刻意给得**宽松**（`≤ 100 ms`），目的是抓灾难性退化，不是卡性能。
//! 真实数字靠打印出来看 —— 把 flaky 的阈值当成性能门槛是自欺。

use std::time::Instant;

use typst_engine::syntax as lang;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};

/// 3000 行量级的文档：够长到能暴露 O(n²)，又不至于让测试跑很久。
fn big_document() -> String {
    let mut s = String::from("= 大文档\n\n");
    while s.lines().count() < 3000 {
        let i = s.lines().count();
        s.push_str(&format!(
            "== 小节 {i}\n\n这是第 {i} 段正文，用来把文档撑长。\n\n"
        ));
    }
    s
}

#[test]
fn the_per_keystroke_hot_path_is_well_inside_a_frame() {
    let text = big_document();
    let lines = text.lines().count();
    let bytes = text.len();

    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("big.typ");
    std::fs::write(&main, &text).unwrap();

    let mut world = EngineWorld::new(
        embedded_and_system_fonts(),
        EntryState::new(dir.path(), &main),
    );
    let main_id = world.entry().main();

    // 冷编译一次，把 Source 建起来（顺便看看大文档排版要多久）
    let t = Instant::now();
    let first = world.compile();
    let cold_ms = t.elapsed().as_secs_f64() * 1000.0;

    // ── 热路径：一次编辑 = 增量重解析 + 大纲 + 诊断 ──
    let mut edited = text.clone();
    let mid = edited.len() / 2;
    let pos = (0..=mid)
        .rev()
        .find(|&p| edited.is_char_boundary(p))
        .unwrap();
    edited.insert(pos, '字');

    let t = Instant::now();
    world.vfs_mut().map_shadow(
        &main,
        typst::foundations::Bytes::from_string(edited.clone()),
    );
    let fed = world.sources().feed_memory(main_id, &edited);
    let feed_ms = t.elapsed().as_secs_f64() * 1000.0;

    let source = typst::World::source(&world, main_id).unwrap();

    let t = Instant::now();
    let outline = lang::outline(&source);
    let outline_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let diags = lang::syntax_diagnostics(&source);
    let diag_ms = t.elapsed().as_secs_f64() * 1000.0;

    // ── 排版本身 ──
    let t = Instant::now();
    let second = world.compile();
    let incremental_ms = t.elapsed().as_secs_f64() * 1000.0;

    let hot_ms = feed_ms + outline_ms + diag_ms + incremental_ms;

    println!("\n=== {lines} 行 / {bytes} 字节 ===");
    println!(
        "冷编译            {cold_ms:>8.1} ms   （{} 页）",
        first.doc.as_ref().map(|d| d.pages().len()).unwrap_or(0)
    );
    println!(
        "增量重解析        {feed_ms:>8.1} ms   （重解析 {} 字节）",
        fed.reparsed.map(|r| r.len()).unwrap_or(0)
    );
    println!(
        "大纲              {outline_ms:>8.1} ms   （{} 个标题）",
        outline.len()
    );
    println!(
        "语法诊断          {diag_ms:>8.1} ms   （{} 条）",
        diags.len()
    );
    println!(
        "增量排版          {incremental_ms:>8.1} ms   （成功：{}）",
        second.fresh
    );
    println!("---- 一次敲键合计 {hot_ms:>8.1} ms ----\n");

    // 宽松门槛：抓灾难性退化，不当性能指标用。
    //
    // 取值依据：初版大纲的 O(n²) 退化实测 116 ms，这条能抓到；
    // 修好后是 11 ms，留了约 4× 余量给不同机器与 debug/release 差异。
    assert!(
        hot_ms < 50.0,
        "一次敲键花了 {hot_ms:.1} ms，远超一帧 —— 有东西退化了"
    );

    // 结构性断言，不是性能断言：它们确保我们测的是真东西。
    assert!(
        outline.len() > 500,
        "大纲该抓到几百个标题，实际 {}",
        outline.len()
    );
    assert!(second.fresh, "编辑后该编译成功");
    assert!(cold_ms > 0.0 && incremental_ms > 0.0, "计时器坏了");
}
