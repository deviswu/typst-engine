//! 导出：把排版结果变成能显示或保存的东西。
//!
//! 这是 L3。目前有两块：
//! - [`svg`] —— 矢量，适合导出与将来的页内 diff
//! - [`pixmap`] —— 位图，适合屏幕预览，**DPI 由调用方决定**
//!
//! 完整的导出计算图（`Computable` + `TypeId` 缓存 + 页级增量 + 不可见页
//! 卸载纹理）属于 Plan 2 的 A5–A7。

mod pdf;
mod pixmap;
mod svg;

pub use pdf::*;
pub use pixmap::*;
pub use svg::*;
