use crate::slab::Slab;
use std::clone::Clone;
use std::collections::LinkedList;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::channel;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::sync::RwLock;

#[derive(Debug)]
struct Session {
    // 正使用吗
    using: Arc<AtomicBool>,
    // 外部连接
    outcons: [Arc<Mutex<Outcon>>; bncom::_const::SESSION_CAP],
    // 下一连接位置
    next: Mutex<usize>,
    // port
    port: u16,
}

impl Session {
    fn new(port: u16) -> Session {
        let mut vc: Vec<Arc<Mutex<Outcon>>> = Vec::with_capacity(bncom::_const::SESSION_CAP);
        for _ in 0..bncom::_const::SESSION_CAP {
            vc.push(Arc::new(Mutex::new(Outcon::new())));
        }
        let vca: [Arc<Mutex<Outcon>>; bncom::_const::SESSION_CAP] = vc.try_into().unwrap();
        Session {
            using: Arc::new(AtomicBool::new(true)),
            outcons: vca,
            next: Mutex::new(0),
            port,
        }
    }
    async fn push(&self, conn: TcpStream) -> Option<usize> {
        let mut count = 0;
        loop {
            let mut next = self.next.lock().await;
            let key = (*next) & bncom::_const::SESSION_CAP_MASK;
            *next += 1;
            let mut u = self.outcons[key].lock().await;
            let start = SystemTime::now();
            let since_the_epoch = start.duration_since(UNIX_EPOCH).unwrap();
            let cur = since_the_epoch.as_secs();
            if u.conn.is_some() {
                // 存在连接，超时判断
                // 10秒超时
                if cur - u.start > 10 {
                    let oldconn = u.conn.take();
                    if let Some(mut c) = oldconn {
                        c.shutdown().await.unwrap();
                    }
                    u.conn = Some(conn);
                    u.start = cur;
                    return Some(key);
                }
            } else {
                u.conn = Some(conn);
                u.start = cur;
                return Some(key);
            }
            count += 1;
            if count >= bncom::_const::SESSION_CAP {
                return None;
            }
        }
    }
    // 关闭Session
    async fn close(sf: &Session) {
        sf.using.store(false, Ordering::Relaxed);
        let _ = match TcpStream::connect(format!("127.0.0.1:{}", sf.port)).await {
            Ok(secon) => secon,
            Err(_) => {
                return;
            }
        };
        // 释放资源
        for i in 0..bncom::_const::SESSION_CAP {
            let aoc = Arc::clone(&sf.outcons[i]);
            let mut v = aoc.lock().await;
            v.start = u64::MAX;
            v.conn = None;
        }
    }
}
#[derive(Debug)]
struct Outcon {
    conn: Option<TcpStream>,
    start: u64,
}

impl Outcon {
    fn new() -> Outcon {
        Outcon {
            conn: None,
            start: u64::MAX,
        }
    }
}

