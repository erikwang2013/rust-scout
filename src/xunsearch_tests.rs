//! XunSearch 引擎集成测试：本地 mock TCP 服务（不依赖真实 xunsearchd）。
//! 引擎约定 host 端口 = index、port+1 = search，`spawn_pair` 按此开监听。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::engine::Engine;
use crate::xunsearch_engine::XunSearchEngine;
use crate::xunsearch_query::*;
use crate::{SearchBuilder, SearchDocument};

/// 解包后的请求帧。
#[derive(Debug, Clone)]
pub struct Frame {
    pub cmd: u8,
    #[allow(dead_code)] // 部分用例只关心 cmd/buf
    pub arg: u16,
    pub buf: Vec<u8>,
    pub buf1: Vec<u8>,
}

/// 读一帧；对端关闭/数据不足 → None。
pub async fn read_frame(stream: &mut TcpStream) -> Option<Frame> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await.ok()?;
    let blen1 = header[3] as usize;
    let blen = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut buf = vec![0u8; blen];
    stream.read_exact(&mut buf).await.ok()?;
    let mut buf1 = vec![0u8; blen1];
    stream.read_exact(&mut buf1).await.ok()?;
    Some(Frame {
        cmd: header[0],
        arg: (u16::from(header[1]) << 8) | u16::from(header[2]),
        buf,
        buf1,
    })
}

/// OK 应答帧（OK 码 arg 为 u16，如 OK_DB_COMMITED=256 → arg1=1、arg2=0）。
pub fn ok_frame(ok_code: u16, buf: &[u8]) -> Vec<u8> {
    pack_cmd(CMD_OK, (ok_code >> 8) as u8, ok_code as u8, buf, &[])
}

/// 开一对监听（search 端口 P、index 端口 P-1），各跑 accept 循环，每连接
/// 起任务跑 handler（读帧→记录→应答）；返回 (engine host, index 帧, search 帧)。
pub async fn spawn_pair<H1, H2, F1, F2>(
    index_handler: H1,
    search_handler: H2,
) -> (String, Arc<Mutex<Vec<Frame>>>, Arc<Mutex<Vec<Frame>>>)
where
    H1: Fn(TcpStream, Arc<Mutex<Vec<Frame>>>) -> F1 + Clone + Send + Sync + 'static,
    F1: Future<Output = ()> + Send + 'static,
    H2: Fn(TcpStream, Arc<Mutex<Vec<Frame>>>) -> F2 + Clone + Send + Sync + 'static,
    F2: Future<Output = ()> + Send + 'static,
{
    let (search_listener, index_listener) = loop {
        let search = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = search.local_addr().unwrap().port();
        match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port - 1))).await {
            Ok(index) => break (search, index),
            Err(_) => continue, // P-1 被占，换一个 search 端口重试
        }
    };
    let search_port = search_listener.local_addr().unwrap().port();
    let index_frames = Arc::new(Mutex::new(Vec::new()));
    let search_frames = Arc::new(Mutex::new(Vec::new()));
    spawn_accept(search_listener, search_handler, search_frames.clone());
    spawn_accept(index_listener, index_handler, index_frames.clone());
    (format!("127.0.0.1:{}", search_port - 1), index_frames, search_frames)
}

fn spawn_accept<H, F>(listener: TcpListener, handler: H, frames: Arc<Mutex<Vec<Frame>>>)
where
    H: Fn(TcpStream, Arc<Mutex<Vec<Frame>>>) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let frames = frames.clone();
            let handler = handler.clone();
            tokio::spawn(async move { handler(sock, frames).await });
        }
    });
}

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
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_SET_DB).count(), 1); // 仅 books 组 SET_DB；无 index 组走服务端默认库
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_REQUEST).count(), 3);
    assert_eq!(frames.iter().filter(|f| f.cmd == CMD_INDEX_SUBMIT).count(), 2); // 两组各一次提交
    let set_db = frames.iter().find(|f| f.cmd == CMD_INDEX_SET_DB).unwrap();
    assert_eq!(set_db.buf, b"books");
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
    assert_eq!(removes[0].buf, b"abc"); // 主键小写化
    assert_eq!(removes[1].buf, b"def");
    assert_eq!(removes[2].buf, b"x");
    assert!(frames.iter().any(|f| f.cmd == CMD_INDEX_SET_DB && f.buf == b"books"));
}

#[tokio::test]
async fn no_index_update_and_search_skip_set_db() {
    let (host, index_frames, search_frames) = spawn_pair(
        |mut sock, frames| async move {
            loop {
                let Some(frame) = read_frame(&mut sock).await else { break };
                let cmd = frame.cmd;
                frames.lock().unwrap().push(frame);
                match cmd {
                    CMD_USE => sock.write_all(&ok_frame(OK_PROJECT, &[])).await.unwrap(),
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
    // 无 index 文档：update 与 search 都不发 SET_DB，读写同一默认库，主路径一致。
    engine
        .update(&[SearchDocument::new("1", serde_json::json!({"title": "a"})).unwrap()])
        .await
        .unwrap();
    let r = engine.search(&SearchBuilder::new("")).await.unwrap();
    assert_eq!(r.total, 0);
    assert!(!index_frames.lock().unwrap().iter().any(|f| f.cmd == CMD_INDEX_SET_DB));
    assert!(!search_frames.lock().unwrap().iter().any(|f| f.cmd == CMD_INDEX_SET_DB));
}
