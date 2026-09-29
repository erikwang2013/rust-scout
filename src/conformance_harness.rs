//! 跨驱动一致性套件的**装置**（Tier G 的机械部分）。
//!
//! 与 `conformance_tests.rs` 分家：那边放内容（闸门断言、例外表及每条例外的出处），
//! 这边放工具（观测分类、记录器、被测对象清单）。分开的理由是两者演进方向不同 ——
//! 判据随契约走，脚手架不动；改一条断言不该翻过两百行装置。
//!
//! 装置与闸门之间只传四样东西：`Observed`（这次调用落进了哪一类）、`Want`（契约要求
//! 它落进哪一类）、`Sink`（记下来，跑完一次性判）、`Subject`（被测的那个驱动）。
//! 判据永远比较**类别**，不比错误文案。

use std::collections::BTreeMap;

use crate::engine::Engine;
use crate::{Result, ScoutError, SearchDocument, SearchResult};

/// 死地址：连不上是**预期**的，闸门要看的正是「连不上之前发生了什么」。
#[cfg(any(feature = "elasticsearch", feature = "meilisearch", feature = "typesense"))]
pub(crate) const DEAD_HTTP: &str = "http://127.0.0.1:1";
#[cfg(feature = "xunsearch")]
pub(crate) const DEAD_TCP: &str = "127.0.0.1:1";

/// 一次调用落进的类别。闸门只看类别：错误文案会变，契约不会。
///
/// `Io` 的构造点全部按 feature 门控（只有网络驱动产出它），默认构建（仅 collection）
/// 下这个变体永不构造 —— 显式放行，别让死代码告警淹了真正的告警。
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observed {
    Ok,
    InvalidIndexName,
    InvalidFieldName,
    Unsupported,
    /// 网络 / 后端 / 数据库失败：校验没拦住，调用已经打出去了。
    Io,
    Other(String),
}

pub(crate) fn inspect<T>(r: &Result<T>) -> Observed {
    match r {
        Ok(_) => Observed::Ok,
        Err(ScoutError::InvalidIndexName(_)) => Observed::InvalidIndexName,
        Err(ScoutError::InvalidFieldName(_)) => Observed::InvalidFieldName,
        Err(ScoutError::Unsupported(_)) => Observed::Unsupported,
        #[cfg(any(feature = "elasticsearch", feature = "meilisearch", feature = "typesense", feature = "algolia"))]
        Err(ScoutError::Http(_)) => Observed::Io,
        #[cfg(feature = "xunsearch")]
        Err(ScoutError::XunSearch(_) | ScoutError::XunSearchIo(_)) => Observed::Io,
        #[cfg(any(feature = "elasticsearch", feature = "meilisearch", feature = "typesense", feature = "algolia", feature = "xunsearch"))]
        Err(ScoutError::Backend(_)) => Observed::Io,
        #[cfg(feature = "database")]
        Err(ScoutError::Sqlx(_)) => Observed::Io,
        Err(other) => Observed::Other(format!("{other:?}")),
    }
}

/// 判据期望。`OkOrIo` 是网络驱动在死地址上的正常形态：断言只能证伪（不得报
/// Unsupported / InvalidIndexName），证不了后端侧语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    Index,
    Field,
    NotField,
    Unsupported,
    Ok,
    OkOrIo,
}

impl Want {
    fn accepts(self, o: &Observed) -> bool {
        match self {
            Want::Index => *o == Observed::InvalidIndexName,
            Want::Field => *o == Observed::InvalidFieldName,
            Want::NotField => *o != Observed::InvalidFieldName,
            Want::Unsupported => *o == Observed::Unsupported,
            Want::Ok => *o == Observed::Ok,
            Want::OkOrIo => matches!(o, Observed::Ok | Observed::Io),
        }
    }

    fn text(self) -> &'static str {
        match self {
            Want::Index => "InvalidIndexName", Want::Field => "InvalidFieldName",
            Want::NotField => "非 InvalidFieldName", Want::Unsupported => "Unsupported（已声明例外）",
            Want::Ok => "Ok", Want::OkOrIo => "Ok 或 I/O",
        }
    }
}

/// 一次闸门运行的记录器：四个桶就是报告的四段 —— 满足 / 已声明例外 / 不可观测 / 分歧。
#[derive(Default)]
pub(crate) struct Sink {
    gate: &'static str,
    total: usize,
    satisfied: BTreeMap<&'static str, usize>,
    exceptions: Vec<String>,
    unobservable: Vec<String>,
    divergences: Vec<String>,
}

impl Sink {
    pub(crate) fn new(gate: &'static str) -> Self {
        Self { gate, ..Default::default() }
    }

