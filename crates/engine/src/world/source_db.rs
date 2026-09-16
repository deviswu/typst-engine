//! 源文件数据库：一棵被持续就地编辑的语法树。
//!
//! 它不只是「带记忆化的缓存」。typst 官方文档
//! （`typst-library-0.15.1/src/lib.rs:47-55`）明确建议长驻程序保留 `Source`
//! 并用 `Source::edit` 就地修改，以换取增量性能。本模块照此实现：
//! 被编辑的文件在 [`SourceDb::feed_memory`] 里走 `Source::replace`
//! 增量重解析，而不是 `Source::new` 重建。

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use typst::diag::FileResult;
use typst::foundations::Bytes;
use typst::syntax::{FileId, Source};

use super::query::QueryRef;

/// [`SourceDb::feed_memory`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedOutcome {
    /// 是否新建了 `Source`（而非增量重解析）。
    pub created: bool,
    /// 实际重解析的字节范围。新建时为 `None`。
    pub reparsed: Option<Range<usize>>,
}

#[derive(Default)]
struct Slot {
    source: QueryRef<Source>,
    bytes: QueryRef<Bytes>,
}

/// 源文件与资源文件的缓存。
#[derive(Default)]
pub struct SourceDb {
    slots: Mutex<HashMap<FileId, Slot>>,
    /// `Source::new` 被调用了多少次。供测试断言「没有重建」。
    constructs: AtomicUsize,
}

impl SourceDb {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取 `Source`，未缓存则用 `f` 构造。
    ///
    /// **锁不变式：`f` 内部不得调用 `SourceDb` 的其它方法**，否则会自死锁
    /// （下面的锁在 `f` 执行期间是持着的）。调用方需要先把自己要读的缓存
    /// 取好，再传进来 —— `EngineWorld::source` 就是这么做的。
    pub fn source(&self, id: FileId, f: impl FnOnce() -> FileResult<Source>) -> FileResult<Source> {
        let mut slots = self.slots.lock();
        let slot = slots.entry(id).or_default();
        let constructs = &self.constructs;

        slot.source.get_or_init(|| {
            constructs.fetch_add(1, Ordering::Relaxed);
            f()
        })
    }

    /// 取 `Bytes`，未缓存则用 `f` 构造。
    ///
    /// 与 [`SourceDb::source`] 相同的锁不变式。
    pub fn bytes(&self, id: FileId, f: impl FnOnce() -> FileResult<Bytes>) -> FileResult<Bytes> {
        let mut slots = self.slots.lock();
        let slot = slots.entry(id).or_default();

        slot.bytes.get_or_init(f)
    }

    /// ★ 编辑器敲键：对已缓存的 `Source` 做增量重解析。
    ///
    /// 已有缓存 → `Source::replace`（typst 自己找最小改动并增量 reparse）。
    /// 尚无缓存 → 退化成 `Source::new`。
    pub fn feed_memory(&self, id: FileId, text: &str) -> FeedOutcome {
        let mut slots = self.slots.lock();
        let slot = slots.entry(id).or_default();

        // 先把「能不能就地改」问出来，再动 bytes —— 避免两块字段的借用交叉。
        let reparsed = slot.source.get_mut_if_filled().map(|src| src.replace(text));

        if let Some(reparsed) = reparsed {
            // 文本变了，字节缓存必须作废。
            slot.bytes.rehydrate();
            return FeedOutcome {
                created: false,
                reparsed: Some(reparsed),
            };
        }

        self.constructs.fetch_add(1, Ordering::Relaxed);
        *slot = Slot {
            source: QueryRef::from_value(Ok(Source::new(id, text.to_owned()))),
            bytes: QueryRef::new(),
        };
        FeedOutcome {
            created: true,
            reparsed: None,
        }
    }

    /// 作废单个文件。其余条目原样保留 —— 这是增量生效的关键。
    pub fn invalidate(&self, id: FileId) {
        let mut slots = self.slots.lock();
        if let Some(slot) = slots.get_mut(&id) {
            slot.source.rehydrate();
            slot.bytes.rehydrate();
        }
    }

    /// 已缓存的文本。编辑器喂进来的未保存内容就在这里。
    pub fn cached_text(&self, id: FileId) -> Option<String> {
        let slots = self.slots.lock();
        let slot = slots.get(&id)?;
        slot.source.peek().map(|s| s.text().to_owned())
    }

    /// 作废全部文件的缓存。
    ///
    /// 用于「手动重新编译」：磁盘上的**被 include 的文件**可能被外部改过，
    /// 而我们没有文件监听。作废后重编就会重新读磁盘。
    ///
    /// 主文件的内存编辑不会因此丢失 —— 它的文本同时存在于 VFS 覆盖层里，
    /// 重新读到的还是编辑器里的那份。
    pub fn invalidate_all(&self) {
        self.slots.lock().clear();
    }

    /// `Source::new` 被调用了几次。
    pub fn construct_count(&self) -> usize {
        self.constructs.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};

    use super::*;

