use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddrV4, TcpStream},
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::Duration,
};

use clipserve::{
    capture::encode_png,
    http::Server,
    store::{SharedStore, Store, StoredImage},
};

fn insert(store: &SharedStore, bytes: Arc<[u8]>) -> String {
    let image = StoredImage {
        png: bytes,
        thumbnail: Arc::new(image::RgbaImage::new(1, 1)),
    };
    let mut id = String::new();
    store
        .lock()
        .unwrap()
        .publish(image, |generated| {
            id = generated.to_owned();
            Ok::<_, ()>(())
        })
        .unwrap();
    id
}

fn listen(store: &SharedStore) -> Server {
    Server::start("127.0.0.1:0".parse().unwrap(), Arc::clone(store), || {
        panic!("the listener failed")
    })
    .unwrap()
}

fn request(endpoint: SocketAddrV4, method: &str, path: &str) -> (String, Vec<u8>) {
    let mut socket = TcpStream::connect_timeout(&endpoint.into(), Duration::from_secs(3)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        socket,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).unwrap();
    let split = response
        .windows(4)
        .position(|chunk| chunk == b"\r\n\r\n")
        .unwrap()
        + 4;
    let body = response.split_off(split);
    (String::from_utf8(response).unwrap(), body)
}

#[test]
fn get_head_rejection_fifo_and_clear() {
    let store = Arc::new(Mutex::new(Store::new(2)));
    let server = listen(&store);
    let endpoint = server.endpoint();
    let png = encode_png(1, 1, &[1, 2, 3, 255]).unwrap();
    let first = insert(&store, Arc::clone(&png));
    let second = insert(&store, Arc::clone(&png));
    let path = format!("/image/{first}.png");
    let (get, body) = request(endpoint, "GET", &path);
    assert!(get.starts_with("HTTP/1.1 200 OK\r\n"));
    for header in [
        "Content-Type: image/png\r\n".into(),
        format!("Content-Length: {}\r\n", png.len()),
        "Cache-Control: no-store\r\n".into(),
        "X-Content-Type-Options: nosniff\r\n".into(),
    ] {
        assert!(get.contains(&header), "missing {header}");
    }
    assert_eq!(&body[..], &*png);
    assert_eq!(request(endpoint, "HEAD", &path), (get, Vec::new()));

    for malformed in [
        "/",
        "/image.png",
        "/image/",
        "/image/..",
        "/image/../../test.png",
        "/image/abcdefgh.png?token=123",
        "/image/abcdefgh.png/",
        "/image/abc%2Fgh.png",
        "/image/abcdefgh.PNG",
        "/image/abcdefgh9.png",
        "/image/abcdefgh.png",
        &format!("http://127.0.0.1:{}/image/{first}.png", endpoint.port()),
    ] {
        assert!(
            request(endpoint, "GET", malformed)
                .0
                .starts_with("HTTP/1.1 404"),
            "{malformed}"
        );
    }
    let (post, body) = request(endpoint, "POST", &path);
    assert!(post.starts_with("HTTP/1.1 405"));
    assert!(post.contains("Allow: GET, HEAD\r\n"));
    assert!(body.is_empty());

    insert(&store, Arc::clone(&png));
    assert!(
        request(endpoint, "GET", &path)
            .0
            .starts_with("HTTP/1.1 404")
    );
    let second = format!("/image/{second}.png");
    assert!(
        request(endpoint, "GET", &second)
            .0
            .starts_with("HTTP/1.1 200")
    );
    store.lock().unwrap().clear();
    assert!(
        request(endpoint, "HEAD", &second)
            .0
            .starts_with("HTTP/1.1 404")
    );
}

#[test]
fn concurrent_requests_and_eviction_share_immutable_snapshots() {
    let store = Arc::new(Mutex::new(Store::new(1)));
    let server = listen(&store);
    let endpoint = server.endpoint();
    let png = encode_png(2, 1, &[0; 8]).unwrap();
    let id = insert(&store, Arc::clone(&png));
    let path = format!("/image/{id}.png");
    let expected = &*png;
    thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    for _ in 0..8 {
                        let (headers, body) = request(endpoint, "GET", &path);
                        if headers.starts_with("HTTP/1.1 200") {
                            assert_eq!(body, expected);
                        } else {
                            assert!(headers.starts_with("HTTP/1.1 404"));
                            assert!(body.is_empty());
                        }
                    }
                })
            })
            .collect();
        insert(&store, Arc::clone(&png));
        for reader in readers {
            reader.join().unwrap();
        }
    });
    assert!(
        request(endpoint, "GET", &path)
            .0
            .starts_with("HTTP/1.1 404")
    );
}

#[test]
fn started_response_finishes_after_clear() {
    let store = Arc::new(Mutex::new(Store::new(1)));
    let server = listen(&store);
    let bytes: Arc<[u8]> = vec![0xa5; 4 * 1024 * 1024].into();
    let id = insert(&store, Arc::clone(&bytes));
    let endpoint = server.endpoint();
    let mut socket = TcpStream::connect(endpoint).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        socket,
        "GET /image/{id}.png HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();

    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "HTTP/1.1 200 OK\r\n");
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
    }
    store.lock().unwrap().clear();
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    assert_eq!(body, &*bytes);
    assert!(
        request(endpoint, "GET", &format!("/image/{id}.png"))
            .0
            .starts_with("HTTP/1.1 404")
    );
}

#[test]
fn failed_bind_preserves_existing_server_and_shutdown_releases_port() {
    let store = Arc::new(Mutex::new(Store::new(1)));
    let server = listen(&store);
    let endpoint = server.endpoint();
    let health = server.health();
    assert!(Server::prepare(endpoint, Arc::clone(&store), || {}).is_err());
    assert!(server.is_healthy());
    assert!(request(endpoint, "GET", "/").0.starts_with("HTTP/1.1 404"));

    let mut slow = TcpStream::connect(endpoint).unwrap();
    slow.write_all(b"GET / HTTP/1.1\r\n").unwrap();
    drop(server);
    assert!(!health.load(Ordering::Acquire));
    let rebound = Server::start(endpoint, store, || {}).unwrap();
    assert!(
        request(rebound.endpoint(), "GET", "/")
            .0
            .starts_with("HTTP/1.1 404")
    );
}

#[test]
fn candidate_does_not_serve_images_until_activated() {
    let store = Arc::new(Mutex::new(Store::new(1)));
    let png = encode_png(1, 1, &[1, 2, 3, 255]).unwrap();
    let id = insert(&store, Arc::clone(&png));
    let path = format!("/image/{id}.png");
    let server = Server::prepare("127.0.0.1:0".parse().unwrap(), store, || {}).unwrap();
    let mut socket = TcpStream::connect(server.endpoint()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let _ = write!(socket, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let mut response = Vec::new();
    let _ = socket.read_to_end(&mut response);
    assert!(response.is_empty());

    server.activate();
    assert_eq!(request(server.endpoint(), "GET", &path).1, &*png);
}
