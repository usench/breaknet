use std::collections::HashMap;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

async fn work_for_server(svr: &str, inner: &str, port_session: &[u8]) {
    // 连内网
    let conn = TcpStream::connect(inner).await;
    match conn {
        Ok(mut incon) => {
            // 发外网指令
            let conn = TcpStream::connect(svr).await;
            match conn {
                Ok(mut outcon) => {
                    // swap
                    if let Err(e) = outcon.write_all(port_session).await {
                        println!("error {}", e);
                        return;
                    }

                    let (mut outr, mut outw) = outcon.split();
                    let (mut inr, mut inw) = incon.split();

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
                Err(e) => {
                    println!("error {}", e);
                }
            }
        }
        Err(e) => {
            println!("error {}", e);
        }
    }
}

pub async fn ever_handle(client: bncom::config::Client) {
    let conn = TcpStream::connect(&client.server).await;
    match conn {
        Ok(mut con) => {
            // 发送START指令
            let sjson = client.clone();
            let json_str = serde_json::to_string(&sjson).expect("Json error");
            let info = json_str.as_bytes();
            let info_len = info.len() as u64;
            if info_len > 1024 * 1024 {
                // 限制消息最大内存使用量 1M
                panic!("Config over 1024 * 1024");
            }
            let tob = |b: u8| -> u8 { ((info_len >> (b * 8)) & 0xff) as u8 };
            let lenb = [
                tob(7),
                tob(6),
                tob(5),
                tob(4),
                tob(3),
                tob(3),
                tob(1),
                tob(0),
            ];
            let mut start_msg: Vec<u8> = Vec::new();
            start_msg.push(bncom::_const::START);
            start_msg.extend(lenb);
            start_msg.extend(info);
            con.write_all(&start_msg).await.expect("Send error");
            let mut res = [0u8];
            if let Err(e) = con.read_exact(&mut res).await {
                print!("error {}", e);
                return;
            }
            let mut session_map: HashMap<u16, String> = HashMap::with_capacity(client.map.len());
            match res[0] {
                bncom::_const::SUCCESS => {
                    // 处理成功，获取会话ID
                    let mut ids: Vec<u8> = Vec::with_capacity(client.map.len() * 2);
                    for _ in 0..client.map.len() * 2 {
                        ids.push(0);
                    }
                    if let Err(e) = con.read_exact(&mut ids).await {
                        print!("error {}", e);
                        return;
                    }
                    for i in 0..client.map.len() {
                        session_map.insert(
                            ((ids[i * 2] as u16) << 8) | (ids[i * 2 + 1] as u16),
                            client.map[i].inner.clone(),
                        );
                    }
                }
                bncom::_const::ERROR => {
                    panic!("Server error ERROR");
                }
                bncom::_const::ERROR_BUSY => {
                    panic!("Server error ERROR_BUSY");
                }
                bncom::_const::ERROR_PWD => {
                    panic!("Server error ERROR_PWD");
                }
                bncom::_const::ERROR_LIMIT_PORT => {
                    panic!("Port error ERROR_LIMIT_PORT");
                }
                bncom::_const::ERROR_SESSION_OVER => {
                    panic!("Port error ERROR_SESSION_OVER");
                }
                _ => {
                    panic!("Password error {}", res[0]);
                }
            }

            println!("Client is running");
            let mut cmd = [0u8; 4];
            loop {
                match con.read(&mut cmd).await {
                    Ok(si) => {
                        if si == 0 {
                            println!("Maybe break");
                            return;
                        }
                        match cmd[0] {
                            bncom::_const::NEWSOCKET => {
                                let id = ((cmd[1] as u16) << 8) | (cmd[2] as u16);
                                let inner = session_map.get(&id).unwrap().clone();
                                let svr = client.server.clone();
                                tokio::spawn(async move {
                                    work_for_server(
                                        &svr,
                                        &inner,
                                        &[bncom::_const::NEWCONN, cmd[1], cmd[2], cmd[3]],
                                    )
                                    .await;
                                });
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        println!("error {}", e);
                        return;
                    }
                }
            }
        }
        Err(e) => {
            println!("error {}", e);
        }
    }
}

pub async fn handle_client(client: bncom::config::Client) {
    println!("client start->{:#?}", client);
    loop {
        println!("Start connect server");
        ever_handle(client.clone()).await;
        std::thread::sleep(std::time::Duration::from_secs(1))
    }
}