    fn fid(name: &str) -> FileId {
        FileId::new(RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new(name).unwrap(),
        ))
    }

    fn source_for(id: FileId, text: &str) -> Source {
        Source::new(id, text.to_owned())
    }

    /// 一份够长的文档，好让「只重解析一小段」与「全文重解析」差别明显。
    fn long_doc() -> String {
        let mut text = String::new();
        for i in 0..300 {
            text.push_str(&format!("= Heading {i}\n\nSome body text number {i}.\n\n"));
        }
        text
    }

    /// I1：同一份内容取两次，不该重新构造 Source。
    #[test]
    fn does_not_recompute_a_cached_source() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        let first = db.source(id, || Ok(source_for(id, "= Title"))).unwrap();
        let second = db.source(id, || Ok(source_for(id, "= Title"))).unwrap();

        assert_eq!(db.construct_count(), 1, "闭包只该跑一次");
        assert!(std::ptr::eq(first.root(), second.root()), "该是同一棵树");
    }

    #[test]
    fn caches_bytes_too() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        let a = db
            .bytes(id, || Ok(Bytes::from_string("data".to_owned())))
            .unwrap();
        let b = db
            .bytes(id, || Ok(Bytes::from_string("other".to_owned())))
            .unwrap();

        assert_eq!(a.as_slice(), b"data");
        assert_eq!(b.as_slice(), b"data", "第二次该拿缓存，而不是 other");
    }

    /// I2（本模块最重要的一条）：编辑走增量重解析，不是重建。
    ///
    /// 直接读 `Source::replace` 返回的重解析范围 —— 让「增量是否生效」
    /// 变成一个可以断言的数字，而不是靠跑分推测。
    #[test]
    fn feeding_memory_reparses_incrementally_not_from_scratch() {
        let db = SourceDb::new();
        let id = fid("/a.typ");
        let text = long_doc();

        db.source(id, || Ok(source_for(id, &text))).unwrap();
        assert_eq!(db.construct_count(), 1);

        // 在中间插一个字符。
        let insert_at = text.len() / 2;
        let mut edited = text.clone();
        edited.insert(insert_at, 'X');

        let outcome = db.feed_memory(id, &edited);

        assert!(!outcome.created, "已有缓存时必须是增量重解析，不能重建");
        assert_eq!(db.construct_count(), 1, "不能调 Source::new");
        assert_eq!(db.cached_text(id).unwrap(), edited, "文本要更新");

        let reparsed = outcome.reparsed.expect("增量路径必须给出重解析范围");
        eprintln!(
            "增量重解析实测：{} 字节 / 全文 {} 字节 = {}‰",
            reparsed.len(),
            text.len(),
            reparsed.len() * 1000 / text.len()
        );
        assert!(
            reparsed.len() < text.len() / 10,
            "只重解析了 {} 字节，占全文 {:.1}% —— 退化成了全量",
            reparsed.len(),
            reparsed.len() as f64 * 100.0 / text.len() as f64,
        );
    }

    /// 首次编辑（还没缓存过）必须能工作：退化成新建。
    #[test]
    fn the_first_feed_creates_the_source() {
        let db = SourceDb::new();
        let id = fid("/a.typ");

        let outcome = db.feed_memory(id, "= Hello");

        assert!(outcome.created);
        assert!(outcome.reparsed.is_none());
        assert_eq!(db.construct_count(), 1);
        assert_eq!(db.cached_text(id).unwrap(), "= Hello");
    }

    /// 连续喂多次增量编辑，每次都该走增量路径。
    #[test]
    fn repeated_feeds_stay_incremental() {
        let db = SourceDb::new();
        let id = fid("/a.typ");
        let mut text = long_doc();
        db.source(id, || Ok(source_for(id, &text))).unwrap();

        for round in 0..20 {
            text.push_str(&format!("\n\nRound {round}.\n"));
            let outcome = db.feed_memory(id, &text);
            assert!(!outcome.created, "第 {round} 轮退化成重建了");
            assert!(outcome.reparsed.is_some());
        }

        assert_eq!(db.construct_count(), 1, "全程只该构造一次 Source");
        assert_eq!(db.cached_text(id).unwrap(), text);
    }

    #[test]
    fn invalidate_forces_a_recompute() {
        let db = SourceDb::new();
        let id = fid("/a.typ");
        db.source(id, || Ok(source_for(id, "one"))).unwrap();

        db.invalidate(id);
        db.source(id, || Ok(source_for(id, "two"))).unwrap();

        assert_eq!(db.construct_count(), 2);
        assert_eq!(db.cached_text(id).unwrap(), "two");
    }

    #[test]
    fn invalidating_a_missing_entry_is_harmless() {
        let db = SourceDb::new();
        db.invalidate(fid("/nope.typ"));
        assert_eq!(db.construct_count(), 0);
    }

    #[test]
    fn unrelated_files_keep_their_sources() {
        let db = SourceDb::new();
        let a = fid("/a.typ");
        let b = fid("/b.typ");

        let b_before = db.source(b, || Ok(source_for(b, "b"))).unwrap();
        db.source(a, || Ok(source_for(a, "a"))).unwrap();

        // 喂 a 之后，b 必须还是同一棵树。
        db.feed_memory(a, "a edited");

        let b_after = db.source(b, || Ok(source_for(b, "b"))).unwrap();
        assert!(
            std::ptr::eq(b_before.root(), b_after.root()),
            "b 不该被动过"
        );
    }
}
