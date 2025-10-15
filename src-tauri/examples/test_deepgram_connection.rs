use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use http::Request;

const API_KEY: &str = "c73950ca938f2b20e33eade8c6dedd7d325033cc";

#[tokio::main]
async fn main() {
    env_logger::init();

    // Попробуем разные варианты URL
    let test_urls = vec![
        ("С en-US", "wss://api.deepgram.com/v1/listen?encoding=linear16&sample_rate=16000&channels=1&language=en-US"),
        ("С nova-2", "wss://api.deepgram.com/v1/listen?encoding=linear16&sample_rate=16000&channels=1&model=nova-2&language=en-US"),
        ("С ru", "wss://api.deepgram.com/v1/listen?encoding=linear16&sample_rate=16000&channels=1&language=ru"),
        ("Полный с nova-2", "wss://api.deepgram.com/v1/listen?encoding=linear16&sample_rate=16000&channels=1&model=nova-2&language=ru&punctuate=true&interim_results=true"),
    ];

    for (name, url) in test_urls {
        println!("\n{}", "=".repeat(60));
        println!("🧪 Тест: {}", name);
        println!("🔗 Подключаемся к: {}", url);

    let request = Request::builder()
        .method("GET")
        .uri(url)
        .header("Host", "api.deepgram.com")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
        .header("Authorization", format!("Token {}", API_KEY))
        .body(())
        .unwrap();

    println!("📡 Заголовки запроса:");
    for (name, value) in request.headers() {
        println!("  {}: {:?}", name, value);
    }

    match connect_async(request).await {
        Ok((ws_stream, response)) => {
            println!("✅ WebSocket подключен!");
            println!("📥 Response status: {:?}", response.status());
            println!("📥 Response headers:");
            for (name, value) in response.headers() {
                println!("  {}: {:?}", name, value);
            }

            let (mut write, mut read) = ws_stream.split();

            println!("\n👂 Слушаем сообщения от Deepgram...\n");

            // Ждем сообщения 10 секунд
            let mut count = 0;
            loop {
                tokio::select! {
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                count += 1;
                                println!("📨 Сообщение #{}: {}", count, text);

                                // Попробуем распарсить JSON
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(msg_type) = json["type"].as_str() {
                                        println!("   Тип: {}", msg_type);
                                    }
                                }
                            }
                            Some(Ok(Message::Binary(data))) => {
                                println!("📦 Бинарные данные: {} байт", data.len());
                            }
                            Some(Ok(Message::Close(frame))) => {
                                println!("🔴 Соединение закрыто: {:?}", frame);
                                break;
                            }
                            Some(Ok(msg)) => {
                                println!("📬 Другое сообщение: {:?}", msg);
                            }
                            Some(Err(e)) => {
                                println!("❌ Ошибка: {}", e);
                                break;
                            }
                            None => {
                                println!("❌ Stream завершен");
                                break;
                            }
                        }
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(3)) => {
                        println!("\n⏱️  3 секунды прошло, закрываем соединение");
                        let _ = write.send(Message::Close(None)).await;
                        break;
                    }
                }
            }

            println!("\n✅ Всего получено сообщений: {}", count);
        }
        Err(e) => {
            println!("❌ Ошибка подключения: {:?}", e);
        }
    }
    } // end for loop
}