    /// 一条类别判据。`why` 非空 = 该 (驱动, 操作) 是**已声明**的例外，值是引用出处。
    pub(crate) fn push(&mut self, driver: &'static str, what: &str, want: Want, obs: Observed, why: &str) {
        self.total += 1;
        let line = format!("{driver} / {what}: {obs:?}");
        if !want.accepts(&obs) {
            self.divergences.push(format!("{line} —— 期望 {}", want.text()));
        } else if !why.is_empty() {
            self.exceptions.push(format!("{line} —— {why}"));
        } else if obs == Observed::Io {
            // 判据成立，但靠的是「没拦住」：网络驱动在死地址上打不到后端。
            self.unobservable.push(line);
        } else {
            *self.satisfied.entry(driver).or_insert(0) += 1;
        }
    }

    /// 内容判据：Ok 时交给 `bad` 检查（`Some(说明)` = 不满足）；I/O 记为不可观测；
    /// 其余类别一律分歧（这几条闸门要的正是「别报 Unsupported」）。
    pub(crate) fn content(&mut self, driver: &'static str, what: &str, res: &Result<SearchResult>, why: &str,
               bad: impl FnOnce(&SearchResult) -> Option<String>) {
        self.total += 1;
        match res {
            Ok(r) => match bad(r) {
                None if why.is_empty() => *self.satisfied.entry(driver).or_insert(0) += 1,
                None => self.exceptions.push(format!("{driver} / {what}: Ok —— {why}")),
                Some(msg) => self.divergences.push(format!("{driver} / {what}: {msg}")),
            },
            Err(e) if inspect(res) == Observed::Io => self.unobservable.push(format!("{driver} / {what}: {e}")),
            Err(e) => self.divergences.push(format!("{driver} / {what}: {e} —— 期望 Ok，实测非 Ok")),
        }
    }

    /// 「匹配不到任何东西」：Ok 必须 total=0 且无命中。
    pub(crate) fn empty(&mut self, driver: &'static str, what: &str, res: &Result<SearchResult>, why: &str) {
        self.content(driver, what, res, why, |r| {
            (r.total != 0 || !r.hits.is_empty())
                .then(|| format!("期望空结果，实测 total={} hits={}", r.total, r.hits.len()))
        });
    }

    /// 「过滤掉 0 条」：Ok 的 total 必须原样是 `want_total`。
    pub(crate) fn total_is(&mut self, driver: &'static str, what: &str, res: &Result<SearchResult>, want_total: usize, why: &str) {
        self.content(driver, what, res, why, |r| {
            (r.total != want_total).then(|| format!("实测 total={}，期望 {want_total}", r.total))
        });
    }

    pub(crate) fn finish(self) {
        let per_driver: Vec<String> = self.satisfied.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("[{}] 判据 {} 条 | 满足 [{}] | 已声明例外 {} | 不可观测 {} | 分歧 {}", self.gate, self.total,
                 per_driver.join(" "), self.exceptions.len(), self.unobservable.len(), self.divergences.len());
        for (label, lines) in [("例外", &self.exceptions), ("不可观测", &self.unobservable), ("分歧", &self.divergences)] {
            for line in lines {
                println!("  [{label}] {line}");
            }
        }
        assert!(self.divergences.is_empty(), "[{}] {} 处未声明分歧：\n{}", self.gate,
                self.divergences.len(), self.divergences.join("\n"));
    }
}

/// 驱动能力：只放「会让判据分叉」的几项，每项都指得出出处（README 驱动表 / `engine.rs`
/// 的 trait 默认实现）。通用契约不在这里 —— 那才是本套件要钉的东西。字段顺序即分支顺序。
#[derive(Default, Clone, Copy)]
pub(crate) struct Caps {
    /// 未实现 `reindex`（`engine.rs:101-116` 的默认实现）
    pub(crate) no_reindex: bool,
    /// 未实现 `soft_delete_in`（`engine.rs:92-100` 的默认实现）
    pub(crate) no_soft_delete_in: bool,
    /// 未实现软删除全家桶：`soft_delete` / `soft_delete_in` / `only_trashed`
    pub(crate) no_soft_delete: bool,
    /// 不带索引的 `soft_delete` 跨索引做不到（`engine.rs:75-91`）
    pub(crate) indexless_soft_delete: bool,
    /// 非空 `where_not_in` 无协议命令
    pub(crate) no_where_not_in: bool,
    /// 字段名参与表达式拼接 → 必须校验字段名
    pub(crate) concatenates_fields: bool,
    /// `order_by` 被忽略（没有拼接点，也就没有校验）
    pub(crate) ignores_order: bool,
    /// 永不报错、永远空结果（null，且**仅限它覆写过的方法**）
    pub(crate) never_validates: bool,
}

pub(crate) struct Subject {
    pub(crate) name: &'static str,
    pub(crate) engine: Box<dyn Engine>,
    /// 进程内驱动：结果（total/hits）可观测；网络驱动在死地址上只能观测到「I/O 之前
    /// 发生了什么」。
    pub(crate) in_process: bool,
    pub(crate) caps: Caps,
}

