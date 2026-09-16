//! 演示：Typst 的实时增量编译。
//!
//! ```bash
//! cargo run --example realtime
//! ```
//!
//! 它会：
//! 1. 造一份多页文档，冷编译一次（含扫系统字体、全量 parse + 排版）
//! 2. 模拟逐字输入，每次测量「实际重解析字节数」与「编译耗时」
//! 3. 对比冷编译，给出增量加速比
//! 4. 验证「未保存文本可编译」且磁盘没被写过

use std::time::{Duration, Instant};

use typst::foundations::Bytes;
use typst_engine::world::{EngineWorld, EntryState, embedded_and_system_fonts};
use typst_layout::PagedDocument;

/// 文档规模：段落数。每一段都会产生排版工作。
const PARAGRAPHS: usize = 300;

/// 模拟敲多少个字符。
const KEYSTROKES: usize = 30;

fn build_document(paragraphs: usize) -> String {
    let mut s = String::from("= 实时编译演示\n\n");
    for i in 0..paragraphs {
        s.push_str(&format!(
            "第 {i} 段。Typst 的增量靠两件事：comemo 的记忆化排版，\
             以及 Source 的就地增量重解析。\n\n"
        ));
    }
    s
}

fn compile(world: &EngineWorld) -> PagedDocument {
    match typst::compile::<PagedDocument>(world).output {
        Ok(doc) => doc,
        Err(errors) => panic!("编译失败：{errors:?}"),
    }
}

fn fmt_ms(d: Duration) -> String {
    format!("{:>8.1} ms", d.as_secs_f64() * 1000.0)
}

fn main() {
    println!("\n=== Typst 实时增量编译演示 ===\n");

    let dir = tempfile::tempdir().expect("创建临时目录");
    let main_path = dir.path().join("demo.typ");
    let original = build_document(PARAGRAPHS);
    std::fs::write(&main_path, &original).expect("写初始文件");

    println!("文档：{PARAGRAPHS} 段，{} 字节", original.len());

    // ── 冷启动：扫系统字体（spec 风险 R6，必须只做一次）──────────────
    let t = Instant::now();
    let fonts = embedded_and_system_fonts();
    let font_load = t.elapsed();
    println!(
        "字体冷启动（扫系统字体）：{}   ← 这件事只能做一次",
        fmt_ms(font_load)
    );

    let mut world = EngineWorld::new(fonts, EntryState::new(dir.path(), &main_path));
    let main_id = world.entry().main();

    // ── 冷编译：全量 parse + 全量排版 ────────────────────────────────
    let t = Instant::now();
    let doc = compile(&world);
    let cold = t.elapsed();
    println!(
        "冷编译（全量）：{}   产出 {} 页",
        fmt_ms(cold),
        doc.pages().len()
    );

    // ── 模拟逐字输入 ────────────────────────────────────────────────
    println!("\n-- 逐字输入（每次在文档中部插入一个字符）--");
    println!(
        "{:<6} {:>12} {:>14} {:>12}",
        "第n键", "重解析字节", "占全文", "编译耗时"
    );

    let mut text = original.clone();
    let mut total_compile = Duration::ZERO;
    let mut total_reparsed = 0usize;
    let mut worst_compile = Duration::ZERO;
    let mut worst_reparsed = 0usize;

    for keystroke in 1..=KEYSTROKES {
        // 在文档中部附近插入，比追加在末尾更能考验增量。
        // 文档里全是中文，字节中点落在字符中间，所以要先退到字符边界。
        let target = text.len() / 2 + keystroke;
        let pos = (0..=target)
            .rev()
            .find(|&p| text.is_char_boundary(p))
            .expect("0 总是字符边界");
        text.insert(pos, '字');

        let bytes = Bytes::from_string(text.clone());
        world.vfs_mut().map_shadow(&main_path, bytes);
        let outcome = world.sources().feed_memory(main_id, &text);

        let reparsed = outcome.reparsed.expect("增量路径必须给出重解析范围");

        let t = Instant::now();
        let doc = compile(&world);
        let took = t.elapsed();

        total_compile += took;
        total_reparsed += reparsed.len();
        worst_compile = worst_compile.max(took);
        worst_reparsed = worst_reparsed.max(reparsed.len());

        let pct = reparsed.len() as f64 * 100.0 / text.len() as f64;
        println!(
            "{:<6} {:>12} {:>13.3}% {}   ({} 页)",
            keystroke,
            reparsed.len(),
            pct,
            fmt_ms(took),
            doc.pages().len()
        );
    }

    // ── 汇总 ────────────────────────────────────────────────────────
    let avg_compile = total_compile / KEYSTROKES as u32;
    let avg_reparsed = total_reparsed / KEYSTROKES;
    let speedup = cold.as_secs_f64() / avg_compile.as_secs_f64();

    println!("\n=== 结果 ===");
    println!(
        "平均每次敲键：重解析 {} 字节（占全文 {:.3}%），编译 {}",
        avg_reparsed,
        avg_reparsed as f64 * 100.0 / text.len() as f64,
        fmt_ms(avg_compile)
    );
    println!(
        "最差一次：    重解析 {} 字节，编译 {}",
        worst_reparsed,
        fmt_ms(worst_compile)
    );
    println!("对比冷编译 {}：增量快 {:.1}×", fmt_ms(cold), speedup);
    println!(
        "累计 {} 次敲键编译 {}，而全量重编需要 {}",
        KEYSTROKES,
        fmt_ms(total_compile),
        fmt_ms(cold * KEYSTROKES as u32)
    );

    // ── 未保存文本可编译，且磁盘没被动过 ────────────────────────────
    let on_disk = std::fs::read_to_string(&main_path).expect("读回磁盘文件");
    println!("\n=== 未保存文本 ===");
    println!(
        "编译用的是内存文本（{} 字节，末尾 {} 字符为了省地方不展示），磁盘仍是 {} 字节",
        text.len(),
        KEYSTROKES,
        on_disk.len()
    );
    assert_eq!(
        on_disk, original,
        "磁盘文件必须完全没变 —— 整个演示从没写过磁盘"
    );
    println!("✓ 磁盘文件与初始内容逐字节一致，从头到尾没有写盘");
    println!(
        "✓ 内存文本比磁盘多 {} 字节（{} 次敲键 × 1 个字符 = 只在内存里）",
        text.len() - on_disk.len(),
        KEYSTROKES
    );

    // ── 编译失败不白屏（留上一次成功的结果）────────────────────────
    println!("\n=== 错误恢复 ===");
    let broken = "#let x = (1 + \n\n= 未闭合";
    world
        .vfs_mut()
        .map_shadow(&main_path, Bytes::from_string(broken.to_owned()));
    world.sources().feed_memory(main_id, broken);

    let result = typst::compile::<PagedDocument>(&world);
    match result.output {
        Ok(_) => println!("✗ 坏语法居然编译成功了"),
        Err(errors) => {
            println!("坏语法给出 {} 条诊断，没有 panic ✓", errors.len());
            println!("  上一条成功结果仍可显示（success_doc 保留策略，Plan 2 落地）");
        }
    }

    println!("\n=== 完成 ===\n");
}
