use super::StreamDeckAction;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, client::client_with_config, protocol::WebSocketConfig};

const TIMEOUT: Duration = Duration::from_secs(3);
const RESPONSE_LIMIT: usize = 1024 * 1024;

fn obs_authentication(password: &str, salt: &str, challenge: &str) -> String {
    let secret = STANDARD.encode(Sha256::digest([password, salt].concat()));
    STANDARD.encode(Sha256::digest([secret.as_str(), challenge].concat()))
}

fn obs_receive(socket: &mut WebSocket<TcpStream>, op: u64) -> Result<Value, String> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        socket
            .get_mut()
            .set_read_timeout(Some(remaining.max(Duration::from_millis(1))))
            .map_err(|_| "OBS-Zeitlimit konnte nicht gesetzt werden.")?;
        match socket
            .read()
            .map_err(|_| "OBS hat die Anfrage nicht bestätigt.")?
        {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|_| "OBS hat eine ungültige Nachricht gesendet.")?;
                if value["op"].as_u64() == Some(op) {
                    return Ok(value["d"].clone());
                }
            }
            Message::Close(_) => {
                return Err(
                    "OBS hat die Verbindung beendet. Prüfe Passwort und Einstellungen.".into(),
                );
            }
            _ => {}
        }
    }
    Err("OBS hat die Anfrage nicht rechtzeitig bestätigt.".into())
}

fn obs_send(socket: &mut WebSocket<TcpStream>, message: Value) -> Result<(), String> {
    socket
        .send(Message::Text(message.to_string().into()))
        .map_err(|_| "Die Anfrage konnte nicht an OBS gesendet werden.".into())
}

pub(super) fn run_obs(
    port: u16,
    password: &str,
    request_type: &str,
    data: &Value,
) -> Result<(), String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&address, TIMEOUT).map_err(|_| {
        format!("OBS ist auf Port {port} nicht erreichbar. Aktiviere den WebSocket-Server in OBS.")
    })?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|_| "OBS-Verbindung konnte nicht konfiguriert werden.")?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|_| "OBS-Verbindung konnte nicht konfiguriert werden.")?;
    let limits = WebSocketConfig::default()
        .max_frame_size(Some(RESPONSE_LIMIT))
        .max_message_size(Some(RESPONSE_LIMIT));
    let (mut socket, _) = client_with_config(format!("ws://{address}"), stream, Some(limits))
        .map_err(|_| "OBS-WebSocket-Verbindung konnte nicht aufgebaut werden.")?;
    let hello = obs_receive(&mut socket, 0)?;
    let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": 0 });
    if let Some(auth) = hello["authentication"].as_object() {
        if password.is_empty() {
            return Err("OBS erwartet ein Passwort. Trage es in der Aktion ein.".into());
        }
        let salt = auth
            .get("salt")
            .and_then(Value::as_str)
            .ok_or("OBS hat keine gültige Anmeldung angeboten.")?;
        let challenge = auth
            .get("challenge")
            .and_then(Value::as_str)
            .ok_or("OBS hat keine gültige Anmeldung angeboten.")?;
        identify["authentication"] = json!(obs_authentication(password, salt, challenge));
    }
    obs_send(&mut socket, json!({"op":1,"d":identify}))?;
    obs_receive(&mut socket, 2)
        .map_err(|_| "OBS hat die Anmeldung abgelehnt. Prüfe das Passwort und den Port.")?;
    let id = "pedal";
    obs_send(
        &mut socket,
        json!({"op":6,"d":{"requestType":request_type,"requestId":id,"requestData":data}}),
    )?;
    let result = obs_receive(&mut socket, 7)?;
    if result["requestId"].as_str() != Some(id) {
        return Err("OBS hat eine unerwartete Anfrage bestätigt.".into());
    }
    if result["requestStatus"]["result"].as_bool() != Some(true) {
        let status = &result["requestStatus"];
        return Err(format!(
            "OBS: {}",
            status["comment"].as_str().unwrap_or("Aktion abgelehnt.")
        ));
    }
    Ok(())
}

