//! XunSearch 引擎集成测试：本地 mock TCP 服务（不依赖真实 xunsearchd）。
//! 引擎约定 host 端口 = index、port+1 = search，`spawn_pair` 按此开监听。

use std::sync::{Arc, Mutex};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use crate::engine::Engine;
use crate::xunsearch_engine::XunSearchEngine;
use crate::xunsearch_mock::*;
use crate::xunsearch_query::*;
use crate::{SearchBuilder, SearchDocument};

#[tokio::test]
async fn search_round_trip() {
    let (host, _, search_frames) = spawn_pair(
        |_sock, _frames| async move {},
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => {
                        sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap()
                    }
                    CMD_INDEX_SET_DB => {
                        // 搜索连接同号（CMD_SEARCH_SET_DB）；index=None ≡ "default"
                        sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap()
                    }
                    CMD_SEARCH_GET_RESULT => {
                        sock.write_all(&ok_frame(OK_RESULT_BEGIN, &1u32.to_le_bytes()))
                            .await
                            .unwrap();
                        let mut doc = vec![0u8; 20];
                        doc[16..20].copy_from_slice(&3.5f32.to_le_bytes());
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_DOC, 0, 0, &doc, &[]))
                            .await
                            .unwrap();
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 0, b"one", &[]))
                            .await
                            .unwrap();
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, MIXED_VNO, b"hello world", &[]))
                            .await
                            .unwrap();
                        sock.write_all(&ok_frame(OK_RESULT_END, &[])).await.unwrap();
                    }
                    _ => {} // QUERY_INIT/PARSE 等静默命令不回应
                }
            }
        },
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    let result = engine.search(&SearchBuilder::new("hello")).await.unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].id, "one");
    assert_eq!(result.hits[0].score, Some(3.5));
    assert_eq!(result.hits[0].source, serde_json::json!({"body": "hello world"}));
    let frames = search_frames.lock().unwrap();
    assert_eq!(frames[0].cmd, CMD_USE);
    assert_eq!(frames[0].buf, b"books");
    // index=None ≡ "default"：搜索也显式 SET_DB，和 update/delete 读写的库对齐
    let set_db = frames.iter().find(|f| f.cmd == CMD_INDEX_SET_DB).unwrap();
    assert_eq!(set_db.buf, b"default");
    let get = frames.iter().find(|f| f.cmd == CMD_SEARCH_GET_RESULT).unwrap();
    assert_eq!(get.buf, b"hello");
    assert_eq!(get.buf1, [0, 0, 0, 0, 10, 0, 0, 0]); // offset=0 limit=10
}

#[tokio::test]
async fn soft_delete_unsupported() {
    let (host, index_frames, _) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                if frame.cmd == CMD_USE {
                    sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap()
                }
                frames.lock().unwrap().push(frame);
            }
        },
        |_sock, _frames| async move {},
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    // 协议无"单字段更新"命令：软删除走 trait 默认（Unsupported），不做读改写。
    let err = engine.soft_delete(&["abc".to_string()]).await.unwrap_err();
    assert!(matches!(err, crate::ScoutError::Unsupported(_)));
    assert!(index_frames.lock().unwrap().is_empty()); // 未发起任何索引写
}

