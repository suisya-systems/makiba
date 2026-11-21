//! Issue 223: `Hub::connected` and `Hub::reaches` are one answer, and it follows the held request.
//!
//! `reaches` (the seam delivery pushes through) reads the same slot as `connected`, so the two
//! never disagree at one moment. The slot exists only while one held request is open: an
//! accepted push ends that request and the agent counts as not connected until its channel
//! server asks again. A reader that looks at `connected` and then at `reaches` may therefore see
//! true and then false; that is the design, not a defect. This test forces each step in order
//! (no timing): a stand-in channel server speaks the wire by hand, and the thread serving the
//! request is joined before the state after it is read.

use makiba::channel::{Hub, PATH};
use makiba::daemon::delivery::Channel;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const TOKEN: &str = "tok-secretary-0123456789abcdef";

fn hub() -> Hub {
    Hub::new(|t| (t == TOKEN).then(|| "secretary".to_string()))
}

/// Both answers, each read once; they must agree.
fn both(hub: &Hub) -> (bool, bool) {
    (hub.connected("secretary"), hub.reaches("secretary"))
}

/// Open one held request: the client end, and the thread running `Hub::serve` for it.
fn ask(hub: &Hub) -> (TcpStream, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let h = hub.clone();
    let serving = std::thread::spawn(move || h.serve(stream));
    write!(
        client,
        "GET {PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nX-Makiba-Session: 1-1\r\n\r\n"
    )
    .unwrap();
    client.flush().unwrap();
    // The slot is inserted by the serving thread; wait for it (a step, not a timing).
    let until = Instant::now() + Duration::from_secs(20);
    while !hub.connected("secretary") {
        assert!(
            Instant::now() < until,
            "the held request was not registered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    (client, serving)
}

#[test]
fn connected_and_reaches_are_one_answer_that_follows_the_held_request() {
    let hub = hub();
    assert_eq!(both(&hub), (false, false), "before any request");

    // A held request is open: both true.
    let (mut client, serving) = ask(&hub);
    assert_eq!(both(&hub), (true, true), "while the request is held");

    // Push through the seam; the stand-in server reads the answer and acknowledges it.
    let pusher = {
        let hub = hub.clone();
        std::thread::spawn(move || hub.push("secretary", "hello"))
    };
    let mut got = Vec::new();
    let mut chunk = [0u8; 1024];
    while !got.ends_with(b"hello") {
        let n = client.read(&mut chunk).unwrap();
        assert!(
            n > 0,
            "the answer ended early: {:?}",
            String::from_utf8_lossy(&got)
        );
        got.extend_from_slice(&chunk[..n]);
    }
    assert!(
        got.starts_with(b"HTTP/1.1 200 OK\r\n"),
        "{:?}",
        String::from_utf8_lossy(&got)
    );
    client.write_all(b"ACK").unwrap();
    assert_eq!(pusher.join().unwrap(), Ok(()), "the push was accepted");

    // The request that took the push has ended: both false, together, until the next request.
    serving.join().unwrap();
    assert_eq!(both(&hub), (false, false), "after the accepted push");
    drop(client);

    // The server asks again: both true again.
    let (_client, serving) = ask(&hub);
    assert_eq!(both(&hub), (true, true), "after the server asked again");

    hub.close();
    serving.join().unwrap();
    assert_eq!(both(&hub), (false, false), "after the hub is closed");
}