struct McpSession {
    agent: ureq::Agent,
    url: String,
    session_id: Option<String>,
    protocol: &'static str,
    next_id: u64,
}

impl McpSession {
    fn open(port: u16) -> Result<Self, String> {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .build();
        let mut session = Self {
            agent: config.into(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            session_id: None,
            protocol: "2025-06-18",
            next_id: 0,
        };
        let response = session.request(
            "initialize",
            json!({
                "protocolVersion":session.protocol, "capabilities":{},
                "clientInfo":{"name":"Pedalsteuerung","version":"2.0.0"}
            }),
        )?;
        session.protocol = match response["protocolVersion"].as_str() {
            Some("2025-03-26") => "2025-03-26",
            Some("2025-06-18") => "2025-06-18",
            Some("2025-11-25") => "2025-11-25",
            _ => return Err("Die Protokollversion von Stream Deck wird nicht unterstützt.".into()),
        };
        session.post(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            None,
        )?;
        Ok(session)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let response = self.post(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            Some(id),
        )?;
        if response.get("error").is_some() {
            return Err(
                "Stream Deck hat die Anfrage abgelehnt. Prüfe das MCP Deck und den Port.".into(),
            );
        }
        response
            .get("result")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| "Stream Deck hat eine unerwartete Antwort gesendet.".into())
    }

    fn post(&mut self, message: Value, id: Option<u64>) -> Result<Value, String> {
        let executing = message["params"]["name"]
            .as_str()
            .is_some_and(|name| name.ends_with("execute_action"));
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", self.protocol)
            .header("Connection", "close");
        if let Some(session) = &self.session_id {
            request = request.header("Mcp-Session-Id", session);
        }
        let mut response = request.send(message.to_string()).map_err(|_| {
            if executing {
                "Stream Deck hat die Ausführung nicht bestätigt. Prüfe den Zustand vor einem erneuten Versuch.".to_owned()
            } else {
                "Der lokale Stream-Deck-Server ist nicht erreichbar. Prüfe Port und MCP Deck.".to_owned()
            }
        })?;
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            self.session_id = Some(session.into());
        }
        let status = response.status().as_u16();
        let Some(id) = id else {
            return match status {
                200 | 202 | 204 => Ok(Value::Null),
                _ => Err(format!("Stream Deck antwortet mit HTTP {status}.")),
            };
        };
        if status != 200 {
            return Err(format!("Stream Deck antwortet mit HTTP {status}."));
        }
        let events = response
            .headers()
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let reader = response
            .body_mut()
            .as_reader()
            .take((RESPONSE_LIMIT + 1) as u64);
        if events {
            return read_event(reader, id);
        }
        let mut payload = Vec::new();
        let mut reader = reader;
        reader
            .read_to_end(&mut payload)
            .map_err(|_| "Die Antwort von Stream Deck konnte nicht gelesen werden.")?;
        if payload.len() > RESPONSE_LIMIT {
            return Err("Die Antwort von Stream Deck ist zu groß.".into());
        }
        let reply: Value = serde_json::from_slice(&payload)
            .map_err(|_| "Stream Deck hat ungültige Daten gesendet.")?;
        if reply["id"].as_u64() != Some(id) {
            return Err("Stream Deck hat eine unerwartete Anfrage bestätigt.".into());
        }
        Ok(reply)
    }

    fn tool(&mut self, name: &str) -> Result<Value, String> {
        let qualified = format!("streamdeck__{name}");
        let mut params = json!({});
        for _ in 0..8 {
            let result = self.request("tools/list", params)?;
            if let Some(tool) = result["tools"].as_array().and_then(|tools| {
                tools.iter().find(|tool| matches!(tool["name"].as_str(), Some(value) if value == name || value == qualified))
            }) {
                return Ok(tool.clone());
            }
            let Some(cursor) = result["nextCursor"].as_str() else {
                break;
            };
            params = json!({"cursor":cursor});
        }
        Err(
            "Die Stream-Deck-Funktion ist nicht verfügbar. Prüfe, ob das MCP Deck aktiv ist."
                .into(),
        )
    }

    fn call(&mut self, tool: &Value, arguments: Value) -> Result<Value, String> {
        let response = self.request(
            "tools/call",
            json!({"name":tool["name"],"arguments":arguments}),
        )?;
        if response["isError"].as_bool() == Some(true) {
            return Err("Stream Deck hat die Aktion abgelehnt. Lade die Aktionen neu und prüfe das MCP Deck.".into());
        }
        if let Some(value) = response
            .get("structuredContent")
            .filter(|item| item.is_object())
        {
            return Ok(value.clone());
        }
        if let Some(items) = response["content"].as_array() {
            for item in items {
                if let Some(text) = item["text"].as_str()
                    && let Ok(value) = serde_json::from_str::<Value>(text)
                    && value.is_object()
                {
                    return Ok(value);
                }
            }
        }
        Ok(json!({}))
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        if let Some(session) = &self.session_id {
            let _ = self
                .agent
                .delete(&self.url)
                .header("Mcp-Session-Id", session)
                .header("MCP-Protocol-Version", self.protocol)
                .config()
                .timeout_global(Some(Duration::from_millis(500)))
                .build()
                .call();
        }
    }
}

