#![cfg(test)]

use super::install_platform_http_client;
use gpui::{Asset, ImageAssetLoader, Resource};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::Duration,
};

#[test]
fn installed_http_client_loads_remote_avatar_resource() {
    const GIF_1X1: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
    ];

    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback avatar server binds");
    let address = listener.local_addr().expect("loopback address resolves");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("avatar request connects");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("loopback read timeout configures");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = socket.read(&mut chunk).expect("avatar request reads");
            assert!(count > 0, "avatar request closed before headers completed");
            request.extend_from_slice(&chunk[..count]);
            assert!(
                request.len() < 16 * 1024,
                "avatar request headers stay bounded"
            );
        }
        let request = String::from_utf8_lossy(&request);
        assert!(
            request.starts_with("GET /avatar.gif "),
            "GPUI image loader must issue a GET for the avatar resource: {request}"
        );
        let expected_user_agent = format!("user-agent: Aeris/{}", env!("CARGO_PKG_VERSION"));
        assert!(
            request
                .lines()
                .any(|line| line.eq_ignore_ascii_case(&expected_user_agent)),
            "installed ReqwestClient must carry the Aeris user agent: {request}"
        );

        write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                GIF_1X1.len()
            )
            .expect("avatar response headers write");
        socket
            .write_all(GIF_1X1)
            .expect("avatar response body writes");
    });

    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let url = format!("http://{address}/avatar.gif");
    gpui_platform::headless().run(move |cx| {
        install_platform_http_client(cx);
        let load = <ImageAssetLoader as Asset>::load(Resource::Uri(url.into()), cx);
        let loaded = reqwest_client::runtime().block_on(load).is_ok();
        let _ = result_tx.send(loaded);
        cx.quit();
    });

    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("avatar loader reports its result"),
        "the GPUI avatar loader must decode a remote image through the installed ReqwestClient"
    );
    server.join().expect("loopback avatar server completes");
}
