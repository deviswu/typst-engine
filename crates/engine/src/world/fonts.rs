//! 字体：内嵌字体 + 系统字体。

use typst_kit::fonts::{FontStore, embedded, system};

/// 构造一个字体仓库。
///
/// **扫系统字体是秒级操作（spec 风险 R6）** —— 调用方必须在后台线程构造
/// **一次**并长期持有，绝不能放在每次编译、甚至第一帧的路径上。
///
/// 返回 `FontStore` 而不是自定义包装：它已经满足 `typst::World` 的
/// `book()` 与 `font(i)` 两个方法，包一层只是多余的间接。
pub fn embedded_and_system_fonts() -> FontStore {
    let mut store = FontStore::new();
    store.extend(embedded());
    store.extend(system());
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_has_fonts() {
        let store = embedded_and_system_fonts();

        // `FontBook` 没有 `len()`，用索引探测：没有字体的话编译必然失败。
        assert!(
            store.book().info(0).is_some(),
            "内嵌字体应该至少给出一份字体，否则编译必然失败"
        );
        assert!(store.font(0).is_some(), "索引 0 的字体应该能加载");
    }

    #[test]
    fn the_book_and_the_store_agree_on_indices() {
        let store = embedded_and_system_fonts();

        // `typst::World` 要求 `book()` 的索引与 `font(i)` 对得上。
        for i in 0..50 {
            assert_eq!(
                store.book().info(i).is_some(),
                store.font(i).is_some(),
                "索引 {i} 在 book 与 font 之间不一致"
            );
        }
    }

    /// typst 会拿超出范围的索引来问字体（增量校验时会这样），
    /// 必须给 None 而不是 panic。
    #[test]
    fn an_out_of_range_index_is_none() {
        let store = embedded_and_system_fonts();
        assert!(store.font(usize::MAX).is_none());
    }
}