fn read_event(reader: impl Read, id: u64) -> Result<Value, String> {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut data = String::new();
    let mut total = 0;
    loop {
        let bytes = reader
            .read_line(&mut line)
            .map_err(|_| "Stream Deck hat die Anfrage nicht bestätigt.")?;
        total += bytes;
        if bytes == 0 || total > RESPONSE_LIMIT {
            break;
        }
        let item = line.trim_end_matches(['\n', '\r']);
        if let Some(part) = item.strip_prefix("data:") {
            data.push_str(part.strip_prefix(' ').unwrap_or(part));
            data.push('\n');
        } else if item.is_empty() && !data.is_empty() {
            let value: Value = serde_json::from_str(&data)
                .map_err(|_| "Stream Deck hat ungültige Daten gesendet.")?;
            if value["id"].as_u64() == Some(id) {
                return Ok(value);
            }
            data.clear();
        }
        line.clear();
    }
    Err("Stream Deck hat die Anfrage nicht bestätigt.".into())
}

pub(super) fn list_stream_deck_actions(port: u16) -> Result<Vec<StreamDeckAction>, String> {
    let mut session = McpSession::open(port)?;
    let tool = session.tool("get_executable_actions")?;
    let result = session.call(&tool, json!({}))?;
    let items = result["actions"]
        .as_array()
        .ok_or("Stream Deck hat keine lesbare Aktionsliste gesendet.")?;
    let mut actions = Vec::new();
    for item in items {
        let id = match &item["id"] {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            _ => continue,
        };
        let name = display_field(item, &["title", "name"])
            .unwrap_or(&id)
            .to_owned();
        let description = display_field(item, &["userDescription", "description"])
            .unwrap_or("")
            .to_owned();
        actions.push(StreamDeckAction {
            id,
            name,
            description,
        });
    }
    Ok(actions)
}

