use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

fn record(root: &Path, value: Value) {
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("login-events.jsonl"))
        .unwrap();
    writeln!(output, "{value}").unwrap();
    output.flush().unwrap();
}

pub fn run(directory: &PathBuf) -> bool {
    let root = directory.parent().unwrap().parent().unwrap().to_path_buf();
    let Ok(bytes) = std::fs::read(root.join("login-fixture.json")) else {
        return false;
    };
    let config: Value = serde_json::from_slice(&bytes).unwrap();
    std::fs::write(
        root.join("simulated-keychain"),
        b"private fixture credentials",
    )
    .unwrap();
    std::fs::write(root.join("existing-keychain-account"), b"untouched").unwrap();
    let manual = "https://claude.ai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=fixture-state";
    println!("{manual}");
    std::io::stdout().flush().unwrap();
    let (send, receive) = mpsc::channel::<()>();
    let input_root = root.clone();
    let input_send = send.clone();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            record(&input_root, json!({"code":line}));
            if line == "fixture-code#fixture-state" {
                let _ = input_send.send(());
            }
        }
    });
    if config["mode"]
        .as_str()
        .unwrap_or_default()
        .starts_with("opened")
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let opened = format!("https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcallback&state=fixture-state");
        // Claude prints its manual link before its browser is launched.
        thread::sleep(Duration::from_millis(80));
        Command::new(std::env::var("BROWSER").unwrap())
            .arg(&opened)
            .status()
            .unwrap();
        let callback_root = root.clone();
        let stalled = config["mode"] == "opened-stalled";
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = [0; 16384];
                let size = stream.read(&mut bytes).unwrap_or_default();
                let line = String::from_utf8_lossy(&bytes[..size])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                record(&callback_root, json!({"request":line}));
                let valid = line.starts_with("GET /callback?")
                    && line.contains("code=fixture-code")
                    && line.contains("state=fixture-state");
                while stalled && !callback_root.join("release-callback").exists() {
                    thread::sleep(Duration::from_millis(10));
                }
                let status = if valid { "200 OK" } else { "400 Bad Request" };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                if valid {
                    let _ = send.send(());
                }
            }
        });
    }
    record(
        &root,
        json!({"ready":true,"pid":std::process::id(),"directory":directory}),
    );
    while receive.recv_timeout(Duration::from_millis(10)).is_err() {
        if root.join("fail-login").exists() {
            std::process::exit(1);
        }
        if root.join("finish-login").exists() {
            break;
        }
    }
    record(&root, json!({"accepted":true}));
    if config["hold"] == true {
        while !root.join("release-login").exists() {
            thread::sleep(Duration::from_millis(10));
        }
    }
    crate::credentials(directory, false);
    record(&root, json!({"finished":true}));
    true
}

pub fn keychain(args: &[String]) -> bool {
    if !args
        .first()
        .is_some_and(|arg| arg == "delete-generic-password")
    {
        return false;
    }
    let root = PathBuf::from(
        std::env::var_os("NORA_FIXTURE_KEYCHAIN").expect("isolated keychain fixture"),
    );
    std::fs::write(
        root.join("keychain-cleanup.json"),
        serde_json::to_vec(args).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("keychain-pid"), std::process::id().to_string()).unwrap();
    if root.join("stall-keychain").exists() {
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    std::fs::remove_file(root.join("simulated-keychain")).unwrap();
    true
}
