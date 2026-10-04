//! MQTT I/O on a dedicated runtime, with bounded delivery and drop-based cancellation.

use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use chrono::Local;

use rumqttc::tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::aws_lc_rs};
use rumqttc::{AsyncClient, Event, MqttOptions, Outgoing, Packet, QoS, SubscribeFilter, SubscribeReasonCode, Transport};
use tokio::sync::{mpsc, oneshot};

use crate::{config::ConnectionConfig, topics::Message};

pub enum BrokerEvent {
    Connecting,
    Connected,
    Status(String),
    OperationError(String),
    PublishQueued,
    PublishError(String),
    Message(Message),
}

/// Receives broker events and cancels the MQTT worker when dropped.
pub struct Connection {
    pub events: mpsc::Receiver<BrokerEvent>,
    stop: Option<oneshot::Sender<()>>,
    commands: mpsc::Sender<Command>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    Connected,
    Disconnected,
}

/// Quality of service for an MQTT publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qos {
    AtMostOnce,
    AtLeastOnce,
    ExactlyOnce,
}

impl From<Qos> for QoS {
    fn from(qos: Qos) -> Self {
        match qos {
            Qos::AtMostOnce => Self::AtMostOnce,
            Qos::AtLeastOnce => Self::AtLeastOnce,
            Qos::ExactlyOnce => Self::ExactlyOnce,
        }
    }
}

const COMMAND_CAPACITY: usize = 16;

struct DeleteTopicsData {
    topics: Vec<String>,
}

struct PublishCommandData {
    topic: String,
    payload: Vec<u8>,
    qos: QoS,
    retain: bool,
}

enum Command {
    DeleteTopics(DeleteTopicsData),
    Publish(PublishCommandData),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    DeleteTopics,
    Publish,
}

impl OperationKind {
    fn error(self, message: String) -> BrokerEvent {
        match self {
            Self::DeleteTopics => BrokerEvent::OperationError(message),
            Self::Publish => BrokerEvent::PublishError(message),
        }
    }
}

impl Command {
    fn kind(&self) -> OperationKind {
        match self {
            Self::DeleteTopics(_) => OperationKind::DeleteTopics,
            Self::Publish(_) => OperationKind::Publish,
        }
    }
}

fn ensure_valid_topic(topic: &str) -> Result<()> {
    ensure!(!topic.is_empty(), "MQTT topic names must not be empty.");
    ensure!(
        !topic.contains(['#', '+']),
        "Publishing requires an exact topic name, not MQTT wildcards."
    );
    ensure!(
        topic.len() <= u16::MAX as usize,
        "MQTT topic names must not exceed 65535 UTF-8 bytes."
    );
    ensure!(!topic.contains('\0'), "MQTT topic names must not contain a null character.");
    Ok(())
}

impl Connection {
    /// Queues a publish to an exact MQTT topic.
    /// Invalid topics and full/closed queues fail immediately. `Ok(())` means
    /// accepted into the command queue, not delivered. The worker reports
    /// `BrokerEvent::PublishQueued` or `BrokerEvent::PublishError`; a disconnect
    /// can produce an error even after `PublishQueued`.
    pub fn publish(&self, topic: String, payload: Vec<u8>, qos: Qos, retain: bool) -> Result<()> {
        ensure_valid_topic(&topic)?;

        self.commands
            .try_send(Command::Publish(PublishCommandData {
                topic,
                payload,
                qos: qos.into(),
                retain,
            }))
            .context("Could not queue publish (queue full or worker stopped).")
    }

    /// Queues retained-message deletion for exact MQTT topic names.
    /// Invalid topics and full/closed queues fail immediately; an empty batch
    /// does nothing. `Ok(())` means queued, not deleted. The worker reports
    /// failures, including disconnection, via `BrokerEvent::OperationError`.
    pub fn delete_topics(&self, topics: Vec<String>) -> Result<()> {
        if topics.is_empty() {
            return Ok(());
        }

        for topic in &topics {
            ensure_valid_topic(topic)?;
        }

        self.commands
            .try_send(Command::DeleteTopics(DeleteTopicsData { topics }))
            .context("Could not queue topic deletion (queue full or worker stopped).")
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Starts a worker for a validated connection, returning setup failures to the caller.
pub fn connect(config: ConnectionConfig) -> Result<Connection> {
    config.validate()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Could not create the MQTT runtime.")?;
    // A bounded queue applies backpressure during retained-message bursts.
    let (sender, events) = mpsc::channel(2048);
    let (stop, stopped) = oneshot::channel();
    let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);

    std::thread::Builder::new()
        .name("mqtt".into())
        .spawn(move || {
            runtime.block_on(async {
                let result = tokio::select! {
                    _ = stopped => return,
                    result = run(config, &sender, command_receiver) => result,
                };
                if let Err(error) = result {
                    let _ = sender.send(BrokerEvent::Status(format!("{error:#}"))).await;
                }
            });
        })
        .context("Could not start the MQTT worker thread.")?;
    Ok(Connection {
        events,
        stop: Some(stop),
        commands,
    })
}

fn tls_transport(roots: RootCertStore) -> Result<Transport> {
    ensure!(!roots.is_empty(), "Could not load trusted system certificates for TLS.");
    let config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .context("Could not configure the TLS protocol versions.")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Transport::tls_with_config(config.into()))
}

