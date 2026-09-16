//! 语法服务：只依赖 `typst-syntax`，不碰排版。
//!
//! # 为什么在 engine 里，而不是独立 crate
//!
//! 原计划（spec §4.2）是拆出 `crates/syntax-svc`，理由是**编译依赖隔离**：
//! 「外壳要每次敲键都做语法服务，它应该能不链接 typst 排版/渲染/PDF 那一大坨」。
//!
//! 实际接线后发现这个理由不成立：外壳必须链接 `engine` 才能拿到那棵
//! **增量维护的语法树**（`SourceDb` 里的 `Source`）。既然 engine 已经链了，
//! 隔离收益就是零，而多一个 crate 边界只多一份需要同步的 API 表面。
//!
//! 等真的出现「只要语法、不要排版」的消费方（比如一个纯 lint 工具），再拆不迟。
//!
//! # 为什么故意没有 `highlight()`
//!
//! `typst_syntax::highlight()` 是现成的（23 个 `Tag`，官方维护）。但
//! **gpui-component 的编辑器把高亮焊死在 tree-sitter 上**：
//! `LanguageConfig.language` 是 `tree_sitter::Language` 硬字段，
//! `SyntaxHighlighter` 是具体结构体（内部持 tree-sitter 树）而不是 trait，
//! `InputState` 上没有注入点。要用官方语法树着色，就得 fork gpui-component。
//!
//! 代价与收益不成比例，所以这里**不提供** `highlight()` ——
//! 不写没有消费方的代码。（同一条标准也用在「不做 ComputeGraph」上。）
//!
//! # 提供的三样东西
//!
//! - [`outline`]：文档大纲（标题树）
//! - [`syntax_diagnostics`]：**不编译**就能拿到的语法错误与警告
//! - [`compile_diagnostics`]：把编译错误映射成同样的形状
//!
//! 后两者共用一份 `DiagSpan → 字节范围` 的映射代码 —— 因为
//! `SyntaxDiagnostic.span` 与 `SourceDiagnostic.span` 本来就是同一个类型。

mod diagnostic;
mod outline;
mod stats;

pub use diagnostic::*;
pub use outline::*;
pub use stats::*;
