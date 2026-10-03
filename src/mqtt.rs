//! MQTT I/O on a dedicated runtime, with bounded delivery and drop-based cancellation.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
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
    /// An operation failed; this does not change the connection status.
    OperationError(String),
    Message(Message),
}

/// Receives broker events and cancels the MQTT worker when dropped.
pub struct Connection {
    pub events: mpsc::Receiver<BrokerEvent>,
    stop: Option<oneshot::Sender<()>>,
    commands: mpsc::Sender<ClearRetained>,
    session: Arc<AtomicU64>,
}

/// Opaque connection identity and connected generation captured before a deletion prompt.
/// Holding this token does not keep the MQTT worker alive.
#[derive(Clone)]
pub struct RetainedClearSession {
    connection: Arc<AtomicU64>,
    generation: u64,
}

const COMMAND_CAPACITY: usize = 16;

struct ClearRetained {
    topics: Vec<String>,
    session: u64,
}

impl Connection {
    /// Captures the current connected session for a retained-deletion confirmation prompt.
    /// Returns `None` while disconnected or after the command receiver has closed.
    pub fn retained_clear_session(&self) -> Option<RetainedClearSession> {
        let generation = self.session.load(Ordering::Acquire);
        (generation % 2 == 1 && !self.commands.is_closed()).then(|| RetainedClearSession {
            connection: Arc::clone(&self.session),
            generation,
        })
    }

    /// Queues user-confirmed retained-message deletion at exact topic names only
    /// if this is still the connection and session captured before confirmation.
    /// A disconnect/reconnect or replacement connection requires a new token and
    /// confirmation, even for an empty batch.
    ///
    /// Returns immediately, with an error for an expired token, invalid topics,
    /// a disconnected worker, or a full/closed command queue. With a current token,
    /// an empty batch is a no-op. Later failures are delivered as
    /// `BrokerEvent::OperationError`. QoS 0 provides no acknowledgement: `Ok(())`
    /// means queued, not that the broker deleted anything. Batches may be partially
    /// sent before cancellation and are never retried after disconnect.
    pub fn clear_retained_in_session(&self, topics: Vec<String>, token: &RetainedClearSession) -> Result<()> {
        ensure!(
            Arc::ptr_eq(&self.session, &token.connection)
                && token.generation % 2 == 1
                && self.session.load(Ordering::Acquire) == token.generation,
            "Retained-topic deletion confirmation expired because the connection changed. Confirm again while connected."
        );
        self.clear_retained_for_generation(topics, token.generation)
    }

    /// Test-only convenience for queueing deletion without a confirmation token.
    #[cfg(test)]
    pub fn clear_retained(&self, topics: Vec<String>) -> Result<()> {
        self.clear_retained_for_generation(topics, self.session.load(Ordering::Acquire))
    }

