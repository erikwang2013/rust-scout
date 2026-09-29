//! 项目宠物 —— 检索机器人 **Scout**。
//!
//! 它是一台插槽式检索机器人：胸前八个模块插槽，插哪个用哪个 ——
//! 换后端就是换一枚模块，业务代码一行不改。
//! 本模块把这台机器人带进代码里 —— 终端横幅（[`banner`]）与错误提示
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
pub const SPECIES: &str = "检索机器人";

/// 一句话人设。
pub const TAGLINE: &str = "八个插槽，插哪个用哪个 —— 业务代码一行不改";

/// ASCII 形象：正面 Q 版 —— 圆脑袋上一根顶着指示灯的天线，
/// 面罩里两点眼，脑袋下面是短脖子和方躯干，两条短腿各一只圆脚掌。
/// 胸前就是模块舱：八个插槽画成 `+--+` 的 4×2 格，七格里插着 `##`，
/// 右上角那一格空着；一枚模块正从右边飞进来 —— `<- - - -'` 是它的轨迹，
/// 尾巴翘向右侧的 `[##]`，箭头落在空槽上。
///
/// 刻意只使用 7 位 ASCII：CJK 终端里制表符与 emoji 常被判为双宽，
/// 会让整台机器人歪掉。
pub const ART: &str = r#"                      (*)
                       |
         ______________|______________
        /                             \
        |    [o]               [o]    |
        |_____________________________|
                      | |
     _________________| |_________________
    /                                     \
    |      +-----+-----+-----+-----+      |     [##]
    |      |  ## |  ## |  ## |     |      |<- - - -'
   \|      +-----+-----+-----+-----+      |/
   o|      |  ## |  ## |  ## |  ## |      |o
    |      +-----+-----+-----+-----+      |
    \_____________________________________/
            ||                   ||
           _||_                 _||_
          (____)               (____)"#;

/// 终端横幅：形象 + 名牌 + 人设。纯文本，不含转义序列（可安全写进日志）。
pub fn banner() -> String {
    format!("{ART}\n\n  {NAME} · {SPECIES}\n  {TAGLINE}\n")
}

/// 针对具体错误，机器人给出的排查方向。
pub fn hint(err: &ScoutError) -> &'static str {
    match err {
        ScoutError::InvalidIndexName(_) => {
            "索引名里有空白、斜杠、通配符，或者以 `.` `-` `_` 开头 —— 这个槽位对不上，换一个名字试试？"
        }
        ScoutError::InvalidHost(_) => {
            "host 里塞了 `user:pass@` —— 凭据别放 URL，请求一出错它就会跟着错误信息进日志。"
        }
        ScoutError::InvalidFieldName(_) => {
            "字段名里有空格或运算符字符 —— 过滤表达式会被它改写成另一条查询，换个字段名。"
        }
        ScoutError::InvalidResult(_) => "文档字段得是 JSON 对象。我读到的是别的东西。",
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
        // 必须与 error.rs 里 Backend 变体的 cfg 完全一致：漏掉 xunsearch 会让
        // `cargo build --features xunsearch` 因 match 不穷尽而编译失败。
        #[cfg(any(
            feature = "elasticsearch",
            feature = "meilisearch",
            feature = "typesense",
            feature = "algolia",
            feature = "xunsearch"
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

/// 把错误渲染成「给人类看」的形态：原始错误 + 机器人的提示。
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
        // 混入宽字符会让终端里的机器人变形，这里直接钉死。
        assert!(ART.is_ascii(), "ASCII art must stay 7-bit");
        assert!(banner().contains(NAME));
    }

    #[test]
    fn hint_covers_every_error_variant() {
        let cases = [
            ScoutError::InvalidIndexName("a/b".into()),
            ScoutError::InvalidHost("userinfo".into()),
            ScoutError::InvalidFieldName("x:=1 || y".into()),
            ScoutError::InvalidResult("not an object".into()),
            ScoutError::Unsupported("no feature".into()),
            ScoutError::Json(serde_json::from_str::<i32>("x").unwrap_err()),
        ];
        for err in cases {
            assert!(!hint(&err).is_empty(), "no robot hint for {err:?}");
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
