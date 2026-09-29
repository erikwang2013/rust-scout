//! 跨驱动一致性套件 —— 闸门层（Tier G）。
//!
//! 「换后端不改业务代码」这条承诺只在契约真的跨驱动一致时才成立，而各驱动自带的
//! 测试都是各驱动的作者写的 ——「A 驱动先校验、B 驱动先发请求」这类分歧恰好落在每个
//! 人的视野之外。本套件由契约这一侧编写：八个驱动，同一段断言。
//!
//! 关键手法：网络驱动一律指向**死地址**（`http://127.0.0.1:1` / `127.0.0.1:1`），
//! 于是任何调用只可能落进三类 —— `Err(InvalidIndexName)`（校验跑在一切之前 ✅）、
//! `Ok(…)`（有东西抢在校验前面短路 ❌）、`Err(Http/Io/Backend/…)`（I/O 跑在校验
//! 前面 ❌）。进程内驱动（collection / database / null）产出同样的三类，所以断言体
//! 不必按驱动分叉；只有「调用合法」时网络驱动必然打不到后端，该断言记为**不可观测**
//! 而不是假装通过（后端侧语义要用捕获桩，属 Tier S）。
//!
//! # 例外表
//!
//! 分歧只有在能引用 README 的驱动能力表、或 `engine.rs` 里 trait 默认实现时才允许被
//! 声明（见各 `WHY_*`，报告里原样带出处）。没有引用的一律按**未声明分歧**报出 ——
//! 否则这套测试会把自己要找的 bug 原地盖章通过。判据一次性收齐再断言（`Sink::finish`）：
//! 首跑是一份**报告**，第一条分歧不遮住后面九条。
//!
//! 装置（观测分类 / 记录器 / 被测对象清单）在 `conformance_harness`，本模块只放内容：
//! 闸门、判据、例外表。

use crate::conformance_harness::{doc, inspect, Harness, Sink, Subject, Want};
use crate::{SearchBuilder, SearchDocument};
#[cfg(feature = "xunsearch")]
use crate::conformance_harness::DEAD_TCP;
// 各闸门用的是 `Subject.engine: Box<dyn Engine>`，trait 对象上的方法不需要把 trait 引入
// 作用域；只有 G1' 拿着具体的 `XunSearchEngine`，那里才需要 —— 其余配置下引入即告警。
#[cfg(feature = "xunsearch")]
use crate::engine::Engine;

/// 保留索引名：ES 会把它展开成「全部索引」，八个驱动一律拒绝。
const RESERVED: &str = "_all";
/// 保留名的其余类别：通配符 / 多索引表达式 / 前导 `.` `_` `-` / 空白 / 路径分隔符。
const BAD_NAMES: &[&str] = &["*", "a,b", ".x", "_hidden", "-dash", "a b", "a/b"];
/// 合法索引名：用来区分「拒绝来自名字」与「调用被打到了后端」。
const OK_NAME: &str = "books";
/// 表达式拼接驱动必须拒绝的字段名：一个空格就够改写整条 filter 表达式。
const BAD_FIELD: &str = "a b";

// —— 已声明例外的出处（原样进报告）。除 NULL 外都指 trait 默认实现或 README 驱动表。 ——
/// null 的契约：写永远成功、搜永远空（`src/null_engine.rs:8`），**仅限它覆写过的方法**。
const WHY_NULL: &str = "null 契约=永不报错（src/null_engine.rs:8）";
/// 未实现的操作走 trait 默认实现，先报 Unsupported 再谈校验（`src/engine.rs:92-116`）。
const WHY_UNIMPL: &str = "未实现 ⇒ trait 默认先报 Unsupported（src/engine.rs:92-116）";
/// xunsearch 未实现软删除：soft_delete / soft_delete_in / only_trashed（README 驱动表）。
const WHY_XUN_SOFT_DELETE: &str = "xunsearch 未实现软删除（README 驱动能力差异表）";
/// xunsearch 的 where_in / where_not_in 无对应协议命令（README 驱动表）。
const WHY_XUN_WHERE: &str = "xunsearch 无 where_in 协议命令（README 驱动能力差异表）";
/// 不带索引的 soft_delete 跨索引做不到：四个 HTTP 驱动走 trait 默认实现报 Unsupported
/// （`src/engine.rs:75-91` 的实现与其文档；README 同款说明）。
const WHY_INDEXLESS_SOFT_DELETE: &str =
    "不带索引的 soft_delete 跨索引做不到 ⇒ 默认 Unsupported（src/engine.rs:75-91）";
