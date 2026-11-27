use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};

#[test]
fn downloads_reuse_their_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/object");
    let (stop, stopped) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut connections = Vec::new();
            while let Ok((mut socket, _)) = listener.accept() {
                if stopped.try_recv().is_ok() {
                    break;
                }
                connections.push(socket.try_clone().unwrap());
                scope.spawn(move || {
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if !reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                            break;
                        }
                        if line == "\r\n"
                            && socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nbody").is_err()
                        {
                            break;
                        }
                    }
                });
            }
            for socket in &connections {
                let _ = socket.shutdown(Shutdown::Both);
            }
            connections.len()
        });
        let reads = std::thread::scope(|workers| {
            let tasks: Vec<_> = (0..DOWNLOAD_JOBS)
                .map(|_| {
                    workers
                        .spawn(|| (0..5).all(|_| get_bytes(&url).is_ok_and(|body| body == b"body")))
                })
                .collect();
            tasks
                .into_iter()
                .map(|task| task.join().unwrap_or(false))
                .collect::<Vec<_>>()
        });
        stop.send(()).unwrap();
        TcpStream::connect(address).unwrap();
        let accepted = server.join().unwrap();
        assert!(reads.into_iter().all(|read| read));
        assert!(
            (1..=DOWNLOAD_JOBS).contains(&accepted),
            "{accepted} sockets"
        );
    });
}
