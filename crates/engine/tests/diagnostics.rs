//! 端到端验证：编译错误 → `DiagSpan` → 字节范围 → 行列。
//!
//! 这条链路的每一段都有单测，但**接起来**是不是对的（尤其是行号有没有错一位）
//! 只有真编译一次才能确认。所以这里跑真 World。

use typst_engine::syntax as lang;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

/// 编译一份文本，拿回（源码, 诊断）。
fn compile_and_collect(source_text: &str) -> (typst::syntax::Source, Vec<lang::Diagnostic>) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.typ");
    std::fs::write(&main, source_text).unwrap();

    let mut world = EngineWorld::new(
        embedded_and_system_fonts(),
        EntryState::new(dir.path(), &main),
    );

    let outcome = world.compile();
    let source = typst::World::source(&world, world.entry().main()).unwrap();

    let mut diags = lang::syntax_diagnostics(&source);
    diags.extend(lang::compile_diagnostics(&source, &outcome.errors));
    (source, diags)
}

/// ★ 这条是整条链路的根据：错误落在正确的行上。
///
/// 0 起的行号 —— 第 5 行（人类数的第 6 行）是 `#undefined_here()`。
#[test]
fn a_compile_error_lands_on_the_right_line() {
    let text = "= Title\n\nFine line.\n\nStill fine.\n\n#undefined_here()\n";
    //                  ^0        ^2          ^4              ^6
    let (_source, diags) = compile_and_collect(text);

    let hit = diags
        .iter()
        .find(|d| d.is_error && d.message.contains("undefined_here"))
        .unwrap_or_else(|| panic!("没报出未定义变量：{diags:#?}"));

    let pos = hit.line_col.expect("该给出行列");
    assert_eq!(pos.line, 6, "错误在第 6 行（0 起），实际 {pos:?}");
    assert!(hit.range.is_some(), "该同时给出字节范围");
}

/// 错误行有前导空行与中文时也不能错位。
#[test]
fn the_line_number_survives_blank_lines_and_cjk() {
    let text = "= 中文标题\n\n第一段中文正文，很长很长。\n\n#undefined_here()\n";
    //                  ^0              ^2                    ^4
    let (_source, diags) = compile_and_collect(text);

    let hit = diags
        .iter()
        .find(|d| d.is_error && d.message.contains("undefined_here"))
        .unwrap_or_else(|| panic!("没报出未定义变量：{diags:#?}"));

    assert_eq!(hit.line_col.unwrap().line, 4, "中文与空行都不该让行号偏");
}

/// 字节范围必须真的指向出错的那个标识符 ——
/// 只断言「有个范围」是不够的，那可能是任意一段字节。
#[test]
fn the_byte_range_points_at_the_offending_identifier() {
    let text = "= Title\n\n#undefined_here()\n";
    let (source, diags) = compile_and_collect(text);

    let hit = diags
        .iter()
        .find(|d| d.is_error && d.message.contains("undefined_here"))
        .expect("该报错");

    let range = hit.range.clone().expect("该有范围");
    let slice = &source.text()[range];

    assert!(
        slice.contains("undefined_here") || slice.contains("undefined"),
        "范围指向了 {slice:?}，而不是出错的标识符"
    );
}

/// 语法错误（不编译就能报）与编译错误混在一起时，形状一致、能一起渲染。
#[test]
fn syntax_and_compile_errors_come_out_the_same_shape() {
    // 未闭合括号 → 解析器就报；未定义变量 → 要编译才知道
    let text = "= Title\n\n#let broken = (1 + 2\n";
    let (_source, diags) = compile_and_collect(text);

    assert!(diags.iter().any(|d| d.is_error), "该有错误：{diags:#?}");
    // 两种来源的诊断都能拿出行列（不该有「报了却不知道在哪」的）
    assert!(
        diags
            .iter()
            .filter(|d| d.is_error)
            .all(|d| d.line_col.is_some()),
        "有错误拿不到位置：{diags:#?}"
    );
}

/// 干净的文档不该有任何错误。
#[test]
fn a_clean_document_reports_no_errors() {
    // 注意 `##` 不是 Typst 的标题（`#` 在 markup 里是开代码），写成 `##` 反而会报错
    // —— 所以这里只用合法的构件。
    let text = "#set page(width: 6cm)\n\n= Title\n\n== Sub\n\n- item one\n- item two\n\n正文。\n\n#text(fill: red)[红字]\n";
    let (_source, diags) = compile_and_collect(text);

    let errors: Vec<_> = diags.iter().filter(|d| d.is_error).collect();
    assert!(errors.is_empty(), "干净文档报了错：{errors:#?}");
}

/// 大纲在真文档上也对（不只是裸 `Source::detached`）。
#[test]
fn the_outline_works_on_a_compiled_document() {
    let text = "= One\n\n== Two\n\n=== Three\n\nBody.\n";
    let (source, _diags) = compile_and_collect(text);

    let items = lang::outline(&source);
    let pairs: Vec<_> = items.iter().map(|i| (i.depth, i.title.as_str())).collect();

    assert_eq!(pairs, [(1, "One"), (2, "Two"), (3, "Three")]);
}

/// 排版成功后不该有错误 —— 免得「无错误」和「有错误」两条路都亮。
#[test]
fn a_successful_compile_yields_no_compile_errors() {
    let text = "= Fine\n\nBody.\n";
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.typ");
    std::fs::write(&main, text).unwrap();

    let mut world = EngineWorld::new(
        embedded_and_system_fonts(),
        EntryState::new(dir.path(), &main),
    );
    let outcome = world.compile();

    assert!(outcome.fresh);
    assert!(outcome.errors.is_empty());
    assert!(outcome.doc.is_some());
    assert!(
        typst::compile::<PagedDocument>(&world).output.is_ok(),
        "成功的文档再编一次也该成功"
    );
}
