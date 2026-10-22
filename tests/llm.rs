use std::io::{Read, Write};
use zedict::net::llm::{LlmConfig, Prompt};

fn mock(body: Vec<u8>) -> (LlmConfig, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let headers = String::from_utf8(request).unwrap();
        let length = headers
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut buf = vec![0; length];
        socket.read_exact(&mut buf).unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
        for byte in body {
            if socket
                .write_all(&[b'1', b'\r', b'\n', byte, b'\r', b'\n'])
                .is_err()
            {
                return;
            }
        }
        let _ = socket.write_all(b"0\r\n\r\n");
    });
    (
        LlmConfig {
            base_url: format!("http://{addr}"),
            api_key: "test-only".into(),
            model: "mock".into(),
        },
        handle,
    )
}

#[tokio::test]
async fn fragmented_utf8_crlf_and_early_disconnect() {
    let body =
        "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n";
    let (cfg, handle) = mock(body.as_bytes().to_vec());
    let prompt = Prompt::default();
    let mut emitted = String::new();
    let result = zedict::net::llm::translate_stream(&cfg, &prompt, "test", |d| emitted.push_str(d))
        .await
        .unwrap();
    assert_eq!(result, "你好");
    assert_eq!(emitted, "你好");
    handle.join().unwrap();
    let (cfg, handle) = mock(
        body.split("data: [DONE]")
            .next()
            .unwrap()
            .as_bytes()
            .to_vec(),
    );
    assert!(zedict::net::llm::translate_once(&cfg, &prompt, "test")
        .await
        .is_err());
    handle.join().unwrap();
}

#[test]
fn chunking_preserves_unicode_and_format() {
    let input = "你好，world。\n\n  下一段";
    let chunks = zedict::net::llm::split_paragraphs(input, 3);
    assert_eq!(chunks.concat(), input);
    assert!(chunks.iter().all(|s| s.chars().count() <= 3));
}

#[tokio::test]
async fn service_error_and_invalid_json_are_not_silent_success() {
    for event in [
        "{\"error\":{\"message\":\"额度不足\"}}",
        "not-json",
        "{\"choices\":[{\"finish_reason\":\"length\"}]}",
    ] {
        let (cfg, handle) = mock(format!("data: {event}\n\ndata: [DONE]\n\n").into_bytes());
        assert!(
            zedict::net::llm::translate_once(&cfg, &Prompt::default(), "test")
                .await
                .is_err()
        );
        handle.join().unwrap();
    }
}