fn mqtt_options(config: &ConnectionConfig) -> Result<MqttOptions> {
    // rumqttc requires the full URL for WebSocket and reads its port from that URL.
    let host = if config.websocket {
        let host = &config.host;
        if host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("ws://[{host}]:{}/mqtt", config.port)
        } else {
            format!("ws://{host}:{}/mqtt", config.port)
        }
    } else {
        config.host.clone()
    };
    let mut options = MqttOptions::new(&config.client_id, host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_session(true);
    options.set_max_packet_size(16 * 1024 * 1024, 16 * 1024 * 1024);
    if !config.username.is_empty() {
        options.set_credentials(&config.username, &config.password);
    }
    if config.websocket {
        options.set_transport(Transport::ws());
    } else if config.tls {
        // The default transport helper panics when platform certificate loading fails.
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        options.set_transport(tls_transport(roots)?);
    }
    Ok(options)
}

async fn publish_clear_retained(client: AsyncClient, topics: Vec<String>, queued: Rc<RefCell<VecDeque<OperationKind>>>) -> Result<()> {
    for topic in topics {
        // Register before awaiting: a full flume queue can hand the request to
        // poll before the sending future is resumed.
        queued.borrow_mut().push_back(OperationKind::DeleteTopics);
        if let Err(error) = client.publish(topic, QoS::AtMostOnce, true, Vec::new()).await {
            queued.borrow_mut().pop_back();
            return Err(error).context("Could not queue a retained-topic deletion publish.");
        }
    }
    Ok(())
}

async fn queue_command(client: AsyncClient, command: Command, queued: Rc<RefCell<VecDeque<OperationKind>>>) -> Result<()> {
    match command {
        Command::DeleteTopics(command) => publish_clear_retained(client, command.topics, queued).await,
        Command::Publish(command) => {
            queued.borrow_mut().push_back(OperationKind::Publish);
            if let Err(error) = client.publish(command.topic, command.qos, command.retain, command.payload).await {
                queued.borrow_mut().pop_back();
                return Err(error).context("Could not queue publish to rumqttc.");
            }
            Ok(())
        }
    }
}

fn disconnected_error(kind: OperationKind) -> BrokerEvent {
    let message = match kind {
        OperationKind::DeleteTopics => "Retained-topic deletion cancelled while disconnected. Retry while connected.",
        OperationKind::Publish => "Publish cancelled while disconnected. Publish again while connected.",
    };
    kind.error(message.into())
}

