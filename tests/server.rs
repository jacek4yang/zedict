#![cfg(feature = "server")]
mod common;

#[tokio::test]
async fn local_api_preserves_query_contracts() {
    let fixture = common::Fixture::new();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let db = fixture.db.clone();
    let server = tokio::spawn(async move { zedict::app::server::serve(addr, db, None).await });
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");
    let mut ready = false;
    for _ in 0..100 {
        if client
            .get(format!("{base}/api/random?n=1"))
            .send()
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(ready, "local server failed to start");
    for mode in ["prefix", "contains"] {
        let response = client
            .get(format!("{base}/api/search"))
            .query(&[("q", "wor"), ("mode", mode)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!(["world"])
        );
    }
    let response = client
        .get(format!("{base}/api/search"))
        .query(&[("q", "计算机"), ("mode", "reverse")])
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()[0]["key"],
        "computer"
    );
    let response = client
        .get(format!("{base}/api/word/take%20into%20account"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for suffix in [
        "/api/search?q=a&mode=invalid",
        "/api/search?q=a&limit=0",
        "/api/random?n=1001",
    ] {
        assert_eq!(
            client
                .get(format!("{base}{suffix}"))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/api/translate"))
            .json(&serde_json::json!({"text":"hello"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.abort();
    let _ = server.await;
}