/// algolia 忽略 order_by（排序需预建 replica index，README 驱动表）：没有拼接点，
/// 也就没有校验可谈。
const WHY_ALGOLIA_ORDER: &str = "algolia 忽略 order_by（README 驱动能力差异表）";
/// 默认窗口：take 不传时 collection / database 返回全部命中（README 驱动表）。
const WHY_WINDOW_ALL: &str = "默认窗口=全部命中（README 驱动能力差异表）";
/// 默认窗口：其余六个驱动默认 10 条（同上一行出处）。
const WHY_WINDOW_TEN: &str = "默认窗口=10 条（README 驱动能力差异表）";

/// 类别判据的糖：`gate!(sink, 驱动, 说明, Want, 引用, 调用表达式)`。
macro_rules! gate {
    ($sink:expr, $driver:expr, $what:expr, $want:expr, $why:expr, $call:expr) => {{
        let r = $call.await;
        $sink.push($driver, $what, $want, inspect(&r), $why);
    }};
}

/// G1 —— 索引名校验先于一切短路与全部 I/O（`config::validate_index_name`）。
///
/// 保留名必须报 `InvalidIndexName`：不是 `Ok`，也不是 I/O 错误。**短路不是豁免** ——
/// `delete_in("_all", &[])`、空 `where_in` 的 `search`/`paginate` 同样要先校验：同一个
/// 输入不能因为「列表恰好是空的」就在一个驱动上 Ok、在另一个上 Err。
async fn g1_cases(sink: &mut Sink, s: &Subject) {
    let e = &s.engine;
    let ids = vec!["x".to_string()];
    let none: Vec<String> = Vec::new();
    let bad_doc = doc("d", RESERVED);
    let bad_bulk = vec![bad_doc.clone()];
    let within = SearchBuilder::new("").within(RESERVED);
    let within_empty_in = SearchBuilder::new("").within(RESERVED).where_in("tag", std::iter::empty::<i32>());

    // 通用期望：保留名 → InvalidIndexName。null 永不报错、未实现的操作先报 Unsupported，
    // 这两类按例外表声明（引用见 WHY_*）。
    let (w, why) = if s.caps.never_validates { (Want::Ok, WHY_NULL) } else { (Want::Index, "") };
    let (w_re, why_re) = if s.caps.no_reindex { (Want::Unsupported, WHY_UNIMPL) } else { (w, why) };
    let (w_sdi, why_sdi) = if s.caps.no_soft_delete_in { (Want::Unsupported, WHY_UNIMPL) } else { (w, why) };

    gate!(sink, s.name, "delete_in(保留名,[id])", w, why, e.delete_in(RESERVED, &ids));
    gate!(sink, s.name, "delete_in(保留名,[])", w, why, e.delete_in(RESERVED, &none));
    gate!(sink, s.name, "delete_bulk(保留名,[id])", w, why, e.delete_bulk(RESERVED, &ids));
    gate!(sink, s.name, "delete_bulk(保留名,[])", w, why, e.delete_bulk(RESERVED, &none));
    gate!(sink, s.name, "soft_delete_in(保留名,[id])", w_sdi, why_sdi, e.soft_delete_in(RESERVED, &ids));
    gate!(sink, s.name, "soft_delete_in(保留名,[])", w_sdi, why_sdi, e.soft_delete_in(RESERVED, &none));
    gate!(sink, s.name, "search(within 保留名)", w, why, e.search(&within));
    gate!(sink, s.name, "search(within 保留名+空 where_in)", w, why, e.search(&within_empty_in));
    gate!(sink, s.name, "paginate(within 保留名)", w, why, e.paginate(&within, 1, 10));
    gate!(sink, s.name, "paginate(within 保留名+空 where_in)", w, why, e.paginate(&within_empty_in, 1, 10));
    gate!(sink, s.name, "create_index(保留名)", w, why, e.create_index(RESERVED, serde_json::json!({})));
    gate!(sink, s.name, "delete_index(保留名)", w, why, e.delete_index(RESERVED));
    // reindex 两端都要校验：`_all` 在前、在后各成一案
    gate!(sink, s.name, "reindex(保留名, ok)", w_re, why_re, e.reindex(RESERVED, OK_NAME));
    gate!(sink, s.name, "reindex(ok, 保留名)", w_re, why_re, e.reindex(OK_NAME, RESERVED));
    gate!(sink, s.name, "update([doc.index=保留名])", w, why, e.update(std::slice::from_ref(&bad_doc)));
    gate!(sink, s.name, "update_bulk([doc.index=保留名])", w, why, e.update_bulk(&bad_bulk));
    // flush 不带例外：**null 也没覆写它**，用的是 trait 默认实现（`engine.rs:39-44`，
    // 校验后返回），所以「null 永不校验」不能外推到它没覆写的方法上。
    gate!(sink, s.name, "flush(保留名)", Want::Index, "", e.flush(RESERVED));

    // 保留名的其余类别（通配符 / 多索引表达式 / 前导点、下划线、减号 / 空白 / 路径分隔符）：
    // 读路径与写路径各扫一遍，规则面见 validate_index_name。
    for name in BAD_NAMES {
        gate!(sink, s.name, &format!("search(within {name:?})"), w, why, e.search(&SearchBuilder::new("").within(*name)));
        gate!(sink, s.name, &format!("delete_index({name:?})"), w, why, e.delete_index(name));
    }
}

