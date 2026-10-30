//! Reproduce accepted-stream inheritance with a delayed client on macOS.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

fn observe(reset: bool) -> std::io::Result<usize> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        thread::sleep(Duration::from_millis(150));
        let _ = stream.write_all(b"x");
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    };
    if reset {
        stream.set_nonblocking(false)?;
    }
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let observed = stream.read(&mut [0; 1]);
    client.join().unwrap();
    observed
}
fn main() {
    let inherited = observe(false);
    println!("accepted stream without reset: {inherited:?}");
    if cfg!(target_os = "macos") {
        assert_eq!(
            inherited.unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    let explicit = observe(true);
    println!("accepted stream with blocking reset: {explicit:?}");
    assert_eq!(explicit.unwrap(), 1);
}
