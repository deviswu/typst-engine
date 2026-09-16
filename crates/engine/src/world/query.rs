//! 一个「只算一次」的格子。

use std::sync::{Arc, OnceLock};

use typst::diag::FileResult;

/// 记忆化的取值格。
///
/// 内部是 `Arc<OnceLock<_>>`：克隆只走引用计数，多个世界快照可以共享同一份结果。
/// 我们刻意**连失败一起缓存** —— 否则每次编译都会重试同一个坏文件。
pub struct QueryRef<T> {
    cell: Arc<OnceLock<FileResult<T>>>,
}

impl<T> Default for QueryRef<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for QueryRef<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
        }
    }
}

impl<T> QueryRef<T> {
    pub fn new() -> Self {
        Self {
            cell: Arc::new(OnceLock::new()),
        }
    }

    /// 直接放进一个已算好的值。
    pub fn from_value(value: FileResult<T>) -> Self {
        let cell = Arc::new(OnceLock::new());
        let _ = cell.set(value);
        Self { cell }
    }

    /// 取已有结果，没有就算一次并存下。
    pub fn get_or_init(&self, f: impl FnOnce() -> FileResult<T>) -> FileResult<T>
    where
        T: Clone,
    {
        self.cell.get_or_init(f).clone()
    }

    /// 看一眼已有值，不算。**缓存的失败不算有值**。
    pub fn peek(&self) -> Option<T>
    where
        T: Clone,
    {
        self.cell.get().and_then(|r| r.as_ref().ok()).cloned()
    }

    /// 若已填充**且是成功值**且当前是唯一持有者，拿到 `&mut T` 供**就地修改**。
    ///
    /// 拿不到独占（结果被别的快照共享了）时返回 `None`，
    /// 调用方退回「重建」路径 —— 语义仍然正确，只是慢一点。
    pub fn get_mut_if_filled(&mut self) -> Option<&mut T> {
        let cell = Arc::get_mut(&mut self.cell)?;
        cell.get_mut()?.as_mut().ok()
    }

    /// 丢掉已缓存的结果，下次重新算。
    pub fn rehydrate(&mut self) {
        *self = Self::new();
    }

    /// 是否已经算过。
    pub fn is_filled(&self) -> bool {
        self.cell.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use typst::diag::FileError;

    use super::*;

    #[test]
    fn computes_at_most_once() {
        let cell: QueryRef<String> = QueryRef::new();
        let calls = AtomicUsize::new(0);

        for _ in 0..5 {
            let got = cell
                .get_or_init(|| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok("computed".to_owned())
                })
                .unwrap();
            assert_eq!(got, "computed");
        }

        assert_eq!(calls.load(Ordering::Relaxed), 1, "只该算一次");
    }

    /// 失败也要被记住 —— 否则每次编译都会重试同一个坏文件。
    #[test]
    fn caches_failures_too() {
        let cell: QueryRef<String> = QueryRef::new();
        let calls = AtomicUsize::new(0);

        for _ in 0..3 {
            let err = cell
                .get_or_init(|| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Err(FileError::AccessDenied)
                })
                .unwrap_err();
            assert!(matches!(err, FileError::AccessDenied));
        }

        assert_eq!(calls.load(Ordering::Relaxed), 1, "失败也只算一次");
    }

    #[test]
    fn rehydrate_recomputes() {
        let mut cell: QueryRef<String> = QueryRef::new();
        assert_eq!(&cell.get_or_init(|| Ok("v1".to_owned())).unwrap(), "v1");

        cell.rehydrate();
        assert_eq!(&cell.get_or_init(|| Ok("v2".to_owned())).unwrap(), "v2");
    }

    #[test]
    fn peek_does_not_compute() {
        let cell: QueryRef<String> = QueryRef::new();
        assert!(cell.peek().is_none());
        assert!(!cell.is_filled());

        cell.get_or_init(|| Ok("v".to_owned())).unwrap();

        assert_eq!(cell.peek().unwrap(), "v");
        assert!(cell.is_filled());
    }

    /// 缓存的失败不算「有值」：`peek` 要给 None，`get_mut_if_filled` 也不能
    /// 给出一个会让调用方误以为可用的 `&mut`。
    #[test]
    fn a_cached_failure_is_not_a_value() {
        let mut cell: QueryRef<String> = QueryRef::new();
        let _ = cell.get_or_init(|| Err(FileError::AccessDenied));

        assert!(cell.is_filled(), "算过了");
        assert!(cell.peek().is_none(), "但不算有值");
        assert!(cell.get_mut_if_filled().is_none());
    }

    #[test]
    fn get_mut_if_filled_gives_exclusive_access() {
        let mut cell: QueryRef<String> = QueryRef::new();
        assert!(cell.get_mut_if_filled().is_none(), "空的时候拿不到");

        cell.get_or_init(|| Ok("v1".to_owned())).unwrap();

        let slot = cell.get_mut_if_filled().expect("唯一持有者应能拿到");
        slot.push('!');
        assert_eq!(cell.peek().unwrap(), "v1!");
    }

    /// 结果被共享时拿不到 `&mut` —— 必须如实返回 None，而不是给出一个
    /// 会让调用方以为「改成功了」的错误保证。
    #[test]
    fn get_mut_if_filled_refuses_when_shared() {
        let mut cell: QueryRef<String> = QueryRef::new();
        cell.get_or_init(|| Ok("v1".to_owned())).unwrap();

        let _shared = cell.clone();

        assert!(cell.get_mut_if_filled().is_none(), "被共享时不该给出 &mut");
    }

    /// 已填充但值是失败时，`get_mut_if_filled` 不该给出 `&mut`。
    /// （否则 `SourceDb::feed_memory` 会拿到一个不存在的 `Source`。）
    #[test]
    fn get_mut_if_filled_requires_a_success_value() {
        let mut cell: QueryRef<String> = QueryRef::new();
        let _ = cell.get_or_init(|| Err(FileError::NotFound(std::path::PathBuf::from("x"))));

        assert!(cell.get_mut_if_filled().is_none());
    }

    #[test]
    fn from_value_is_already_filled() {
        let cell = QueryRef::from_value(Ok("given".to_owned()));
        let calls = AtomicUsize::new(0);

        let got = cell
            .get_or_init(|| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok("computed".to_owned())
            })
            .unwrap();

        assert_eq!(got, "given");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