#[tokio::test]
async fn search_rejects_where_in_and_only_trashed() {
    let (host, index_frames, _) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => {
                        sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap()
                    }
                    CMD_INDEX_SET_DB => {
                        sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap()
                    }
                    CMD_INDEX_SUBMIT => {
                        sock.write_all(&ok_frame(OK_RQST_FINISHED, &[])).await.unwrap()
                    }
                    _ => {}
                }
            }
        },
        // 固定返回两文档：a{id, __soft_deleted=1, title=x}、b{id, title=y}
        |mut sock, _frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                if frame.cmd == CMD_USE {
                    sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap();
                } else if frame.cmd == CMD_INDEX_SET_DB {
                    sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap();
                } else if frame.cmd == CMD_SEARCH_GET_RESULT {
                    sock.write_all(&ok_frame(OK_RESULT_BEGIN, &2u32.to_le_bytes()))
                        .await
                        .unwrap();
                    for (id, deleted, title) in [("a", "1", "x"), ("b", "", "y")] {
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_DOC, 0, 0, &[0u8; 20], &[]))
                            .await
                            .unwrap();
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 0, id.as_bytes(), &[]))
                            .await
                            .unwrap();
                        if !deleted.is_empty() {
                            // vno=1: __soft_deleted（缺省方案字母序先于 title）
                            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 1, deleted.as_bytes(), &[]))
                                .await
                                .unwrap();
                        }
                        // vno=2: title
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 2, title.as_bytes(), &[]))
                            .await
                            .unwrap();
                    }
                    sock.write_all(&ok_frame(OK_RESULT_END, &[])).await.unwrap();
                }
            }
        },
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    engine
        .update(&[
            SearchDocument::new("a", serde_json::json!({"__soft_deleted": "1", "title": "x"})).unwrap(),
            SearchDocument::new("b", serde_json::json!({"title": "y"})).unwrap(),
        ])
        .await
        .unwrap();
    // 协议不支持 IN/NOT-IN 与 only_trashed → 明确 Unsupported（不做内存过滤）。
    let err = engine
        .search(&SearchBuilder::new("").where_in("title", ["x", "y"]))
        .await
        .unwrap_err();
    assert!(matches!(err, crate::ScoutError::Unsupported(_)));
    let err = engine.search(&SearchBuilder::new("").only_trashed()).await.unwrap_err();
    assert!(matches!(err, crate::ScoutError::Unsupported(_)));
    // 默认 Exclude 与 with_trashed 走服务端路径，mock 两文档全可见。
    let r = engine.search(&SearchBuilder::new("")).await.unwrap();
    assert_eq!(r.total, 2);
    assert_eq!(r.ids(), vec!["a", "b"]);
    let r = engine.search(&SearchBuilder::new("").with_trashed()).await.unwrap();
    assert_eq!(r.total, 2);
    assert_eq!(r.ids(), vec!["a", "b"]);
    assert_eq!(index_frames.lock().unwrap().iter().filter(|f| f.cmd == CMD_INDEX_SUBMIT).count(), 1);
}

#[tokio::test]
async fn update_bulk_groups_by_index() {
    let (host, index_frames, _) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => {
                        sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap()
                    }
                    CMD_INDEX_SET_DB => {
                        sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap()
                    }
                    CMD_INDEX_SUBMIT => {
                        sock.write_all(&ok_frame(OK_RQST_FINISHED, &[])).await.unwrap()
                    }
                    _ => {}
                }
            }
        },
        |_sock, _frames| async move {},
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    let mut d1 = SearchDocument::new("1", serde_json::json!({"title": "a"})).unwrap();
    d1.index = Some("books".to_string());
    let mut d2 = SearchDocument::new("2", serde_json::json!({"title": "b"})).unwrap();
    d2.index = Some("books".to_string());
    let d3 = SearchDocument::new("3", serde_json::json!({"title": "c"})).unwrap();
    engine.update_bulk(&[d1, d2, d3]).await.unwrap();
    let frames = index_frames.lock().unwrap();
    // 两组（books / None≡default）各一次 SET_DB：组间串库不再可能
    let set_dbs: Vec<&[u8]> = frames
        .iter()
        .filter(|f| f.cmd == CMD_INDEX_SET_DB)
        .map(|f| f.buf.as_slice())
        .collect();
    assert_eq!(set_dbs, vec![b"books".as_slice(), b"default".as_slice()]);
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_REQUEST).count(), 3);
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_SUBMIT).count(), 2); // 两组各一次提交
}