    fn clear_retained_for_generation(&self, topics: Vec<String>, session: u64) -> Result<()> {
        for topic in &topics {
            ensure!(!topic.is_empty(), "Retained topic names must not be empty.");
            ensure!(
                !topic.contains(['#', '+']),
                "Retained deletion requires exact topic names, not MQTT wildcards."
            );
            ensure!(
                topic.len() <= u16::MAX as usize,
                "MQTT topic names must not exceed 65535 UTF-8 bytes."
            );
            ensure!(!topic.contains('\0'), "MQTT topic names must not contain a null character.");
        }
        if topics.is_empty() {
            return Ok(());
        }
        ensure!(session % 2 == 1, "Cannot delete retained topics while disconnected.");
        ensure!(
            self.session.load(Ordering::Acquire) == session,
            "Retained-topic deletion confirmation expired because the connection changed. Confirm again while connected."
        );
        // Preserve the captured generation in the command so the worker also
        // rejects a disconnect/reconnect racing with validation or queueing.
        self.commands
            .try_send(ClearRetained { topics, session })
            .context("Could not queue retained-topic deletion (queue full or worker stopped).")
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
    // Odd generations are connected; even generations are disconnected.
    let session = Arc::new(AtomicU64::new(0));
    let worker_session = Arc::clone(&session);
    std::thread::Builder::new()
        .name("mqtt".into())
        .spawn(move || {
            runtime.block_on(async {
                let result = tokio::select! {
                    _ = stopped => return,
                    result = run(config, &sender, command_receiver, &worker_session) => result,
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
        session,
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

async fn publish_clear_retained(client: AsyncClient, topics: Vec<String>) -> Result<()> {
    for topic in topics {
        client
            .publish(topic, QoS::AtMostOnce, true, Vec::new())
            .await
            .context("Could not queue a retained-topic deletion publish.")?;
    }
    Ok(())
}

async fn run(
    config: ConnectionConfig,
    sender: &mpsc::Sender<BrokerEvent>,
    mut commands: mpsc::Receiver<ClearRetained>,
    session: &AtomicU64,
) -> Result<()> {
    let mut options = MqttOptions::new(config.client_id, config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_session(true);
    options.set_max_packet_size(16 * 1024 * 1024, 16 * 1024 * 1024);
    if !config.username.is_empty() {
        options.set_credentials(config.username, config.password);
    }
    if config.tls {
        // The default transport helper panics when platform certificate loading fails.
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        options.set_transport(tls_transport(roots)?);
    }
    let (client, mut event_loop) = AsyncClient::new(options, 16);
    event_loop.network_options.set_connection_timeout(10);
    let mut operation = None;
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
                        let current = session.load(Ordering::Acquire);
                        if current.is_multiple_of(2) || command.session != current {
                            if sender.send(BrokerEvent::OperationError(
                                "Retained-topic deletion cancelled because the connection changed. Confirm again while connected.".into(),
                            )).await.is_err() {
                                return Ok(());
                            }
                        } else {
                            operation = Some(Box::pin(publish_clear_retained(client.clone(), command.topics)));
                        }
                    }
                    result = async {
                        match operation.as_mut() {
                            Some(operation) => operation.await,
                            None => std::future::pending().await,
                        }
                    } => {
                        operation = None;
                        if let Err(error) = result
                            && sender.send(BrokerEvent::OperationError(format!("{error:#}"))).await.is_err()
                        {
                            return Ok(());
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
                    if session.load(Ordering::Acquire).is_multiple_of(2) {
                        session.fetch_add(1, Ordering::AcqRel);
                    }
                    BrokerEvent::Connected
                }
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
                if session.load(Ordering::Acquire) % 2 == 1 {
                    session.fetch_add(1, Ordering::AcqRel);
                }
                let interrupted = operation.take().is_some();
                // rumqttc normally saves queued requests for reconnect. Deletions
                // are destructive, so also purge requests already handed to it.
                event_loop.clean();
                let pending_deletions = event_loop
                    .pending
                    .iter()
                    .any(|request| matches!(request, rumqttc::Request::Publish(_)));
                event_loop
                    .pending
                    .retain(|request| !matches!(request, rumqttc::Request::Publish(_)));
                if (interrupted || pending_deletions)
                    && sender.send(BrokerEvent::OperationError(
                        "Retained-topic deletion interrupted by disconnect; some topics may already have been cleared. It will not be retried.".into(),
                    )).await.is_err()
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
                tokio::time::sleep(Duration::from_secs(2)).await;
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
                BrokerEvent::Status(status) | BrokerEvent::OperationError(status) => {
                    panic!("expected retained message, got: {status}")
                }
                BrokerEvent::Connected | BrokerEvent::Connecting => {
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
            BrokerEvent::Message(_) | BrokerEvent::Connecting | BrokerEvent::OperationError(_) => false,
        });
        assert!(matches!(event, BrokerEvent::Status(_)), "a refused subscription is not connected");
        assert!(connection.retained_clear_session().is_none());
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
                BrokerEvent::Message(_) | BrokerEvent::Connecting | BrokerEvent::OperationError(_) => false,
            });
            assert!(matches!(event, BrokerEvent::Status(_)), "an incomplete SUBACK is not connected");
            assert!(connection.retained_clear_session().is_none());
            drop(connection);
            broker.join().unwrap();
        }
    }

    fn command_connection() -> (Connection, mpsc::Receiver<ClearRetained>, oneshot::Receiver<()>) {
        let (_, events) = mpsc::channel(1);
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (stop, stopped) = oneshot::channel();
        (
            Connection {
                events,
                stop: Some(stop),
                commands,
                session: Arc::new(AtomicU64::new(1)),
            },
            receiver,
            stopped,
        )
    }

    #[test]
    fn retained_clear_session_is_available_only_while_connected_with_a_live_queue() {
        let (connection, commands, _) = command_connection();
        assert!(connection.retained_clear_session().is_some());
        for generation in [0, 2] {
            connection.session.store(generation, Ordering::Release);
            assert!(connection.retained_clear_session().is_none());
        }
        connection.session.store(3, Ordering::Release);
        assert!(connection.retained_clear_session().is_some());
        drop(commands);
        assert!(connection.retained_clear_session().is_none());
    }

    #[test]
    fn clear_retained_in_session_queues_the_confirmed_generation_and_validates_topics() {
        let (connection, mut commands, _) = command_connection();
        let token = connection.retained_clear_session().unwrap();
        assert!(connection.clear_retained_in_session(vec!["#".into()], &token).is_err());
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        connection.clear_retained_in_session(vec!["test/value".into()], &token).unwrap();
        let command = commands.try_recv().unwrap();
        assert_eq!(command.topics, vec!["test/value"]);
        assert_eq!(command.session, 1);
    }

    #[test]
    fn clear_retained_in_session_rejects_tokens_after_disconnect_and_reconnect() {
        let (connection, mut commands, _) = command_connection();
        let token = connection.retained_clear_session().unwrap();
        for generation in [2, 3] {
            connection.session.store(generation, Ordering::Release);
            for topics in [vec!["test/value".into()], Vec::new()] {
                assert!(connection.clear_retained_in_session(topics, &token).is_err());
            }
            assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
        let fresh_token = connection.retained_clear_session().unwrap();
        connection
            .clear_retained_in_session(vec!["test/value".into()], &fresh_token)
            .unwrap();
        assert_eq!(commands.try_recv().unwrap().session, 3);
    }

    #[test]
    fn clear_retained_in_session_rejects_tokens_from_a_replaced_connection() {
        let (original, _original_commands, _) = command_connection();
        let token = original.retained_clear_session().unwrap();
        drop(original);
        let (replacement, mut commands, _) = command_connection();
        assert_eq!(replacement.session.load(Ordering::Acquire), token.generation);
        assert!(replacement.clear_retained_in_session(vec!["test/value".into()], &token).is_err());
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        let fresh_token = replacement.retained_clear_session().unwrap();
        replacement
            .clear_retained_in_session(vec!["test/value".into()], &fresh_token)
            .unwrap();
        assert_eq!(commands.try_recv().unwrap().session, token.generation);
    }

    #[test]
    fn clear_retained_validates_the_entire_batch_before_queueing() {
        let (connection, mut commands, _) = command_connection();
        for invalid in ["", "#", "a/+", "a#b", "a+b", "a\0b"] {
            assert!(connection.clear_retained(vec!["valid".into(), invalid.into()]).is_err());
            assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
        // MQTT's two-byte topic length is in UTF-8 bytes, not characters.
        for invalid in ["a".repeat(65536), "é".repeat(32768)] {
            assert!(connection.clear_retained(vec![invalid]).is_err());
            assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
    }

    #[test]
    fn clear_retained_accepts_exact_topics_and_the_maximum_byte_length() {
        let (connection, mut commands, _) = command_connection();
        let topics = vec!["/".into(), "$SYS/value".into(), "a//é".into(), "a".repeat(65535)];
        connection.clear_retained(topics.clone()).unwrap();
        let command = commands.try_recv().unwrap();
        assert_eq!(command.topics, topics);
        assert_eq!(command.session, 1);
    }

    #[test]
    fn clear_retained_returns_immediately_when_the_command_queue_is_full_or_closed() {
        let (connection, mut commands, _) = command_connection();
        for _ in 0..COMMAND_CAPACITY {
            connection.clear_retained(vec!["test/value".into()]).unwrap();
        }
        assert!(connection.clear_retained(vec!["test/value".into()]).is_err());
        for _ in 0..COMMAND_CAPACITY {
            commands.try_recv().unwrap();
        }
        drop(commands);
        assert!(connection.clear_retained(vec!["test/value".into()]).is_err());
    }

    #[test]
    fn clear_retained_rejects_disconnected_calls_and_empty_batches_are_noops() {
        let (connection, mut commands, _) = command_connection();
        connection.session.store(0, Ordering::Release);
        assert!(connection.clear_retained(vec!["test/value".into()]).is_err());
        connection.clear_retained(Vec::new()).unwrap();
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
        connection.clear_retained(topics).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(connection.events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "QoS 0 must not generate a success acknowledgement"
        );
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn stale_commands_report_operation_errors_and_are_not_published_after_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (disconnect, disconnected) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let stream = handshake(&listener, 0);
            disconnected.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(stream);
            let mut stream = handshake(&listener, 0);
            // Any stale PUBLISH before or after SUBACK fails this assertion.
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
        let old_session = connection.session.load(Ordering::Acquire);
        disconnect.send(()).unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Status(_)));
        assert!(connection.clear_retained(vec!["test/value".into()]).is_err());
        // Simulate a command queued just as the worker detects a disconnect.
        assert!(
            connection
                .commands
                .try_send(ClearRetained {
                    topics: vec!["test/value".into()],
                    session: old_session,
                })
                .is_ok()
        );
        let mut connected = false;
        let mut operation_error = false;
        while !connected || !operation_error {
            match next_event(&runtime, &mut connection) {
                BrokerEvent::Connected => connected = true,
                BrokerEvent::OperationError(error) => {
                    assert!(error.contains("connection changed"));
                    operation_error = true;
                }
                BrokerEvent::Connecting => {}
                _ => panic!("unexpected broker event during reconnect"),
            }
        }
        assert_eq!(connection.session.load(Ordering::Acquire), old_session + 2);
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
        connection.clear_retained(vec!["test/value".into(); 100_000]).unwrap();
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
        let session = connection.session.load(Ordering::Acquire);
        // Bypass API validation to force rumqttc's publish queue to reject a request.
        assert!(
            connection
                .commands
                .try_send(ClearRetained {
                    topics: vec!["#".into()],
                    session,
                })
                .is_ok()
        );
        match next_event(&runtime, &mut connection) {
            BrokerEvent::OperationError(error) => assert!(error.contains("Could not queue")),
            _ => panic!("a publishing failure must not emit a connection status"),
        }
        assert_eq!(connection.session.load(Ordering::Acquire), session);
        connection.clear_retained(vec!["test/value".into()]).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn dropping_connection_cancels_queued_commands_and_signals_stop() {
        let (connection, mut commands, mut stopped) = command_connection();
        connection.clear_retained(vec!["test/value".into()]).unwrap();
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
        connection.clear_retained(vec!["test/value".into(); 100_000]).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn publish_clear_retained_returns_queue_failure_when_event_loop_is_gone() {
        let (client, event_loop) = AsyncClient::new(MqttOptions::new("test", "localhost", 1883), 16);
        drop(event_loop);
        let error = runtime()
            .block_on(publish_clear_retained(client, vec!["test/value".into()]))
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
