#![allow(dead_code)]

use bytes::Bytes;
use recall_protocol::Reply;
use recall_server::{Config, Server};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub struct TestServer {
    pub address: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<io::Result<()>>,
}

impl TestServer {
    pub async fn start(mut config: Config) -> Self {
        config.bind = "127.0.0.1:0".parse().unwrap();
        let server = Server::bind(config).await.unwrap();
        let address = server.local_addr().unwrap();
        let (stop, receiver) = oneshot::channel();
        let task = tokio::spawn(server.run(async move { let _ = receiver.await; }));
        Self { address, stop, task }
    }

    pub async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.address).await.unwrap()
    }

    pub async fn stop(self) {
        let _ = self.stop.send(());
        timeout(Duration::from_secs(10), self.task).await.unwrap().unwrap().unwrap();
    }
}

pub fn config() -> Config {
    Config {
        workers: 2,
        keys_per_worker: 128,
        max_payload_bytes: 4 * 1024 * 1024,
        ..Config::default()
    }
}

pub fn frame(arguments: &[&[u8]]) -> Vec<u8> {
    let mut result = format!("*{}\r\n", arguments.len()).into_bytes();
    for argument in arguments {
        result.extend_from_slice(format!("${}\r\n", argument.len()).as_bytes());
        result.extend_from_slice(argument);
        result.extend_from_slice(b"\r\n");
    }
    result
}

pub async fn command(stream: &mut TcpStream, arguments: &[&[u8]]) -> Reply {
    stream.write_all(&frame(arguments)).await.unwrap();
    response(stream).await
}

pub async fn response(stream: &mut TcpStream) -> Reply {
    timeout(Duration::from_secs(5), read_reply(stream)).await.unwrap().unwrap()
}

fn read_reply(stream: &mut TcpStream) -> Pin<Box<dyn Future<Output = io::Result<Reply>> + Send + '_>> {
    Box::pin(async move {
        let prefix = stream.read_u8().await?;
        let mut line = Vec::new();
        loop {
            let byte = stream.read_u8().await?;
            if byte == b'\r' {
                if stream.read_u8().await? != b'\n' { return Err(invalid("invalid reply CRLF")); }
                break;
            }
            if line.len() >= 4096 { return Err(invalid("reply header too long")); }
            line.push(byte);
        }
        let number = || std::str::from_utf8(&line).ok().and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| invalid("invalid numeric reply header"));
        match prefix {
            b'+' => Ok(Reply::Simple(Bytes::from(line))),
            b'-' => Ok(Reply::Error(Bytes::from(line))),
            b':' => Ok(Reply::Integer(number()?)),
            b'$' => {
                let length = number()?;
                if length == -1 { return Ok(Reply::Bulk(None)); }
                if !(0..=64 * 1024 * 1024).contains(&length) { return Err(invalid("invalid bulk reply length")); }
                let mut payload = vec![0; length as usize];
                stream.read_exact(&mut payload).await?;
                let mut ending = [0; 2];
                stream.read_exact(&mut ending).await?;
                if ending != *b"\r\n" { return Err(invalid("invalid bulk reply ending")); }
                Ok(Reply::bulk(Bytes::from(payload)))
            }
            b'*' => {
                let count = number()?;
                if !(0..=4096).contains(&count) { return Err(invalid("invalid array reply length")); }
                let mut values = Vec::with_capacity(count as usize);
                for _ in 0..count { values.push(read_reply(stream).await?); }
                Ok(Reply::Array(values))
            }
            _ => Err(invalid("unknown reply prefix")),
        }
    })
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