/// G1' —— xunsearch 的项目名与库名都必须在**连接之前**校验。
///
/// 本批次修掉的缺陷：此前库名校验挂在 CMD_USE 握手之后，后端挂着时 `"_all"` 会被报成
/// `XunSearchIo` ——「名字非法」得看后端脸色才知道，而其余七个驱动一律 `InvalidIndexName`。
/// 同一个输入的错误类型不能随后端死活而变。
#[cfg(feature = "xunsearch")]
#[tokio::test]
async fn g1_project_name_is_validated_before_connecting() {
    let mut sink = Sink::new("G1'");
    let e = crate::XunSearchEngine::new(DEAD_TCP, RESERVED, None);
    let (ids, d) = (vec!["x".to_string()], doc("d", OK_NAME));
    let b = SearchBuilder::new("").within(OK_NAME);
    let (i, ok) = (Want::Index, "");
    gate!(sink, "xunsearch(项目名非法)", "search", i, ok, e.search(&b));
    gate!(sink, "xunsearch(项目名非法)", "paginate", i, ok, e.paginate(&b, 1, 10));
    gate!(sink, "xunsearch(项目名非法)", "update", i, ok, e.update(std::slice::from_ref(&d)));
    gate!(sink, "xunsearch(项目名非法)", "update_bulk", i, ok, e.update_bulk(std::slice::from_ref(&d)));
    gate!(sink, "xunsearch(项目名非法)", "delete_in", i, ok, e.delete_in(OK_NAME, &ids));
    gate!(sink, "xunsearch(项目名非法)", "delete", i, ok, e.delete(&ids));
    gate!(sink, "xunsearch(项目名非法)", "flush", i, ok, e.flush(OK_NAME));
    gate!(sink, "xunsearch(项目名非法)", "delete_index", i, ok, e.delete_index(OK_NAME));
    sink.finish();
}