async fn work_for_client(
    sdr: Sender<[u8; 4]>,
    sid: u16,
    son_session: Arc<Session>,
    listener: TcpListener,
) {
    let us = Arc::clone(&son_session.using);
    println!("Bind {}", son_session.port);
    let p1 = (sid >> 8) as u8;
    let p2 = (sid & 0xff) as u8;

    loop {
        let apt = listener.accept().await;
        if us.load(Ordering::Relaxed) == false {
            println!("Port break {}", son_session.port);
            return;
        }
        match apt {
            Ok((stream, _)) => {
                // 通知客户端新连接到了
                if let Some(i) = son_session.push(stream).await {
                    sdr.send([bncom::_const::NEWSOCKET, p1, p2, i as u8])
                        .await
                        .unwrap();
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 正常
                continue;
            }
            Err(e) => {
                println!("error {}", e);
                return;
            }
        }
    }
}

// 关闭连接清理资源
async fn close_session(ids: Vec<usize>, sessions: Arc<RwLock<Slab<Arc<Session>>>>) {
    for id in ids {
        let mut seo: Option<Arc<Session>> = None;
        {
            let mut sew = sessions.write().await;
            seo = sew.remove(id);
        }
        match seo {
            Some(sec) => {
                Session::close(&sec).await;
            }
            None => {}
        }
    }
}

async fn do_start(
    session: Arc<RwLock<Slab<Arc<Session>>>>,
    mut stream: TcpStream,
    cfg: bncom::config::Server,
) {
    // 初始化
    // START info_len info
    let mut cmd = [0u8; 8];
    if let Err(e) = stream.read_exact(&mut cmd).await {
        println!("error {}", e);
        return;
    }
    let info_len: u64 = ((cmd[0] as u64) << 56)
        | ((cmd[1] as u64) << 48)
        | ((cmd[2] as u64) << 40)
        | ((cmd[3] as u64) << 32)
        | ((cmd[4] as u64) << 24)
        | ((cmd[5] as u64) << 16)
        | ((cmd[6] as u64) << 8)
        | (cmd[7] as u64);
    if info_len > 1024 * 1024 {
        // 限制消息最大内存使用量 1M
        if let Err(e) = stream.write_all(&[bncom::_const::ERROR]).await {
            println!("error {}", e);
        }
        return;
    }
    let mut cmdv = vec![0u8; info_len as usize];
    if let Err(e) = stream.read_exact(&mut cmdv).await {
        println!("error {}", e);
        return;
    }
    let p: bncom::config::Client;
    match serde_json::from_slice::<bncom::config::Client>(&cmdv) {
        Ok(v) => {
            p = v;
        }
        Err(e) => {
            println!("error {}", e);
            return;
        }
    }
    if p.map.len() == 0 {
        if let Err(e) = stream.write_all(&[bncom::_const::ERROR]).await {
            println!("error {}", e);
        }
        return;
    }
    if p.key != cfg.key {
        println!("Password error => {}", p.key);
        if let Err(e) = stream.write_all(&[bncom::_const::ERROR_PWD]).await {
            println!("error {}", e);
            return;
        }
        return;
    }
    // 检车端口是否被占用
    // 检查端口是否在规定范围
    let mut listeners: LinkedList<TcpListener> = LinkedList::new();
    for v in &p.map {
        match cfg._limit_port {
            Some((st, ed)) => {
                if v.outer > ed || v.outer < st {
                    // 端口超范围
                    if let Err(e) = stream.write_all(&[bncom::_const::ERROR_LIMIT_PORT]).await {
                        println!("error {}", e);
                        return;
                    }
                    return;
                }
            }
            None => {}
        }
        match TcpListener::bind(format!("0.0.0.0:{}", v.outer)).await {
            Ok(listener) => {
                listeners.push_back(listener);
            }
            Err(_) => {
                // 端口被占用
                if let Err(e) = stream.write_all(&[bncom::_const::ERROR_BUSY]).await {
                    println!("error {}", e);
                    return;
                }
                return;
            }
        }
    }
    let (s, mut r): (Sender<[u8; 4]>, Receiver<[u8; 4]>) = channel(100);
    // 客户端存入session
    // 成功建立连接时的响应
    let mut _success: Vec<u8> = Vec::with_capacity(1 + p.map.len() * 2);
    let mut ids: Vec<usize> = Vec::with_capacity(p.map.len());

    _success.push(bncom::_const::SUCCESS);
    // 客户端存入session
    for v in &p.map {
        let sec = Arc::new(Session::new(v.outer));
        let mut sew = session.write().await;
        let mut sid: u16 = 0;
        if let Some(id) = sew.push(Arc::clone(&sec)) {
            sid = id as u16;
            ids.push(id);
            _success.push(((id & 0xffff) > 8) as u8);
            _success.push((id & 0xff) as u8);
        } else {
            // Session初始化失败
            if let Err(e) = stream.write_all(&[bncom::_const::ERROR_SESSION_OVER]).await {
                println!("error {}", e);
                close_session(ids, Arc::clone(&session)).await;
                return;
            }
        }

        let listener = listeners.pop_front().unwrap();
        let sd = s.clone();
        tokio::spawn(async move {
            work_for_client(sd, sid, sec, listener).await;
        });
    }

    if let Err(e) = stream.write_all(&_success).await {
        println!("error {}", e);
        close_session(ids, session).await;
        return;
    }
    let (mut rs, mut ws) = stream.split();

    let asw = async {
        loop {
            let cmd = r.recv().await.unwrap();
            if cmd[0] == bncom::_const::KILL {
                r.close();
                break;
            }
            let r = ws.write_all(&cmd).await;
            if let Err(_) = r {
                break;
            }
        }
    };
    let asr = async {
        let mut cmd = [0u8];
        loop {
            match rs.read(&mut cmd).await {
                Ok(0) => {
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    break;
                }
            }
        }
        s.send([bncom::_const::KILL, 0, 0, 0]).await.unwrap();
    };

    tokio::join!(asw, asr);
    // 关闭连接清理资源
    close_session(ids, session).await;
}

async fn do_newconn(session: Arc<RwLock<Slab<Arc<Session>>>>, mut stream: TcpStream) {
    let mut cmd = [0u8; 3];
    if let Err(e) = stream.read_exact(&mut cmd).await {
        println!("error {}", e);
        return;
    }

    let id = ((cmd[0] as usize) << 8) | (cmd[1] as usize);
    let son_index = cmd[2] as usize;
    if son_index >= bncom::_const::SESSION_CAP {
        // 下标越界
        return;
    }
    let mut ocon: Option<TcpStream> = None;
    {
        let ser = session.read().await;
        let seo = ser.get(id);
        if let Some(se) = seo {
            let outc = &se.outcons[son_index];
            let mut oc = outc.lock().await;
            ocon = oc.conn.take();
        }
    }
    if let Some(mut con) = ocon {
        let (mut outr, mut outw) = stream.split();
        let (mut inr, mut inw) = con.split();

        let t1 = async {
            match tokio::io::copy(&mut outr, &mut inw).await {
                Ok(_) => {}
                Err(_) => {}
            }
            inw.shutdown().await.unwrap();
        };
        let t2 = async {
            match tokio::io::copy(&mut inr, &mut outw).await {
                Ok(_) => {}
                Err(_) => {}
            }
            outw.shutdown().await.unwrap();
        };
        tokio::join!(t1, t2);
    }
}

async fn new_socket(
    session: Arc<RwLock<Slab<Arc<Session>>>>,
    mut stream: TcpStream,
    cfg: bncom::config::Server,
) {
    let mut cmd = [0u8];
    if let Err(e) = stream.read_exact(&mut cmd).await {
        println!("error {}", e);
        return;
    }
    match cmd[0] {
        bncom::_const::START => {
            do_start(session, stream, cfg).await;
        }
        bncom::_const::NEWCONN => {
            do_newconn(session, stream).await;
        }
        _ => {}
    }
}

pub async fn handle_server(server: bncom::config::Server) {
    println!("server start->{:#?}", server);
    let mut sessions: Slab<Arc<Session>> = Slab::new();
    sessions.set_limit_len(bncom::_const::SESSION_MAX);
    let arc_sessions = Arc::new(RwLock::new(sessions));
    let listener = TcpListener::bind(format!("0.0.0.0:{}", server.port))
        .await
        .unwrap();
    while let Ok((stream, _)) = listener.accept().await {
        let se = Arc::clone(&arc_sessions);
        let cfg = server.clone();
        tokio::spawn(async move {
            new_socket(se, stream, cfg).await;
        });
    }
}