#[tokio::test]
async fn delete_and_delete_bulk_remove_lowercased_ids() {
    let (host, index_frames, _) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => {
                        sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap()
                    }
                    CMD_INDEX_SET_DB => {
                        sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap()
                    }
                    CMD_INDEX_REMOVE => {
                        sock.write_all(&ok_frame(OK_RQST_FINISHED, &[])).await.unwrap()
                    }
                    _ => {}
                }
            }
        },
        |_sock, _frames| async move {},
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    engine.delete(&["AbC".to_string(), "def".to_string()]).await.unwrap();
    engine.delete_bulk("books", &["x".to_string()]).await.unwrap();
    let frames = index_frames.lock().unwrap();
    let removes: Vec<&Frame> = frames.iter().filter(|f| f.cmd == CMD_INDEX_REMOVE).collect();
    assert_eq!(removes.len(), 3);
    // 小写化的是**删除的查找 term**（xapian term 一律小写，不小写就删不掉）；
    // 文档里存的值槽保持原样，见 xunsearch_query::doc_commands_keep_id_casing_*
    assert_eq!(removes[0].buf, b"abc");
    assert_eq!(removes[1].buf, b"def");
    assert_eq!(removes[2].buf, b"x");
    assert!(frames.iter().any(|f| f.cmd == CMD_INDEX_SET_DB && f.buf == b"books"));
}

#[tokio::test]
async fn no_index_means_default_db_on_both_write_and_search() {
    // 回归：这里原先断言「无 index 就完全不发 SET_DB」，把 None≠default 的分裂锁死。
    // 那与其余七个驱动不一致：`update([index Some("default")])` 配
    // `search(无 index)`（或反过来）两边读写的库不同，各自都搜不到。现在
    // None ≡ "default"，写与读都显式 SET_DB 到 default。
    let (host, index_frames, search_frames) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap(),
                    CMD_INDEX_SET_DB => sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap(),
                    CMD_INDEX_SUBMIT => sock.write_all(&ok_frame(OK_RQST_FINISHED, &[])).await.unwrap(),
                    _ => {}
                }
            }
        },
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap(),
                    CMD_INDEX_SET_DB => sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap(),
                    CMD_SEARCH_GET_RESULT => {
                        sock.write_all(&ok_frame(OK_RESULT_BEGIN, &0u32.to_le_bytes())).await.unwrap();
                        sock.write_all(&ok_frame(OK_RESULT_END, &[])).await.unwrap();
                    }
                    _ => {}
                }
            }
        },
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    engine
        .update(&[SearchDocument::new("1", serde_json::json!({"title": "a"})).unwrap()])
        .await
        .unwrap();
    let r = engine.search(&SearchBuilder::new("")).await.unwrap();
    assert_eq!(r.total, 0);
    let set_dbs = |frames: &Arc<Mutex<Vec<Frame>>>| -> Vec<Vec<u8>> {
        frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.cmd == CMD_INDEX_SET_DB)
            .map(|f| f.buf.clone())
            .collect()
    };
    assert_eq!(set_dbs(&index_frames), vec![b"default".to_vec()]);
    assert_eq!(set_dbs(&search_frames), vec![b"default".to_vec()]);
}

#[tokio::test]
async fn io_failures_keep_their_error_kind() {
    // 原先 with_timeout 把所有 io::Error 塞进 XunSearch(String)：ErrorKind 和
    // source() 都没了，调用方分不清「连接被拒」和「服务端报错」。现在保留 XunSearchIo。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // 端口随即关闭：connect 应得 ConnectionRefused
    let engine = XunSearchEngine::new(&format!("127.0.0.1:{port}"), "proj", None);
    let err = engine.search(&SearchBuilder::new("q")).await.unwrap_err();
    assert!(
        matches!(err, crate::ScoutError::XunSearchIo(ref e) if e.kind() == std::io::ErrorKind::ConnectionRefused),
        "got {err:?}"
    );
}

