//! 项目宠物 —— 嗅探猎犬 **Scout**（Search Hound）。
//!
//! 它嗅探文档、追踪索引：只要还有查询没被满足，它就会顺着爪印找下去。
//! 本模块把这只狗带进代码里 —— 终端横幅（[`banner`]）与错误提示
//! （[`hint`] / [`format_error`]，也可直接调用
//! [`ScoutError::pet_hint`](crate::ScoutError::pet_hint)）。
//!
//! ```no_run
//! println!("{}", rust_scout::pet::banner());
//!
//! let err = rust_scout::ScoutError::Unsupported("缺少 feature".into());
//! eprintln!("{}", rust_scout::pet::format_error(&err));
//! ```
//!
//! 图形版形象见 `docs/svg/pet.svg`；可运行示例见 `examples/pet.rs`。

use crate::ScoutError;

/// 宠物的名字。
pub const NAME: &str = "Scout";

/// 物种 / 设定。
pub const SPECIES: &str = "嗅探猎犬";

/// 一句话人设。
pub const TAGLINE: &str = "嗅探文档，追踪索引 —— 哪里有查询，哪里就有它";

/// ASCII 形象：正面 Q 版 —— 一对长垂耳夹着大脑袋，两眼一点鼻口；
/// 颈间系侦察兵领巾，胸前挎着别有两排八枚徽章的帆布包，四条短腿各有脚掌。
///
/// 刻意只使用 7 位 ASCII：CJK 终端里制表符与 emoji 常被判为双宽，
/// 会让整只狗歪掉。
pub const ART: &str = r#"      ___              ___
     /   \            /   \
    |     |__________|     |
    |     /          \     |
    |    |   o    o   |    |
    |    |     __     |    |
    |     \   /  \   /     |
     \     \  \__/  /     /
      \     \________/    /
       \_________________/
         \   ~~~~~~   /
          \__________/
        +----------------+
        |  [] [] [] []   |
        |  [] [] [] []   |
        +----------------+
           ||      ||
           ||      ||
          (__)    (__)


   ,^.     ,^.     ,^.     ,^."#;

/// 终端横幅：形象 + 名牌 + 人设。纯文本，不含转义序列（可安全写进日志）。
pub fn banner() -> String {
    format!("{ART}\n\n  {NAME} · {SPECIES}\n  {TAGLINE}\n")
}

/// 针对具体错误，猎犬给出的排查方向。
pub fn hint(err: &ScoutError) -> &'static str {
    match err {
        ScoutError::InvalidIndexName(_) => {
            "索引名里有空白、斜杠，或者以点开头 —— 这条路我嗅不出味道，换一个名字试试？"
        }
        ScoutError::InvalidResult(_) => "文档字段得是 JSON 对象。我闻到的是别的东西。",
        ScoutError::Unsupported(_) => {
            "这个后端我还没找到路 —— Cargo.toml 里对应的 feature 启用了吗？"
        }
        ScoutError::Json(_) => "JSON 没解析出来 —— 检查一下字段的类型和嵌套层级。",
        #[cfg(any(
            feature = "elasticsearch",
            feature = "meilisearch",
            feature = "typesense",
            feature = "algolia"
        ))]
        ScoutError::Http(_) => "连不上后端 —— 服务起来了吗？地址（host）和 API key 对吗？",
        #[cfg(feature = "database")]
        ScoutError::Sqlx(_) => "SQLite 那边出问题了 —— 看看 database.url 指向的库和表结构。",
        #[cfg(any(
            feature = "elasticsearch",
            feature = "meilisearch",
            feature = "typesense",
            feature = "algolia"
        ))]
        ScoutError::Backend(_) => "后端拒绝了这次请求 —— 往上翻，它的原始错误信息里有线索。",
        #[cfg(feature = "xunsearch")]
        ScoutError::XunSearch(_) => {
            "xunsearchd 回了个我看不懂的东西 —— 项目的 ini 字段方案和索引端一致吗？"
        }
        #[cfg(feature = "xunsearch")]
        ScoutError::XunSearchIo(_) => {
            "没连上 xunsearchd —— 它起来了吗？index/search 端口（8383 / 8384）通吗？"
        }
    }
}

/// 把错误渲染成「给人类看」的形态：原始错误 + 猎犬的提示。
///
/// [`ScoutError`] 自身的 [`Display`](std::fmt::Display) 刻意保持单行、机器可读
/// —— `?` 传播、日志采集、CI 里 grep 错误串都依赖它。需要带宠物提示的输出时
/// 用这个函数，别去改 `Display`。
pub fn format_error(err: &ScoutError) -> String {
    format!("error: {err}\n\n  [o_o] {NAME}：{}\n", hint(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_is_pure_ascii() {
        // 混入宽字符会让终端里的狗变形，这里直接钉死。
        assert!(ART.is_ascii(), "ASCII art must stay 7-bit");
        assert!(banner().contains(NAME));
    }

    #[test]
    fn hint_covers_every_error_variant() {
        let cases = [
            ScoutError::InvalidIndexName("a/b".into()),
            ScoutError::InvalidResult("not an object".into()),
            ScoutError::Unsupported("no feature".into()),
            ScoutError::Json(serde_json::from_str::<i32>("x").unwrap_err()),
        ];
        for err in cases {
            assert!(!hint(&err).is_empty(), "no hound hint for {err:?}");
            assert!(format_error(&err).contains(NAME));
        }
    }

    #[test]
    fn format_error_keeps_the_original_message() {
        let err = ScoutError::Unsupported("engine driver `xunsearch` requires its feature".into());
        let text = format_error(&err);
        assert!(text.contains("engine driver `xunsearch` requires its feature"));
        assert!(text.contains("feature"));
    }
}
