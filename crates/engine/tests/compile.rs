//! 集成测试：真的把 .typ 编译成排版结果。

use std::path::PathBuf;

use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn world_for(name: &str) -> EngineWorld {
    let path = fixture(name);
    let root = path.parent().unwrap().to_path_buf();
    EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path))
}

#[test]
fn compiles_a_simple_document() {
    let world = world_for("hello.typ");

    let result = typst::compile::<PagedDocument>(&world);

    assert!(
        result.output.is_ok(),
        "编译失败：{:?}",
        result.output.as_ref().unwrap_err()
    );
    let doc = result.output.unwrap();
    assert_eq!(
        doc.pages().len(),
        2,
        "fixture 里有 1 个 pagebreak，应为 2 页"
    );
}

/// 文件缺失必须是诊断，不是 panic。
#[test]
fn a_missing_main_file_is_an_error_not_a_panic() {
    let path = fixture("does-not-exist.typ");
    let root = path.parent().unwrap().to_path_buf();
    let world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));

    let result = typst::compile::<PagedDocument>(&world);

    assert!(result.output.is_err(), "缺文件应该报错");
}

/// ★ 本计划的核心验收：磁盘上是 1 页的文档，喂进 2 页的未保存文本，
///   编译结果必须跟着内存走，而磁盘文件纹丝不动。
#[test]
fn compiles_unsaved_text_from_the_overlay() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let disk_before = std::fs::read_to_string(&path).unwrap();

    // 内存版本：多一个 pagebreak，所以是 2 页。
    let unsaved = "\
= Memory version

Page one body.

#pagebreak()

= Second page

Page two body.
";

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));

    // ① 先按磁盘编译：1 页。
    let doc = typst::compile::<PagedDocument>(&world)
        .output
        .expect("磁盘版本该编译成功");
    assert_eq!(doc.pages().len(), 1, "磁盘版本应该是 1 页");

    // ② 把未保存文本喂进覆盖层 + 增量重解析。
    let main = world.entry().main();
    world.vfs_mut().map_shadow(
        &path,
        typst::foundations::Bytes::from_string(unsaved.to_owned()),
    );
    let outcome = world.sources().feed_memory(main, unsaved);
    assert!(!outcome.created, "第二次喂养必须是增量重解析");

    // ③ 再编译：必须是 2 页。
    let doc = typst::compile::<PagedDocument>(&world)
        .output
        .expect("内存版本该编译成功");
    assert_eq!(doc.pages().len(), 2, "编译结果该跟着内存里的未保存文本走");

    // ④ 磁盘上什么都没动。
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        disk_before,
        "绝不能写磁盘"
    );
}

/// 编译失败不能 panic，必须给出诊断。
#[test]
fn a_syntax_error_yields_diagnostics_not_a_panic() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let broken = "#let x = (1 + \n\n= Unclosed";

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));
    let main = world.entry().main();
    world.vfs_mut().map_shadow(
        &path,
        typst::foundations::Bytes::from_string(broken.to_owned()),
    );
    world.sources().feed_memory(main, broken);

    let result = typst::compile::<PagedDocument>(&world);
    let errors = match result.output {
        Ok(_) => panic!("坏语法该报错，却编译成功了"),
        Err(errors) => errors,
    };
    assert!(!errors.is_empty(), "至少该给出一条错误诊断");
}

/// revision 只在内容真变时推进 —— 这是 Plan 2 防抖的地基。
#[test]
fn feeding_identical_text_does_not_bump_the_revision() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let text = "= Same\n\nUnchanged.\n";

    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));
    let main = world.entry().main();
    let bytes = typst::foundations::Bytes::from_string(text.to_owned());

    world.vfs_mut().map_shadow(&path, bytes.clone());
    let rev = world.vfs().revision();

    // 先把 Source 建起来，否则下面量到的是「冷启动」而不是幂等性。
    typst::World::source(&world, main).unwrap();

    assert!(
        !world.vfs_mut().map_shadow(&path, bytes),
        "相同内容不该算变化"
    );
    assert_eq!(world.vfs().revision(), rev);

    // feed_memory 必须幂等：内容一样就一点也不用重解析。
    let outcome = world.sources().feed_memory(main, text);
    assert!(!outcome.created, "已有缓存时该走增量重解析路径");
    assert_eq!(
        outcome.reparsed.map(|r| r.len()),
        Some(0),
        "喂完全相同的文本，重解析范围应当为空"
    );
}

/// 反复编辑同一份文档，全程必须保持在增量路径上（不重建 Source）。
#[test]
fn repeated_edits_stay_incremental_end_to_end() {
    let path = fixture("overlay.typ");
    let root = path.parent().unwrap().to_path_buf();
    let mut world = EngineWorld::new(embedded_and_system_fonts(), EntryState::new(root, &path));
    let main = world.entry().main();

    // 先编译一次，把 Source 建起来。
    typst::compile::<PagedDocument>(&world)
        .output
        .expect("首次编译该成功");
    let constructs_after_first = world.sources().construct_count();

    for round in 0..10 {
        let text = format!("= Round {round}\n\nBody for round {round}.\n");
        world
            .vfs_mut()
            .map_shadow(&path, typst::foundations::Bytes::from_string(text.clone()));
        let outcome = world.sources().feed_memory(main, &text);

        assert!(!outcome.created, "第 {round} 轮退化成重建 Source 了");

        typst::compile::<PagedDocument>(&world)
            .output
            .unwrap_or_else(|e| panic!("第 {round} 轮编译失败：{e:?}"));
    }

    assert_eq!(
        world.sources().construct_count(),
        constructs_after_first,
        "10 轮编辑不该构造出任何新的 Source"
    );
}