#[tokio::test]
async fn search_results_are_bounded_by_the_requested_limit() {
    // 回归：结果循环原先只认 OK_RESULT_END，对端一直发 CMD_SEARCH_RESULT_DOC
    // （8 字节零长度包也照样 push 一条）就能把 hits 撑到 OOM。take 缺省 10，
    // 第 11 条必须报错 —— 静默截断会被读成「本来就这么多结果」。
    let (host, _, _) = spawn_pair(
        |_sock, _frames| async move {},
        |mut sock, _frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                match frame.cmd {
                    CMD_USE => sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap(),
                    CMD_INDEX_SET_DB => sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap(),
                    CMD_SEARCH_GET_RESULT => {
                        // 谎报 total=1 之后狂发 DOC 帧（buf 为空：blen=0，weight 缺省 0.0）
                        sock.write_all(&ok_frame(OK_RESULT_BEGIN, &1u32.to_le_bytes())).await.unwrap();
                        for _ in 0..64 {
                            if sock
                                .write_all(&pack_cmd(CMD_SEARCH_RESULT_DOC, 0, 0, &[], &[]))
                                .await
                                .is_err()
                            {
                                return; // 客户端报错关连接
                            }
                        }
                    }
                    _ => {}
                }
            }
        },
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    let err = engine.search(&SearchBuilder::new("hello")).await.unwrap_err();
    // 上限跟着请求窗口走（take 缺省 10），不是某个写死的常数
    assert!(
        matches!(err, crate::ScoutError::XunSearch(ref m) if m.contains("accepted 10")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn unmapped_result_vno_errors_instead_of_dropping_the_value() {
    // vno↔名字是客户端约定（服务端不交换方案）：没有映射就说明写入端用了另一份
    // 方案，读回的名字会是错的。原先直接丢弃该值并返回 Ok，调用方只看到「字段是空的」。
    let (host, _, _) = spawn_pair(
        |_sock, _frames| async move {},
        |mut sock, _frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                match frame.cmd {
                    CMD_USE => sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap(),
                    CMD_INDEX_SET_DB => sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap(),
                    CMD_SEARCH_GET_RESULT => {
                        sock.write_all(&ok_frame(OK_RESULT_BEGIN, &1u32.to_le_bytes())).await.unwrap();
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_DOC, 0, 0, &[0u8; 20], &[]))
                            .await
                            .unwrap();
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 0, b"one", &[]))
                            .await
                            .unwrap();
                        // vno=7：缺省动态方案里从没分配过（本连接也没 update 过）
                        sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 7, b"mystery", &[]))
                            .await
                            .unwrap();
                        let _ = sock.write_all(&ok_frame(OK_RESULT_END, &[])).await;
                    }
                    _ => {}
                }
            }
        },
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    let err = engine.search(&SearchBuilder::new("hello")).await.unwrap_err();
    assert!(
        matches!(err, crate::ScoutError::XunSearch(ref m) if m.contains("vno 7")),
        "未映射的 vno 必须报错，得到 {err:?}"
    );
}

#[tokio::test]
async fn delete_bulk_uses_one_connection() {
    // trait 默认实现是逐 id 调 delete_in（每 id 一次连接 + 握手）；这里一批
    // 3 个 id 应当只有一次 CMD_USE。
    let (host, index_frames, _) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap(),
                    CMD_INDEX_SET_DB => sock.write_all(&ok_frame(OK_DB_CHANGED, &[])).await.unwrap(),
                    CMD_INDEX_REMOVE => sock.write_all(&ok_frame(OK_RQST_FINISHED, &[])).await.unwrap(),
                    _ => {}
                }
            }
        },
        |_sock, _frames| async move {},
    )
    .await;
    let engine = XunSearchEngine::new(&host, "books", None);
    let ids: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
    engine.delete_bulk("books", &ids).await.unwrap();
    let frames = index_frames.lock().unwrap();
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_USE).count(), 1);
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_REMOVE).count(), 3);
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_SET_DB).count(), 1);
}