async fn run(config: ConnectionConfig, sender: &mpsc::Sender<BrokerEvent>, mut commands: mpsc::Receiver<Command>) -> Result<()> {
    let options = mqtt_options(&config)?;
    let mut session = SessionState::Disconnected;
    let (mut client, mut event_loop) = AsyncClient::new(options.clone(), 16);
    event_loop.network_options.set_connection_timeout(10);
    let mut operation = None;
    // Only the worker and its active future share this FIFO. Keeping origins
    // separately avoids mistaking an empty retained user publish for a deletion.
    let queued = Rc::new(RefCell::new(VecDeque::new()));
    let mut inflight_publishes = HashSet::new();
    let mut pending_subscription = None;

    loop {
        // Publishing must yield to poll when rumqttc's bounded request queue fills.
        // Keeping the future here also cancels a blocked batch on drop/disconnect.
        let result = {
            // poll is not cancellation-safe while establishing a connection.
            // Keep it alive when commands arrive or a batch finishes queueing.
            let poll = event_loop.poll();
            tokio::pin!(poll);
            loop {
                tokio::select! {
                    result = &mut poll => break result,
                    Some(command) = commands.recv(), if operation.is_none() => {
                        let kind = command.kind();
                        if session == SessionState::Disconnected {
                            if sender.send(disconnected_error(kind)).await.is_err() {
                                return Ok(());
                            }
                        } else {
                            operation = Some((kind, Box::pin(queue_command(client.clone(), command, Rc::clone(&queued)))));
                        }
                    }
                    result = async {
                        match operation.as_mut() {
                            Some((_, operation)) => operation.await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Some((kind, _)) = operation.take() {
                            let event = match result {
                                Err(error) => kind.error(format!("{error:#}")),
                                Ok(()) if kind == OperationKind::Publish => BrokerEvent::PublishQueued,
                                Ok(()) => continue,
                            };
                            if sender.send(event).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        };
        let event = match result {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                pending_subscription = None;
                let filters = config.topics.iter().map(|subscription| {
                    let qos = match subscription.qos {
                        0 => QoS::AtMostOnce,
                        1 => QoS::AtLeastOnce,
                        _ => QoS::ExactlyOnce, // connect validates the QoS range before starting the worker.
                    };
                    SubscribeFilter::new(subscription.topic.clone(), qos)
                });
                // One request avoids filling the bounded queue while poll is paused,
                // even when there are more filters than the request queue's capacity.
                client.subscribe_many(filters).await.context("Could not subscribe to topics.")?;
                continue;
            }
            Ok(Event::Outgoing(Outgoing::Subscribe(pkid))) => {
                pending_subscription = Some(pkid);
                continue;
            }
            Ok(Event::Incoming(Packet::SubAck(ack))) => {
                if pending_subscription != Some(ack.pkid) {
                    continue;
                }
                pending_subscription = None;
                if ack.return_codes.iter().any(|code| matches!(code, SubscribeReasonCode::Failure)) {
                    BrokerEvent::Status("Broker refused one or more topic subscriptions. Check access permissions.".into())
                } else if ack.return_codes.len() != config.topics.len() {
                    BrokerEvent::Status("Broker did not acknowledge all topic subscriptions.".into())
                } else {
                    session = SessionState::Connected;
                    BrokerEvent::Connected
                }
            }
            Ok(Event::Outgoing(Outgoing::Publish(pkid))) => {
                if queued.borrow_mut().pop_front() == Some(OperationKind::Publish) && pkid != 0 {
                    inflight_publishes.insert(pkid);
                }
                continue;
            }
            Ok(Event::Incoming(Packet::PubAck(ack))) => {
                inflight_publishes.remove(&ack.pkid);
                continue;
            }
            Ok(Event::Incoming(Packet::PubComp(ack))) => {
                inflight_publishes.remove(&ack.pkid);
                continue;
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => BrokerEvent::Message(Message {
                topic: publish.topic,
                payload: publish.payload,
                qos: publish.qos as u8,
                retained: publish.retain,
                received_at: Local::now(),
            }),
            Ok(_) => continue,
            Err(error) => {
                pending_subscription = None;
                session = SessionState::Disconnected;
                while let Ok(command) = commands.try_recv() {
                    if sender.send(disconnected_error(command.kind())).await.is_err() {
                        return Ok(());
                    }
                }
                let interrupted = operation.take().map(|(kind, _)| kind);
                let (pending_deletions, pending_publishes) = {
                    let mut queued = queued.borrow_mut();
                    let deletions = queued.contains(&OperationKind::DeleteTopics);
                    let publishes = queued.contains(&OperationKind::Publish);
                    queued.clear();
                    (deletions, publishes)
                };
                // A fresh client discards rumqttc's requests, unacknowledged QoS
                // publishes, QoS 2 releases, collisions, and buffered events.
                // No user operation may carry over into a new broker session.
                (client, event_loop) = AsyncClient::new(options.clone(), 16);
                event_loop.network_options.set_connection_timeout(10);
                let interrupted_publish =
                    interrupted == Some(OperationKind::Publish) || pending_publishes || !inflight_publishes.is_empty();
                inflight_publishes.clear();
                if (interrupted == Some(OperationKind::DeleteTopics) || pending_deletions)
                    && sender.send(BrokerEvent::OperationError(
                        "Retained-topic deletion interrupted by disconnect; some topics may already have been cleared. It will not be retried.".into(),
                    )).await.is_err()
                {
                    return Ok(());
                }
                if interrupted_publish
                    && sender
                        .send(BrokerEvent::PublishError(
                            "Publish interrupted by disconnect; it may already have reached the broker. It will not be retried.".into(),
                        ))
                        .await
                        .is_err()
                {
                    return Ok(());
                }
                if sender
                    .send(BrokerEvent::Status(format!("{error} · retrying in 2 seconds")))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                let retry = tokio::time::sleep(Duration::from_secs(2));
                tokio::pin!(retry);
                loop {
                    tokio::select! {
                        _ = &mut retry => break,
                        Some(command) = commands.recv() => {
                            if sender.send(disconnected_error(command.kind())).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
                if sender.send(BrokerEvent::Connecting).await.is_err() {
                    return Ok(());
                }
                continue;
            }
        };
        if sender.send(event).await.is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TopicSubscription;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
    };

    fn read_packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut header = [0];
        stream.read_exact(&mut header).unwrap();
        let mut length = 0;
        let mut multiplier = 1;
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            length += (byte[0] & 127) as usize * multiplier;
            if byte[0] & 128 == 0 {
                break;
            }
            multiplier *= 128;
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    fn connect_client_id(body: &[u8]) -> &str {
        assert_eq!(&body[..7], b"\0\x04MQTT\x04");
        let length = usize::from(u16::from_be_bytes([body[10], body[11]]));
        std::str::from_utf8(&body[12..12 + length]).unwrap()
    }

    fn handshake(listener: &TcpListener, granted: u8) -> TcpStream {
        handshake_with_subscriptions(listener, &[TopicSubscription::default()], &[granted])
    }

    fn handshake_with_subscriptions(listener: &TcpListener, topics: &[TopicSubscription], granted: &[u8]) -> TcpStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(read_packet(&mut stream).0, 0x10); // CONNECT
        stream.write_all(&[0x20, 2, 0, 0]).unwrap(); // CONNACK
        let (header, body) = read_packet(&mut stream);
        assert_eq!(header, 0x82); // SUBSCRIBE, never PUBLISH
        let mut expected = Vec::new();
        for subscription in topics {
            expected.extend_from_slice(&(subscription.topic.len() as u16).to_be_bytes());
            expected.extend_from_slice(subscription.topic.as_bytes());
            expected.push(subscription.qos);
        }
        assert_eq!(&body[2..], expected.as_slice(), "all filters and their QoS must be in one packet");
        let mut ack = vec![0x90, (2 + granted.len()) as u8, body[0], body[1]];
        ack.extend_from_slice(granted);
        stream.write_all(&ack).unwrap();
        stream
    }

    fn next_event(runtime: &tokio::runtime::Runtime, connection: &mut Connection) -> BrokerEvent {
        next_matching_event(runtime, connection, |_| true)
    }

    fn next_matching_event(
        runtime: &tokio::runtime::Runtime,
        connection: &mut Connection,
        predicate: impl Fn(&BrokerEvent) -> bool,
    ) -> BrokerEvent {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = connection.events.recv().await.expect("MQTT worker stopped");
                    if predicate(&event) {
                        return event;
                    }
                }
            })
            .await
            .expect("MQTT event timeout")
        })
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn mqtt_options_keep_plain_tcp_host_and_port() {
        let config = ConnectionConfig {
            host: "broker.example".into(),
            port: 1884,
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert_eq!(options.broker_address(), ("broker.example".into(), 1884));
        assert!(matches!(options.transport(), Transport::Tcp));
    }

    #[test]
    fn mqtt_options_use_system_trusted_tls_for_mqtts() {
        let config = ConnectionConfig {
            tls: true,
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert!(matches!(options.transport(), Transport::Tls(_)));
    }

    #[test]
    fn mqtt_options_use_websocket_urls_for_hostnames_ipv4_and_ipv6() {
        for (host, expected) in [
            ("broker.example", "ws://broker.example:9001/mqtt"),
            ("127.0.0.1", "ws://127.0.0.1:9001/mqtt"),
            ("::1", "ws://[::1]:9001/mqtt"),
            ("[::1]", "ws://[::1]:9001/mqtt"),
        ] {
            let config = ConnectionConfig {
                host: host.into(),
                port: 9001,
                websocket: true,
                ..Default::default()
            };
            let options = mqtt_options(&config).unwrap();
            assert_eq!(options.broker_address(), (expected.into(), 9001));
            assert!(matches!(options.transport(), Transport::Ws));
        }
    }

    #[test]
    fn mqtt_options_prefer_unencrypted_websocket_over_tls_and_keep_mqtt_settings() {
        let config = ConnectionConfig {
            websocket: true,
            tls: true,
            client_id: "ws-client".into(),
            username: "mqtt-user".into(),
            password: "mqtt-secret".into(),
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert!(matches!(options.transport(), Transport::Ws));
        assert_eq!(options.client_id(), config.client_id);
        assert_eq!(options.credentials(), Some(rumqttc::Login::new(config.username, config.password)));
        assert_eq!(options.keep_alive(), Duration::from_secs(30));
        assert!(options.clean_session());
    }

    #[expect(
        clippy::result_large_err,
        reason = "tungstenite's handshake callback requires an unboxed HTTP error response"
    )]
    fn websocket_broker_roundtrip(listener: TcpListener, host: &str) {
        use tungstenite::{
            Message as WsMessage, accept_hdr,
            handshake::server::{Request, Response},
        };

        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut socket = accept_hdr(stream, |request: &Request, mut response: Response| {
                assert_eq!(request.uri().path(), "/mqtt");
                assert!(request.headers()["host"].to_str().unwrap().ends_with(&format!(":{port}")));
                assert_eq!(request.headers()["sec-websocket-protocol"], "mqtt");
                response.headers_mut().insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                Ok(response)
            })
            .unwrap();
            let connect = socket.read().unwrap().into_data();
            assert_eq!(connect[0], 0x10);
            assert_eq!(connect_client_id(&connect[2..]), "ws-client");
            socket.send(WsMessage::Binary(vec![0x20, 2, 0, 0].into())).unwrap();
            let subscribe = socket.read().unwrap().into_data();
            assert_eq!(subscribe[0], 0x82);
            assert_eq!(&subscribe[4..], b"\0\x01#\0");
            socket
                .send(WsMessage::Binary(vec![0x90, 3, subscribe[2], subscribe[3], 0].into()))
                .unwrap();
            socket.send(WsMessage::Binary(b"\x30\x0b\0\x05topicdata".to_vec().into())).unwrap();
            // Keep the broker alive until the client has consumed the message and stops.
            let _ = socket.read();
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: host.into(),
            port,
            websocket: true,
            client_id: "ws-client".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        match next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Message(_))) {
            BrokerEvent::Message(message) => {
                assert_eq!(message.topic, "topic");
                assert_eq!(message.payload.as_ref(), b"data");
            }
            _ => unreachable!(),
        }
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn websocket_transport_connects_subscribes_and_receives_messages_over_ipv4() {
        websocket_broker_roundtrip(TcpListener::bind("127.0.0.1:0").unwrap(), "127.0.0.1");
    }

    #[test]
    fn websocket_transport_connects_subscribes_and_receives_messages_over_ipv6() {
        let listener = match TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(error) if matches!(error.kind(), std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported) => {
                eprintln!("Skipping IPv6 WebSocket roundtrip: IPv6 loopback is unavailable ({error}).");
                return;
            }
            Err(error) => panic!("Could not bind IPv6 loopback: {error}"),
        };
        websocket_broker_roundtrip(listener, "::1");
        websocket_broker_roundtrip(TcpListener::bind("[::1]:0").unwrap(), "[::1]");
    }

    #[test]
    fn connect_uses_the_configured_client_id() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let (header, body) = read_packet(&mut stream);
            assert_eq!(header, 0x10);
            assert_eq!(connect_client_id(&body), "custom-client");
            stream.write_all(&[0x20, 2, 0, 0]).unwrap();
            let (header, subscribe) = read_packet(&mut stream);
            assert_eq!(header, 0x82);
            stream.write_all(&[0x90, 3, subscribe[0], subscribe[1], 0]).unwrap();
            // Closing now can race with the client reading SUBACK; wait for client teardown.
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).unwrap(), 0, "dropping the connection must close its socket");
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            client_id: "custom-client".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn receives_retained_messages_resubscribes_and_stops_on_drop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (delivered, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            for pass in 0..2 {
                let mut stream = handshake(&listener, 0);
                // Retained QoS 0 PUBLISH: topic `test/value`, payload `42`.
                stream.write_all(b"\x31\x0e\x00\x0atest/value42").unwrap();
                if pass == 0 {
                    received.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                if pass == 1 {
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).unwrap(), 0, "dropping the connection must close its socket");
                }
                // Drop the first socket to exercise automatic reconnect.
            }
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        for pass in 0..2 {
            next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
            match next_event(&runtime, &mut connection) {
                BrokerEvent::Message(message) => {
                    assert_eq!(message.topic, "test/value");
                    assert_eq!(message.payload.as_ref(), b"42");
                    assert_eq!(message.qos, 0);
                    assert!(message.retained);
                }
                BrokerEvent::Status(status) | BrokerEvent::OperationError(status) | BrokerEvent::PublishError(status) => {
                    panic!("expected retained message, got: {status}")
                }
                BrokerEvent::Connected | BrokerEvent::Connecting | BrokerEvent::PublishQueued => {
                    panic!("expected retained message, got duplicate connect")
                }
            }
            if pass == 0 {
                delivered.send(()).unwrap();
            }
        }
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn subscribes_to_all_configured_topics_with_individual_qos_in_one_packet_after_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (delivered, received) = std::sync::mpsc::channel();
        // More filters than rumqttc's request queue capacity catches a loop of
        // awaited subscribe calls that would deadlock without polling the queue.
        let topics: Vec<_> = (0..24)
            .map(|index| TopicSubscription {
                topic: format!("home/{index}/+"),
                qos: (index % 3) as u8,
            })
            .collect();
        let expected = topics.clone();
        let broker = std::thread::spawn(move || {
            let granted: Vec<_> = expected.iter().map(|subscription| subscription.qos).collect();
            for _ in 0..2 {
                let _stream = handshake_with_subscriptions(&listener, &expected, &granted);
                received.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            topics,
            ..Default::default()
        })
        .unwrap();
        for _ in 0..2 {
            next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
            delivered.send(()).unwrap();
        }
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn reports_rejected_subscriptions_without_connecting_when_only_one_filter_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let topics = vec![
            TopicSubscription {
                topic: "home/#".into(),
                qos: 0,
            },
            TopicSubscription {
                topic: "office/#".into(),
                qos: 1,
            },
            TopicSubscription {
                topic: "$SYS/#".into(),
                qos: 2,
            },
        ];
        let expected = topics.clone();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake_with_subscriptions(&listener, &expected, &[0, 0x80, 2]);
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            topics,
            ..Default::default()
        })
        .unwrap();
        let event = next_matching_event(&runtime, &mut connection, |event| match event {
            BrokerEvent::Status(status) => status.contains("refused"),
            BrokerEvent::Connected => true,
            BrokerEvent::Message(_)
            | BrokerEvent::Connecting
            | BrokerEvent::OperationError(_)
            | BrokerEvent::PublishQueued
            | BrokerEvent::PublishError(_) => false,
        });
        assert!(matches!(event, BrokerEvent::Status(_)), "a refused subscription is not connected");
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn does_not_connect_when_suback_has_too_few_or_too_many_results() {
        for granted in [vec![0], vec![0, 1, 2]] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let topics = vec![
                TopicSubscription {
                    topic: "home/#".into(),
                    qos: 0,
                },
                TopicSubscription {
                    topic: "office/#".into(),
                    qos: 1,
                },
            ];
            let expected = topics.clone();
            let broker = std::thread::spawn(move || {
                let mut stream = handshake_with_subscriptions(&listener, &expected, &granted);
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            });
            let runtime = runtime();
            let mut connection = connect(ConnectionConfig {
                host: "127.0.0.1".into(),
                port,
                topics,
                ..Default::default()
            })
            .unwrap();
            let event = next_matching_event(&runtime, &mut connection, |event| match event {
                BrokerEvent::Status(status) => status.contains("acknowledge all"),
                BrokerEvent::Connected => true,
                BrokerEvent::Message(_)
                | BrokerEvent::Connecting
                | BrokerEvent::OperationError(_)
                | BrokerEvent::PublishQueued
                | BrokerEvent::PublishError(_) => false,
            });
            assert!(matches!(event, BrokerEvent::Status(_)), "an incomplete SUBACK is not connected");
            drop(connection);
            broker.join().unwrap();
        }
    }

    fn command_connection() -> (Connection, mpsc::Receiver<Command>, oneshot::Receiver<()>) {
        let (_, events) = mpsc::channel(1);
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (stop, stopped) = oneshot::channel();
        (
            Connection {
                events,
                stop: Some(stop),
                commands,
            },
            receiver,
            stopped,
        )
    }

    #[test]
    fn publish_validates_exact_topics_before_queueing() {
        let (connection, mut commands, _) = command_connection();
        for invalid in ["", "#", "a/+", "a#b", "a+b", "a\0b"] {
            assert!(connection.publish(invalid.into(), Vec::new(), Qos::AtMostOnce, false).is_err());
        }
        for invalid in ["a".repeat(65536), "é".repeat(32768)] {
            assert!(connection.publish(invalid, Vec::new(), Qos::AtMostOnce, false).is_err());
        }
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    }

    #[test]
    fn qos_converts_to_rumqttc_qos() {
        assert_eq!(QoS::from(Qos::AtMostOnce), QoS::AtMostOnce);
        assert_eq!(QoS::from(Qos::AtLeastOnce), QoS::AtLeastOnce);
        assert_eq!(QoS::from(Qos::ExactlyOnce), QoS::ExactlyOnce);
    }

    #[test]
    fn publish_preserves_exact_topics_payload_options() {
        let (connection, mut commands, _) = command_connection();
        let topics = ["/".into(), "$SYS/value".into(), " a//é ".into(), format!("{}a", "é".repeat(32767))];
        for topic in topics {
            for qos in [Qos::AtMostOnce, Qos::AtLeastOnce, Qos::ExactlyOnce] {
                for retain in [false, true] {
                    let payload = vec![0, 0xff, b'\n'];
                    connection.publish(topic.clone(), payload.clone(), qos, retain).unwrap();
                    let Command::Publish(command) = commands.try_recv().unwrap() else {
                        panic!("expected a publish command");
                    };
                    assert_eq!(command.topic, topic);
                    assert_eq!(command.payload, payload);
                    assert_eq!(command.qos, qos.into());
                    assert_eq!(command.retain, retain);
                }
            }
        }
    }

    #[test]
    fn publish_queues_without_a_caller_side_session_check() {
        let (connection, mut commands, _) = command_connection();
        connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).unwrap();
        assert!(matches!(commands.try_recv(), Ok(Command::Publish(_))));
    }

    #[test]
    fn commands_queued_before_connection_report_errors_from_the_worker() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        })
        .unwrap();
        connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).unwrap();
        connection.delete_topics(vec!["test/value".into()]).unwrap();
        match next_event(&runtime, &mut connection) {
            BrokerEvent::PublishError(error) => assert!(error.contains("disconnected")),
            _ => panic!("publish before connection must report a publish error"),
        }
        match next_event(&runtime, &mut connection) {
            BrokerEvent::OperationError(error) => assert!(error.contains("disconnected")),
            _ => panic!("deletion before connection must report an operation error"),
        }
    }

    #[test]
    fn commands_queued_during_reconnect_delay_report_errors_without_replaying() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || drop(handshake(&listener, 0)));
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Status(_)));
        for _ in 0..2 {
            connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).unwrap();
        }
        connection.delete_topics(vec!["test/value".into()]).unwrap();
        for _ in 0..2 {
            match next_event(&runtime, &mut connection) {
                BrokerEvent::PublishError(error) => assert!(error.contains("disconnected")),
                _ => panic!("publish during reconnect must report a publish error"),
            }
        }
        match next_event(&runtime, &mut connection) {
            BrokerEvent::OperationError(error) => assert!(error.contains("disconnected")),
            _ => panic!("deletion during reconnect must report an operation error"),
        }
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn publish_returns_immediately_when_the_shared_queue_is_full_or_closed() {
        let (connection, mut commands, _) = command_connection();
        for _ in 0..COMMAND_CAPACITY {
            connection.delete_topics(vec!["test/value".into()]).unwrap();
        }
        assert!(connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).is_err());
        for _ in 0..COMMAND_CAPACITY {
            commands.try_recv().unwrap();
        }
        for _ in 0..COMMAND_CAPACITY {
            connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).unwrap();
        }
        assert!(connection.delete_topics(vec!["test/value".into()]).is_err());
        drop(commands);
        assert!(connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).is_err());
    }

    #[test]
    fn publish_sends_binary_payload_qos_and_retain_and_reports_queueing_before_broker_ack() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (acknowledge, acknowledged) = std::sync::mpsc::channel();
        let (finished, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            for qos in 0..=2 {
                for retain in [false, true] {
                    let (header, body) = read_packet(&mut stream);
                    assert_eq!(header, 0x30 | (qos << 1) | u8::from(retain));
                    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    assert_eq!(&body[2..2 + topic_len], "test/é".as_bytes());
                    let offset = 2 + topic_len;
                    let payload_offset = offset + if qos == 0 { 0 } else { 2 };
                    assert_eq!(&body[payload_offset..], &[0, 0xff, b'\n']);
                    // The UI must be able to finish its pending state before any ack.
                    acknowledged.recv_timeout(Duration::from_secs(5)).unwrap();
                    if qos != 0 {
                        let pkid = &body[offset..offset + 2];
                        stream
                            .write_all(&[if qos == 1 { 0x40 } else { 0x50 }, 2, pkid[0], pkid[1]])
                            .unwrap();
                        if qos == 2 {
                            assert_eq!(read_packet(&mut stream), (0x62, pkid.to_vec()));
                            stream.write_all(&[0x70, 2, pkid[0], pkid[1]]).unwrap();
                        }
                    }
                    finished.send(()).unwrap();
                }
            }
            // TCP orders this marker after the final PUBCOMP. Receiving it
            // proves the worker drained the ack before drop, avoiding a reset
            // caused by closing a socket with unread incoming data.
            stream.write_all(b"\x30\x16\x00\x10test/ack-barrierdone").unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        for qos in [Qos::AtMostOnce, Qos::AtLeastOnce, Qos::ExactlyOnce] {
            for retain in [false, true] {
                connection.publish("test/é".into(), vec![0, 0xff, b'\n'], qos, retain).unwrap();
                assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
                acknowledge.send(()).unwrap();
                received.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }
        match next_event(&runtime, &mut connection) {
            BrokerEvent::Message(message) => {
                assert_eq!(message.topic, "test/ack-barrier");
                assert_eq!(message.payload.as_ref(), b"done");
            }
            _ => panic!("expected the shutdown barrier, not a publish acknowledgement or error"),
        }
        assert!(matches!(connection.events.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn publish_worker_failure_reports_publish_error_without_changing_the_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (finished, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            assert_eq!(read_packet(&mut stream), (0x30, b"\x00\x0atest/value\x00\xff".to_vec()));
            finished.send(()).unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        // Bypass validation to exercise rumqttc's failure rather than the API's.
        assert!(
            connection
                .commands
                .try_send(Command::Publish(PublishCommandData {
                    topic: "#".into(),
                    payload: Vec::new(),
                    qos: QoS::AtMostOnce,
                    retain: false,
                }))
                .is_ok()
        );
        match next_event(&runtime, &mut connection) {
            BrokerEvent::PublishError(error) => assert!(error.contains("Could not queue publish")),
            _ => panic!("publishing failure must not report a deletion or status error"),
        }
        connection
            .publish("test/value".into(), vec![0, 0xff], Qos::AtMostOnce, false)
            .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn disconnect_discards_unacknowledged_publishes_and_qos_two_releases_without_deletion_errors() {
        for (qos, qos_level) in [(Qos::AtLeastOnce, 1), (Qos::ExactlyOnce, 2)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (disconnect, disconnected) = std::sync::mpsc::channel();
            let broker = std::thread::spawn(move || {
                let mut stream = handshake(&listener, 0);
                let (header, body) = read_packet(&mut stream);
                assert_eq!(header, 0x31 | (qos_level << 1));
                // An empty retained publish is still a user publish, not a deletion command.
                assert_eq!(&body[2..12], b"test/value");
                assert_eq!(body.len(), 14);
                if qos_level == 2 {
                    stream.write_all(&[0x50, 2, body[12], body[13]]).unwrap();
                    assert_eq!(read_packet(&mut stream), (0x62, body[12..14].to_vec()));
                }
                disconnected.recv_timeout(Duration::from_secs(5)).unwrap();
                drop(stream);
                let mut stream = handshake(&listener, 0);
                assert_eq!(stream.read(&mut [0]).unwrap(), 0, "publishes and releases must not replay");
            });
            let runtime = runtime();
            let mut connection = connect(ConnectionConfig {
                host: "127.0.0.1".into(),
                port,
                ..Default::default()
            })
            .unwrap();
            next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
            connection.publish("test/value".into(), Vec::new(), qos, true).unwrap();
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
            disconnect.send(()).unwrap();
            match next_event(&runtime, &mut connection) {
                BrokerEvent::PublishError(error) => assert!(error.contains("will not be retried")),
                _ => panic!("a disconnected publish must report a publish error, not a deletion error"),
            }
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Status(_)));
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connecting));
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
            drop(connection);
            broker.join().unwrap();
        }
    }

    #[test]
    fn delete_topics_queues_for_the_current_connection_and_validates_topics() {
        let (connection, mut commands, _) = command_connection();
        assert!(connection.delete_topics(vec!["#".into()]).is_err());
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        connection.delete_topics(vec!["test/value".into()]).unwrap();
        let Command::DeleteTopics(command) = commands.try_recv().unwrap() else {
            panic!("expected a retained deletion command");
        };
        assert_eq!(command.topics, vec!["test/value"]);
    }

    #[test]
    fn delete_topics_queues_without_a_caller_side_session_check() {
        let (connection, mut commands, _) = command_connection();
        connection.delete_topics(vec!["test/value".into()]).unwrap();
        let Command::DeleteTopics(command) = commands.try_recv().unwrap() else {
            panic!("expected a retained deletion command");
        };
        assert_eq!(command.topics, vec!["test/value"]);
    }

    #[test]
    fn delete_topics_validates_the_entire_batch_before_queueing() {
        let (connection, mut commands, _) = command_connection();
        for invalid in ["", "#", "a/+", "a#b", "a+b", "a\0b"] {
            assert!(connection.delete_topics(vec!["valid".into(), invalid.into()]).is_err());
            assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
        // MQTT's two-byte topic length is in UTF-8 bytes, not characters.
        for invalid in ["a".repeat(65536), "é".repeat(32768)] {
            assert!(connection.delete_topics(vec![invalid]).is_err());
            assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
    }

    #[test]
    fn delete_topics_accepts_exact_topics_and_the_maximum_byte_length() {
        let (connection, mut commands, _) = command_connection();
        let topics = vec!["/".into(), "$SYS/value".into(), "a//é".into(), "a".repeat(65535)];
        connection.delete_topics(topics.clone()).unwrap();
        let Command::DeleteTopics(command) = commands.try_recv().unwrap() else {
            panic!("expected a retained deletion command");
        };
        assert_eq!(command.topics, topics);
    }

    #[test]
    fn delete_topics_returns_immediately_when_the_command_queue_is_full_or_closed() {
        let (connection, mut commands, _) = command_connection();
        for _ in 0..COMMAND_CAPACITY {
            connection.delete_topics(vec!["test/value".into()]).unwrap();
        }
        assert!(connection.delete_topics(vec!["test/value".into()]).is_err());
        for _ in 0..COMMAND_CAPACITY {
            commands.try_recv().unwrap();
        }
        drop(commands);
        assert!(connection.delete_topics(vec!["test/value".into()]).is_err());
    }

    #[test]
    fn delete_topics_empty_batches_are_noops() {
        let (connection, mut commands, _) = command_connection();
        connection.delete_topics(Vec::new()).unwrap();
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    }

    #[test]
    fn publishes_empty_retained_qos_zero_to_exact_topics_in_a_batch_larger_than_client_queue() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let topics: Vec<_> = (0..64).map(|index| format!("test/é/{index}")).collect();
        let expected = topics.clone();
        let (finished, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            for topic in expected {
                let (header, body) = read_packet(&mut stream);
                assert_eq!(header, 0x31, "PUBLISH must be QoS 0, retained, and not DUP");
                let length = u16::from_be_bytes([body[0], body[1]]) as usize;
                assert_eq!(&body[2..2 + length], topic.as_bytes());
                assert_eq!(body.len(), 2 + length, "deletion payload must be empty");
            }
            finished.send(()).unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        connection.delete_topics(topics).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(connection.events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "QoS 0 must not generate a success acknowledgement"
        );
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn disconnect_interrupts_an_active_batch_without_replaying_deletions() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            assert_eq!(read_packet(&mut stream).0, 0x31);
            drop(stream);
            let mut stream = handshake(&listener, 0);
            assert_eq!(stream.read(&mut [0]).unwrap(), 0, "deletions must not be replayed");
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        connection.delete_topics(vec!["test/value".into(); 100_000]).unwrap();
        match next_event(&runtime, &mut connection) {
            BrokerEvent::OperationError(error) => assert!(error.contains("will not be retried")),
            _ => panic!("an interrupted batch must report an operation error"),
        }
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn publishing_failure_reports_an_operation_error_without_changing_connection_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (finished, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            assert_eq!(read_packet(&mut stream), (0x31, b"\x00\x0atest/value".to_vec()));
            finished.send(()).unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        // Bypass API validation to force rumqttc's publish queue to reject a request.
        assert!(
            connection
                .commands
                .try_send(Command::DeleteTopics(DeleteTopicsData { topics: vec!["#".into()] }))
                .is_ok()
        );
        match next_event(&runtime, &mut connection) {
            BrokerEvent::OperationError(error) => assert!(error.contains("Could not queue")),
            _ => panic!("a publishing failure must not emit a connection status"),
        }
        connection.delete_topics(vec!["test/value".into()]).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn dropping_connection_cancels_queued_commands_and_signals_stop() {
        let (connection, mut commands, mut stopped) = command_connection();
        connection.delete_topics(vec!["test/value".into()]).unwrap();
        drop(connection);
        assert_eq!(stopped.try_recv(), Ok(()));
        commands.try_recv().unwrap();
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)));
    }

    #[test]
    fn dropping_connection_interrupts_an_active_large_batch() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (started, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            assert_eq!(read_packet(&mut stream).0, 0x31);
            started.send(()).unwrap();
            let mut buffer = [0; 8192];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(error) => panic!("worker did not stop after drop: {error}"),
                }
            }
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        connection.delete_topics(vec!["test/value".into(); 100_000]).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn publish_clear_retained_returns_queue_failure_when_event_loop_is_gone() {
        let (client, event_loop) = AsyncClient::new(MqttOptions::new("test", "localhost", 1883), 16);
        drop(event_loop);
        let error = runtime()
            .block_on(publish_clear_retained(client, vec!["test/value".into()], Rc::default()))
            .unwrap_err();
        assert!(error.to_string().contains("Could not queue"));
    }

    #[test]
    fn tls_transport_returns_an_error_when_no_trusted_certificates_are_available() {
        assert_eq!(
            tls_transport(RootCertStore::empty())
                .err()
                .expect("TLS without trusted roots must fail")
                .to_string(),
            "Could not load trusted system certificates for TLS."
        );
    }
}
