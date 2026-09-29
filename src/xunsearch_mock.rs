//! XunSearch 测试共用的本地 mock TCP 服务：帧读取、OK 应答与 accept 循环，
//! 不依赖真实 xunsearchd。引擎约定 host 端口 = index、port+1 = search，
//! `spawn_pair` 按此开监听。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use crate::xunsearch_query::{pack_cmd, CMD_OK};

/// 解包后的请求帧。
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub(crate) cmd: u8,
    #[allow(dead_code)] // 部分用例只关心 cmd/buf
    pub(crate) arg: u16,
    pub(crate) buf: Vec<u8>,
    pub(crate) buf1: Vec<u8>,
}

/// 读一帧；对端关闭/数据不足 → None。
pub(crate) async fn read_frame(stream: &mut TcpStream) -> Option<Frame> {
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
pub(crate) fn ok_frame(ok_code: u16, buf: &[u8]) -> Vec<u8> {
    pack_cmd(CMD_OK, (ok_code >> 8) as u8, ok_code as u8, buf, &[])
}

/// 开一对监听（search 端口 P、index 端口 P-1），各跑 accept 循环，每连接
/// 起任务跑 handler（读帧→记录→应答）；返回 (engine host, index 帧, search 帧)。
pub(crate) async fn spawn_pair<H1, H2, F1, F2>(
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