impl Subject {
    pub(crate) fn new(name: &'static str, engine: impl Engine + 'static, in_process: bool, caps: Caps) -> Self {
        Self { name, engine: Box::new(engine), in_process, caps }
    }

    /// 该驱动的「不报错」期望：进程内必须 Ok，网络驱动退化成 Ok 或 I/O。
    pub(crate) fn ok(&self) -> Want {
        if self.in_process { Want::Ok } else { Want::OkOrIo }
    }
}

/// 建一组被测对象。每个测试各建一份（数据库用独立临时文件，互不干扰）。
pub(crate) struct Harness {
    pub(crate) subjects: Vec<Subject>,
    #[cfg(feature = "database")]
    db_files: Vec<std::path::PathBuf>,
}

impl Harness {
    /// `_tag` 只用来给临时 sqlite 文件起名（关掉 database feature 时用不上）。
    pub(crate) fn new(_tag: &str) -> Self {
        // 关闭全部其余 feature 时只剩 collection 一个对象，`mut` 会显得多余。
        #[allow(unused_mut)]
        let mut subjects: Vec<Subject> = vec![Subject::new("collection", crate::CollectionEngine::new(), true, Caps::default())];

        #[cfg(feature = "null")]
        subjects.push(Subject::new("null", crate::NullEngine::new(), true, Caps { never_validates: true, ..Caps::default() }));

        #[cfg(feature = "database")]
        let mut db_files = Vec::new();
        #[cfg(feature = "database")]
        {
            // 临时文件而不是 `:memory:`：`DatabaseEngine::new` 走默认池（可能开多条连接），
            // 而 `:memory:` 每条连接一个独立库 —— 写在 A 连接、读在 B 连接会看不到，断言
            // 就成了掷骰子。
            let path = temp_db_path(_tag);
            let url = format!("sqlite://{}?mode=rwc", path.display());
            let engine = crate::DatabaseEngine::new(&url, Vec::new()).expect("临时 sqlite 路径");
            subjects.push(Subject::new("database", engine, true, Caps::default()));
            db_files.push(path);
        }

        // 四个 HTTP 驱动：死地址 = 连接被拒。algolia 的 host 由 app_id 拼出
        // （`https://{app_id}.algolia.net`），指不到 127.0.0.1 —— 用不存在的 app_id 让 DNS
        // 立刻 NXDOMAIN，同样是「请求必失败，且失败在 I/O 层」。
        #[cfg(feature = "elasticsearch")]
        subjects.push(Subject::new("elasticsearch", crate::ElasticsearchEngine::new(DEAD_HTTP.to_string(), None), false,
            Caps { indexless_soft_delete: true, ..Caps::default() }));
        #[cfg(feature = "meilisearch")]
        subjects.push(Subject::new("meilisearch", crate::MeilisearchEngine::new(DEAD_HTTP.to_string(), Some("k".to_string())), false,
            Caps { no_reindex: true, indexless_soft_delete: true, concatenates_fields: true, ..Caps::default() }));
        #[cfg(feature = "typesense")]
        subjects.push(Subject::new("typesense", crate::TypesenseEngine::new(DEAD_HTTP.to_string(), Some("k".to_string())), false,
            Caps { no_reindex: true, indexless_soft_delete: true, concatenates_fields: true, ..Caps::default() }));
        #[cfg(feature = "algolia")]
        subjects.push(Subject::new("algolia", crate::AlgoliaEngine::new("scoutconformance".to_string(), "k".to_string()), false,
            Caps { indexless_soft_delete: true, concatenates_fields: true, ignores_order: true, ..Caps::default() }));
        #[cfg(feature = "xunsearch")]
        subjects.push(Subject::new("xunsearch", crate::XunSearchEngine::new(DEAD_TCP, "scout", None), false,
            Caps { no_reindex: true, no_soft_delete_in: true, no_soft_delete: true, no_where_not_in: true, ..Caps::default() }));

        Self { subjects, #[cfg(feature = "database")] db_files }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        #[cfg(feature = "database")]
        for path in &self.db_files {
            // SQLite 正常收尾只留主文件；journal/wal 是异常路径的残留，一并清掉。
            let base = path.to_string_lossy().into_owned();
            for suffix in ["", "-journal", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{base}{suffix}"));
            }
        }
    }
}

#[cfg(feature = "database")]
fn temp_db_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!("rust_scout_conformance_{}_{}_{}.db",
        std::process::id(), tag, N.fetch_add(1, Ordering::Relaxed)))
}

/// 一条写入文档。索引名必须显式给出（`None` ≡ `"default"`）。
pub(crate) fn doc(id: &str, index: &str) -> SearchDocument {
    let mut d = SearchDocument::new(id, serde_json::json!({"tag": "x"})).expect("字段必须是 JSON 对象");
    d.index = Some(index.to_string());
    d
}