/// G2 —— 空 `where_in` = 不匹配任何，绝不是错误（`Err(Unsupported)` 即分歧）。
///
/// 先写一条**能匹配**的文档：「空集合匹配不到」才有内容 —— 否则「库里本来就没东西」也能
/// 让断言通过。基准语义见 `builder.rs` 的 `matches()`：空集合上 `any()` 恒假。
async fn g2_cases(sink: &mut Sink, s: &Subject) {
    let _ = s.engine.update(&[doc("hit", OK_NAME)]).await; // 网络驱动上会失败，与判据无关
    let b = SearchBuilder::new("").within(OK_NAME).where_in("tag", std::iter::empty::<i32>());
    sink.empty(s.name, "search(空 where_in)", &s.engine.search(&b).await, "");
    sink.empty(s.name, "paginate(空 where_in)", &s.engine.paginate(&b, 1, 10).await, "");
}

/// G3 —— 空 `where_not_in` 不过滤任何东西，也不是错误；**非空** `where_not_in` 在
/// xunsearch 上是已声明例外（README 驱动表：无对应协议命令）。
async fn g3_cases(sink: &mut Sink, s: &Subject) {
    let _ = s.engine.update(&[doc("hit", OK_NAME)]).await;
    // 空 NOT IN：同一条文档必须留得住（total 不变）。null 的契约是永远空结果，对它只能
    // 验 total=0，并按例外记。
    let b = SearchBuilder::new("").within(OK_NAME).where_not_in("tag", std::iter::empty::<i32>());
    let (n, why) = if s.caps.never_validates { (0, WHY_NULL) } else { (1, "") };
    sink.total_is(s.name, "search(空 where_not_in 不过滤)", &s.engine.search(&b).await, n, why);

    // 非空 NOT IN：xunsearch 未实现（例外），其余驱动必须能用（死地址上退化成 I/O）
    let b = SearchBuilder::new("").within(OK_NAME).where_not_in("tag", ["x"]);
    let (w, why) = if s.caps.no_where_not_in { (Want::Unsupported, WHY_XUN_WHERE) } else { (s.ok(), "") };
    gate!(sink, s.name, "search(非空 where_not_in)", w, why, s.engine.search(&b));
}

/// G4 —— `take(0)` 在八个驱动上都不是错误（README：返回空 `hits`，`total` 仍是过滤后的
/// 真实总数）。
///
/// 顺带钉住「默认窗口」这条已声明例外：`take` 不传时 collection / database 返回**全部**
/// 命中，其余六个默认 **10** 条。网络驱动在死地址上不可观测（要捕获桩，属 Tier S）。
async fn g4_cases(sink: &mut Sink, s: &Subject) {
    let docs: Vec<SearchDocument> = (0..12).map(|i| doc(&format!("d{i:02}"), OK_NAME)).collect();
    let _ = s.engine.update_bulk(&docs).await;
    let r = s.engine.search(&SearchBuilder::new("").within(OK_NAME)).await;
    let (n, why) = match (s.caps.never_validates, s.in_process) {
        (true, _) => (0, WHY_NULL),
        (false, true) => (12, WHY_WINDOW_ALL),
        (false, false) => (10, WHY_WINDOW_TEN),
    };
    sink.total_is(s.name, "search(不传 take) 的默认窗口", &r, n, why);

    // take(0)：合法；hits 必须为空，total 仍是过滤后的真实总数。
    let r = s.engine.search(&SearchBuilder::new("").within(OK_NAME).take(0)).await;
    let (n, why) = if s.caps.never_validates { (0, WHY_NULL) } else { (12, "") };
    if s.in_process {
        sink.content(s.name, "search(take(0))", &r, why, |r| {
            (r.total != n || !r.hits.is_empty())
                .then(|| format!("实测 total={} hits={}，期望 total={n} 且 hits 为空", r.total, r.hits.len()))
        });
    } else {
        // 死地址上 total 不可知（那 12 条根本没进后端）：只验可观测的两件事 —— 没提前报错
        // 且 hits 为空。
        sink.content(s.name, "search(take(0))", &r, "", |r| {
            (!r.hits.is_empty()).then(|| format!("实测 hits={}，期望为空", r.hits.len()))
        });
    }
}