fn display_field<'a>(item: &'a Value, fields: &[&str]) -> Option<&'a str> {
    fields.iter().find_map(|field| {
        item[*field]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

pub(super) fn run_stream_deck(port: u16, action_id: &str) -> Result<(), String> {
    let mut session = McpSession::open(port)?;
    let tool = session.tool("execute_action")?;
    let id = match tool["inputSchema"]["properties"]["id"]["type"].as_str() {
        Some("integer" | "number") => {
            json!(
                action_id
                    .parse::<i64>()
                    .map_err(|_| "Die Aktions-ID passt nicht. Lade die Aktionen neu.")?
            )
        }
        _ => json!(action_id),
    };
    let result = session.call(&tool, json!({"id":id}))?;
    if result.get("error").is_some()
        || result["status"]
            .as_str()
            .is_some_and(|status| status != "ok")
    {
        return Err("Stream Deck hat die Ausführung nicht bestätigt.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    #[test]
    fn reads_only_the_requested_mcp_event() {
        let events = b"data: {\"id\":4}\n\ndata: {\"id\":5,\ndata: \"result\":{}}\n\n";
        assert_eq!(
            read_event(events.as_slice(), 5).unwrap(),
            json!({"id":5,"result":{}})
        );
    }

    #[test]
    fn invokes_only_the_selected_local_stream_deck_action() {
        use std::io::Write;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for phase in 0..10 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(TIMEOUT)).unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut length = 0;
                let mut has_session = false;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim().to_ascii_lowercase();
                    if header.is_empty() {
                        break;
                    }
                    if let Some(value) = header.strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if header == "mcp-session-id: pedal-test" {
                        has_session = true;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if phase % 5 == 4 {
                    assert!(request.starts_with("DELETE /mcp "));
                    assert!(has_session);
                    write!(
                        stream,
                        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    continue;
                }
                assert!(request.starts_with("POST /mcp "));
                assert_eq!(has_session, phase % 5 != 0);
                let value: Value = serde_json::from_slice(&body).unwrap();
                let method = value["method"].as_str().unwrap();
                let result = match phase % 5 {
                    0 => {
                        assert_eq!(method, "initialize");
                        json!({"protocolVersion":"2025-06-18"})
                    }
                    1 => {
                        assert_eq!(method, "notifications/initialized");
                        write!(stream, "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                        continue;
                    }
                    2 => {
                        assert_eq!(method, "tools/list");
                        json!({"tools":[
                            {"name":"streamdeck__get_executable_actions"},
                            {"name":"streamdeck__execute_action","inputSchema":{"properties":{"id":{"type":"integer"}}}}
                        ]})
                    }
                    3 if phase < 5 => {
                        assert_eq!(method, "tools/call");
                        assert_eq!(
                            value["params"]["name"],
                            "streamdeck__get_executable_actions"
                        );
                        json!({"structuredContent":{"actions":[{"id":42,"title":"Studio"}]}})
                    }
                    3 => {
                        assert_eq!(method, "tools/call");
                        assert_eq!(value["params"]["name"], "streamdeck__execute_action");
                        assert_eq!(value["params"]["arguments"], json!({"id":42}));
                        json!({"structuredContent":{"status":"ok"}})
                    }
                    _ => unreachable!(),
                };
                let reply = json!({"jsonrpc":"2.0","id":value["id"],"result":result});
                let body = format!("data: {reply}\n\n");
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: pedal-test\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", body.len(), body).unwrap();
            }
        });
        let found = list_stream_deck_actions(port).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Studio");
        run_stream_deck(port, &found[0].id).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn authenticates_with_obs_and_waits_for_request_confirmation() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(TIMEOUT)).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            obs_send(&mut socket, json!({"op":0,"d":{"rpcVersion":1,"authentication":{"salt":"salt","challenge":"challenge"}}})).unwrap();
            let request = obs_receive(&mut socket, 1).unwrap();
            assert_eq!(
                request["authentication"],
                "zTM5ki6L2vVvBQiTG9ckH1Lh64AbnCf6XZ226UmnkIA="
            );
            obs_send(&mut socket, json!({"op":2,"d":{"negotiatedRpcVersion":1}})).unwrap();
            let request = obs_receive(&mut socket, 6).unwrap();
            assert_eq!(request["requestType"], "SetCurrentProgramScene");
            assert_eq!(request["requestData"]["sceneName"], "Studio");
            obs_send(&mut socket, json!({"op":7,"d":{"requestId":request["requestId"],"requestStatus":{"result":true}}})).unwrap();
        });
        let result = run_obs(
            port,
            "password",
            "SetCurrentProgramScene",
            &json!({"sceneName":"Studio"}),
        );
        server.join().unwrap();
        result.unwrap();
    }
}