/// 软删除三件套的期望：null 永不报错 → xunsearch 未实现（README 驱动表）→ 四个 HTTP 驱动
/// 的不带索引 `soft_delete` 跨索引做不到（`engine.rs:75-91`）→ 其余按通用不变量。`indexless`
/// 只有「不带索引的 soft_delete」这一条判据为真。
fn soft_expect(s: &Subject, indexless: bool) -> (Want, &'static str) {
    if s.caps.never_validates {
        (Want::Ok, WHY_NULL)
    } else if s.caps.no_soft_delete {
        (Want::Unsupported, WHY_XUN_SOFT_DELETE)
    } else if indexless && s.caps.indexless_soft_delete {
        (Want::Unsupported, WHY_INDEXLESS_SOFT_DELETE)
    } else {
        (s.ok(), "")
    }
}

/// G5 —— 软删除三件套在**且仅在** xunsearch 上是 Unsupported；不带索引的 `soft_delete` 在
/// 四个 HTTP 驱动上也是（这是与「仅在 xunsearch」字面措辞的唯一出入，按例外表的规则
/// **引用式声明**，而不是当成未声明分歧，见 `soft_expect`）。
async fn g5_cases(sink: &mut Sink, s: &Subject) {
    let ids = vec!["x".to_string()];
    let (w, why) = soft_expect(s, false);
    gate!(sink, s.name, "soft_delete_in(ok,[id])", w, why, s.engine.soft_delete_in(OK_NAME, &ids));
    let (w, why) = soft_expect(s, true);
    gate!(sink, s.name, "soft_delete([id])", w, why, s.engine.soft_delete(&ids));
    // only_trashed 是 search 侧过滤，四个 HTTP 驱动**支持**它（indexless=false）
    let (w, why) = soft_expect(s, false);
    let b = SearchBuilder::new("").within(OK_NAME).only_trashed();
    gate!(sink, s.name, "search(only_trashed)", w, why, s.engine.search(&b));
}

/// G6 —— 删除是幂等的：删不存在的 id / 索引都是 `Ok`（README 的索引生命周期把
/// `delete_index` 当作清理入口，返回错误等于逼调用方先查存在性）。
///
/// 进程内驱动可观测；网络驱动在死地址上只能确认「没有提前报错」。
async fn g6_cases(sink: &mut Sink, s: &Subject) {
    let missing = vec!["no-such-id".to_string()];
    let (w, why) = if s.caps.never_validates { (Want::Ok, WHY_NULL) } else { (s.ok(), "") };
    gate!(sink, s.name, "delete([不存在的 id])", w, why, s.engine.delete(&missing));
    gate!(sink, s.name, "delete_in(ok,[不存在的 id])", w, why, s.engine.delete_in(OK_NAME, &missing));
    gate!(sink, s.name, "delete_bulk(ok,[不存在的 id])", w, why, s.engine.delete_bulk(OK_NAME, &missing));
    gate!(sink, s.name, "delete_index(不存在的索引)", w, why, s.engine.delete_index("no_such_index"));
    let (w, why) = soft_expect(s, false);
    gate!(sink, s.name, "soft_delete_in(ok,[不存在的 id])", w, why, s.engine.soft_delete_in(OK_NAME, &missing));
}

/// 字段名判据：把字段名拼进表达式的驱动必须拒绝；没有拼接点的驱动只要求「不是
/// InvalidFieldName」（进程内驱动必须 Ok —— 字段名合法与否都只是「这个字段不存在」，
/// 不该报错）。
fn field_expect(s: &Subject) -> (Want, &'static str) {
    if s.caps.never_validates {
        (Want::Ok, WHY_NULL)
    } else if s.caps.concatenates_fields {
        (Want::Field, "")
    } else if s.in_process {
        (Want::Ok, "")
    } else {
        (Want::NotField, "")
    }
}

/// G7 —— 字段名校验是**按驱动的能力**，不是通用规律（`validate_field_name`）。
///
/// 只有把字段名拼进表达式文本的三个驱动（meilisearch / typesense / algolia）必须拒绝：
/// 字段名那里的**值**转了义，字段名本身裸拼，一个空格就够改写整条表达式。其余驱动没有
/// 可被改写的表达式，接受（进程内驱动直接 Ok）。
async fn g7_cases(sink: &mut Sink, s: &Subject) {
    let e = &s.engine;
    let (w, why) = field_expect(s);
    gate!(sink, s.name, "search(where_field 非法字段名)", w, why,
        e.search(&SearchBuilder::new("").within(OK_NAME).where_field(BAD_FIELD, 1)));
    gate!(sink, s.name, "search(where_in 非法字段名)", w, why,
        e.search(&SearchBuilder::new("").within(OK_NAME).where_in(BAD_FIELD, [1])));
    gate!(sink, s.name, "search(where_not_in 非法字段名)", w, why,
        e.search(&SearchBuilder::new("").within(OK_NAME).where_not_in(BAD_FIELD, [1])));
    // order_by：algolia 的排序要预建 replica index，`order_by` 被**忽略**（README 驱动表）
    // —— 字段名没有拼接点，也就没有校验可谈。
    let (w, why) = if s.caps.ignores_order { (s.ok(), WHY_ALGOLIA_ORDER) } else { field_expect(s) };
    gate!(sink, s.name, "search(order_by 非法字段名)", w, why,
        e.search(&SearchBuilder::new("").within(OK_NAME).order_by(BAD_FIELD, false)));
}

/// G8 —— `flush` 在八个驱动上都校验索引名。
///
/// flush 是最像 no-op 的那一个：五个驱动直接用 trait 默认实现（校验后返回），ES 发
/// `_refresh`、xunsearch 发 COMMIT。恰恰因为它「什么都不做」最容易被漏掉校验 —— 而
/// `engine.rs:30-38` 记录了三个驱动曾把它接到**清空索引**的接口上，那时 `flush("_all")`
/// 的后果是删库。所以保留名必须一个不漏地被拒（null 也包含在内：它没覆写 flush）。
async fn g8_cases(sink: &mut Sink, s: &Subject) {
    for name in BAD_NAMES {
        gate!(sink, s.name, &format!("flush({name:?})"), Want::Index, "", s.engine.flush(name));
    }
    // 合法名字的另一半：进程内驱动必须 Ok（网络驱动在死地址上是 I/O，不可观测）。
    if s.in_process {
        gate!(sink, s.name, "flush(合法名)", Want::Ok, "", s.engine.flush(OK_NAME));
    }
}

/// 每个闸门一个 `#[tokio::test]` 入口：建被测对象 → 逐条判据 → 汇总报告（只在 `finish()`
/// 里断言，首跑一次就能看到全部分歧）。
macro_rules! gate_tests {
    ($($name:ident => $gate:literal, $tag:literal, $cases:ident;)*) => {$(
        #[tokio::test]
        async fn $name() {
            let h = Harness::new($tag);
            let mut sink = Sink::new($gate);
            for s in &h.subjects {
                $cases(&mut sink, s).await;
            }
            sink.finish();
        }
    )*};
}

gate_tests! {
    g1_index_validation_precedes_short_circuits_and_io => "G1", "g1", g1_cases;
    g2_empty_where_in_matches_nothing => "G2", "g2", g2_cases;
    g3_empty_where_not_in_filters_nothing => "G3", "g3", g3_cases;
    g4_take_zero_is_not_an_error => "G4", "g4", g4_cases;
    g5_soft_delete_unsupported_only_on_xunsearch => "G5", "g5", g5_cases;
    g6_delete_is_idempotent => "G6", "g6", g6_cases;
    g7_field_name_validation_is_per_driver_capability => "G7", "g7", g7_cases;
    g8_flush_validates_index_name => "G8", "g8", g8_cases;
}
