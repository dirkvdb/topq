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

use rumqttc::tokio_rustls::rustls::{
    self, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, aws_lc_rs, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use rumqttc::v5::{
    AsyncClient, Event, MqttOptions,
    mqttbytes::{
        QoS,
        v5::{
            ConnAckProperties, Filter, Packet, PubAckReason, PubCompReason, PubRecReason, PublishProperties, Subscribe, SubscribeReasonCode,
        },
    },
};
use rumqttc::{Outgoing, Transport};
use tokio::sync::{mpsc, oneshot};

use crate::{
    config::ConnectionConfig,
    topics::{Message, MessageProperties},
};

pub enum BrokerEvent {
    Connecting,
    Connected,
    Status(String),
    OperationError(String),
    PublishQueued,
    PublishError(String),
    Message(Message),
}

/// Queues broker commands and cancels the MQTT worker when dropped.
pub struct Connection {
    events: Option<mpsc::Receiver<BrokerEvent>>,
    stop: Option<oneshot::Sender<()>>,
    commands: mpsc::Sender<Command>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    Connected,
    Disconnected,
}

/// Quality of service for an MQTT publish.
#[expect(
    clippy::enum_variant_names,
    reason = "Variants use the standard MQTT delivery-guarantee terminology"
)]
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
pub(crate) const MAX_PACKET_SIZE: u32 = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
struct PublishCapabilities {
    max_qos: u8,
    retain_available: bool,
    max_packet_size: u32,
}

impl PublishCapabilities {
    fn from_connack(properties: Option<&ConnAckProperties>) -> Self {
        Self {
            max_qos: properties.and_then(|properties| properties.max_qos).unwrap_or(2),
            retain_available: properties.and_then(|properties| properties.retain_available) != Some(0),
            max_packet_size: properties
                .and_then(|properties| properties.max_packet_size)
                .unwrap_or(MAX_PACKET_SIZE)
                .min(MAX_PACKET_SIZE),
        }
    }

    fn validate_publish(&self, topic: &str, payload_len: usize, qos: QoS, retain: bool, content_type: Option<&str>) -> Result<()> {
        ensure!(
            qos as u8 <= self.max_qos,
            "Broker supports a maximum publish QoS of {}.",
            self.max_qos
        );
        ensure!(!retain || self.retain_available, "Broker does not support retained publishes.");
        ensure_publish_packet_size(topic, payload_len, qos, content_type, self.max_packet_size)
    }

    fn validate_command(&self, command: &Command) -> Result<()> {
        match command {
            Command::Publish(command) => self.validate_publish(
                &command.topic,
                command.payload.len(),
                command.qos,
                command.retain,
                command.content_type.as_deref(),
            ),
            Command::DeleteTopics(command) => {
                // Check the whole batch before clearing anything on the broker.
                for topic in &command.topics {
                    self.validate_publish(topic, 0, QoS::AtMostOnce, true, None)?;
                }
                Ok(())
            }
        }
    }
}

struct DeleteTopicsData {
    topics: Vec<String>,
}

struct PublishCommandData {
    topic: String,
    payload: Vec<u8>,
    qos: QoS,
    retain: bool,
    content_type: Option<String>,
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

fn ensure_valid_content_type(content_type: &str) -> Result<()> {
    ensure!(
        content_type.len() <= u16::MAX as usize,
        "MQTT content type must not exceed 65535 UTF-8 bytes."
    );
    ensure!(!content_type.contains('\0'), "MQTT content type must not contain a null character.");
    Ok(())
}

fn variable_integer_len(mut length: usize) -> usize {
    let mut length_bytes = 1;
    while length >= 128 {
        length_bytes += 1;
        length /= 128;
    }
    length_bytes
}

fn ensure_publish_packet_size(topic: &str, payload_len: usize, qos: QoS, content_type: Option<&str>, max_packet_size: u32) -> Result<()> {
    // Content type includes its property ID and two-byte UTF-8 string length.
    let properties_length = content_type.map_or(0, |value| 1 + 2 + value.len());
    let remaining_length = 2
        + topic.len()
        + usize::from(qos != QoS::AtMostOnce) * 2
        + variable_integer_len(properties_length)
        + properties_length
        + payload_len;
    ensure!(
        1 + variable_integer_len(remaining_length) + remaining_length <= max_packet_size as usize,
        "MQTT publish packet exceeds the maximum packet size of {max_packet_size} bytes."
    );
    Ok(())
}

impl Connection {
    /// Transfers sole ownership of the broker event receiver to the caller.
    /// Returns `None` after the receiver has been taken once. Taking the receiver
    /// leaves this command handle alive; dropping `Connection` still cancels the worker.
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<BrokerEvent>> {
        self.events.take()
    }

    /// Queues a publish to an exact MQTT topic.
    /// Invalid topics and full/closed queues fail immediately. `Ok(())` means
    /// accepted into the command queue, not delivered. The worker reports
    /// `BrokerEvent::PublishQueued` or `BrokerEvent::PublishError`; a disconnect
    /// can produce an error even after `PublishQueued`.
    pub fn publish(&self, topic: String, payload: Vec<u8>, qos: Qos, retain: bool) -> Result<()> {
        self.publish_with_content_type(topic, payload, qos, retain, None)
    }

    /// Queues unmodified payload bytes with an optional MQTT 5 content type.
    /// `None` omits the property; `Some` preserves the string exactly, including
    /// an empty string. Content types must fit in 65535 UTF-8 bytes and contain
    /// no null characters. No payload-format indicator is set.
    /// Validation and queue/delivery semantics are the same as [`Self::publish`].
    pub fn publish_with_content_type(
        &self,
        topic: String,
        payload: Vec<u8>,
        qos: Qos,
        retain: bool,
        content_type: Option<String>,
    ) -> Result<()> {
        ensure_valid_topic(&topic)?;
        if let Some(content_type) = content_type.as_deref() {
            ensure_valid_content_type(content_type)?;
        }
        ensure_publish_packet_size(&topic, payload.len(), qos.into(), content_type.as_deref(), MAX_PACKET_SIZE)?;

        self.commands
            .try_send(Command::Publish(PublishCommandData {
                topic,
                payload,
                qos: qos.into(),
                retain,
                content_type,
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
                    tracing::error!(error = %format_args!("{error:#}"), "MQTT worker stopped");
                    let _ = sender.send(BrokerEvent::Status(format!("{error:#}"))).await;
                }
            });
        })
        .context("Could not start the MQTT worker thread.")?;
    Ok(Connection {
        events: Some(events),
        stop: Some(stop),
        commands,
    })
}

#[derive(Debug)]
struct UncheckedServerCertVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for UncheckedServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Opting out removes server identity checks, not proof of private-key possession.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_transport(roots: RootCertStore, validate_certificate: bool, websocket: bool) -> Result<Transport> {
    if validate_certificate {
        ensure!(!roots.is_empty(), "Could not load trusted system certificates for TLS.");
    }
    let provider = Arc::new(aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .context("Could not configure the TLS protocol versions.")?;
    let config = if validate_certificate {
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(UncheckedServerCertVerifier { provider }))
            .with_no_client_auth()
    };
    Ok(if websocket {
        Transport::wss_with_config(config.into())
    } else {
        Transport::tls_with_config(config.into())
    })
}

fn mqtt_options(config: &ConnectionConfig) -> Result<MqttOptions> {
    // rumqttc requires the full URL for WebSocket and reads its port from that URL.
    let host = if config.websocket {
        let scheme = if config.tls { "wss" } else { "ws" };
        let host = &config.host;
        if host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("{scheme}://[{host}]:{}/mqtt", config.port)
        } else {
            format!("{scheme}://{host}:{}/mqtt", config.port)
        }
    } else {
        config.host.clone()
    };

    let mut options = MqttOptions::new(&config.client_id, host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_start(true);
    options.set_connection_timeout(10);
    options.set_outgoing_inflight_upper_limit(100);
    options.set_max_packet_size(Some(MAX_PACKET_SIZE));
    // rumqttc 0.25 emits publishes before resolving aliases. Do not negotiate
    // aliases until those events reliably contain the resolved topic name.
    options.set_topic_alias_max(Some(0));
    if !config.username.is_empty() {
        options.set_credentials(&config.username, &config.password);
    }
    if config.tls {
        // The default transport helper panics when platform certificate loading fails.
        let mut roots = RootCertStore::empty();
        if config.validate_certificate {
            roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        }
        options.set_transport(tls_transport(roots, config.validate_certificate, config.websocket)?);
    } else if config.websocket {
        options.set_transport(Transport::ws());
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

async fn queue_command(
    client: AsyncClient,
    command: Command,
    queued: Rc<RefCell<VecDeque<OperationKind>>>,
    capabilities: PublishCapabilities,
) -> Result<()> {
    capabilities.validate_command(&command)?;
    match command {
        Command::DeleteTopics(command) => publish_clear_retained(client, command.topics, queued).await,
        Command::Publish(command) => {
            queued.borrow_mut().push_back(OperationKind::Publish);
            let result = match command.content_type {
                Some(content_type) => {
                    let properties = PublishProperties {
                        content_type: Some(content_type),
                        ..Default::default()
                    };
                    client
                        .publish_with_properties(command.topic, command.qos, command.retain, command.payload, properties)
                        .await
                }
                None => client.publish(command.topic, command.qos, command.retain, command.payload).await,
            };
            if let Err(error) = result {
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

fn validate_incoming_event(event: Event) -> Result<Event> {
    match &event {
        Event::Incoming(Packet::Publish(publish)) => {
            ensure!(
                publish.properties.as_ref().and_then(|properties| properties.topic_alias).is_none(),
                "Broker sent a topic alias although the negotiated alias maximum is zero."
            );
            let topic = std::str::from_utf8(&publish.topic).context("Broker sent a non-UTF-8 MQTT topic.")?;
            ensure_valid_topic(topic).context("Broker sent an invalid MQTT topic.")?;
        }
        // rumqttc normally turns DISCONNECT into StateError::ServerDisconnect.
        // Handle a delivered packet too, rather than leaving the session connected.
        Event::Incoming(Packet::Disconnect(disconnect)) => {
            anyhow::bail!(
                "Broker disconnected: {:?}{}",
                disconnect.reason_code,
                disconnect
                    .properties
                    .as_ref()
                    .and_then(|properties| properties.reason_string.as_deref())
                    .map(|reason| format!(" · {reason}"))
                    .unwrap_or_default()
            );
        }
        _ => {}
    }
    Ok(event)
}

fn publish_rejection(packet: &Packet) -> Option<(u16, String)> {
    let (pkid, acknowledgement, reason, detail) = match packet {
        Packet::PubAck(ack) if !matches!(ack.reason, PubAckReason::Success | PubAckReason::NoMatchingSubscribers) => (
            ack.pkid,
            "PUBACK",
            format!("{:?}", ack.reason),
            ack.properties.as_ref().and_then(|properties| properties.reason_string.as_deref()),
        ),
        Packet::PubRec(ack) if !matches!(ack.reason, PubRecReason::Success | PubRecReason::NoMatchingSubscribers) => (
            ack.pkid,
            "PUBREC",
            format!("{:?}", ack.reason),
            ack.properties.as_ref().and_then(|properties| properties.reason_string.as_deref()),
        ),
        Packet::PubComp(ack) if ack.reason != PubCompReason::Success => (
            ack.pkid,
            "PUBCOMP",
            format!("{:?}", ack.reason),
            ack.properties.as_ref().and_then(|properties| properties.reason_string.as_deref()),
        ),
        _ => return None,
    };
    let detail = detail.map(|reason| format!(" · {reason}")).unwrap_or_default();
    Some((pkid, format!("Broker rejected publish ({acknowledgement}: {reason}){detail}.")))
}

async fn run(config: ConnectionConfig, sender: &mpsc::Sender<BrokerEvent>, mut commands: mpsc::Receiver<Command>) -> Result<()> {
    let options = mqtt_options(&config)?;
    let mut session = SessionState::Disconnected;
    let mut capabilities = PublishCapabilities::from_connack(None);
    let (mut client, mut event_loop) = AsyncClient::new(options.clone(), 16);
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
                            operation = Some((kind, Box::pin(queue_command(client.clone(), command, Rc::clone(&queued), capabilities))));
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
        let mut result = result.map_err(anyhow::Error::from).and_then(validate_incoming_event);
        if let Ok(Event::Incoming(packet)) = &result
            && let Some((pkid, error)) = publish_rejection(packet)
        {
            if inflight_publishes.remove(&pkid) && sender.send(BrokerEvent::PublishError(error)).await.is_err() {
                return Ok(());
            }
            // rumqttc 0.25 does not release its inflight slot on a negative
            // QoS 2 acknowledgement, or resolve a QoS 1 packet-ID collision.
            // Reset these sessions to avoid stalled requests.
            if matches!(packet, Packet::PubRec(_) | Packet::PubComp(_))
                || event_loop.state.collision.as_ref().is_some_and(|publish| publish.pkid == pkid)
            {
                result = Err(anyhow::anyhow!("MQTT publish rejected; resetting the session."));
            }
        }
        let event = match result {
            Ok(Event::Incoming(Packet::ConnAck(ack))) => {
                capabilities = PublishCapabilities::from_connack(ack.properties.as_ref());
                if ack.properties.as_ref().and_then(|properties| properties.server_keep_alive) == Some(0) {
                    // rumqttc's private timer is already due and cannot be disabled.
                    // Optional PINGREQs are valid even with keepalive zero; restore
                    // our interval so the next timer reset cannot create a busy loop.
                    event_loop.options.set_keep_alive(options.keep_alive());
                }
                pending_subscription = None;
                let filters = config.topics.iter().map(|subscription| {
                    let qos = match subscription.qos {
                        0 => QoS::AtMostOnce,
                        1 => QoS::AtLeastOnce,
                        _ => QoS::ExactlyOnce, // connect validates the QoS range before starting the worker.
                    };
                    Filter::new(subscription.topic.clone(), qos)
                });
                // One request avoids filling the bounded queue while poll is paused,
                // even when there are more filters than the request queue's capacity.
                let subscribe = Subscribe::new_many(filters, None);
                ensure!(
                    subscribe.size() <= MAX_PACKET_SIZE as usize,
                    "MQTT subscription packet must not exceed 16 MiB."
                );
                client
                    .subscribe_many(subscribe.filters)
                    .await
                    .context("Could not subscribe to topics.")?;
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
                if ack.return_codes.iter().any(|code| !matches!(code, SubscribeReasonCode::Success(_))) {
                    tracing::warn!(broker = %config.host, "Broker refused one or more topic subscriptions");
                    let detail = ack
                        .properties
                        .as_ref()
                        .and_then(|properties| properties.reason_string.as_deref())
                        .map(|reason| format!(" · {reason}"))
                        .unwrap_or_default();
                    BrokerEvent::Status(format!(
                        "Broker refused one or more topic subscriptions: {:?}{detail}. Check access permissions and broker capabilities.",
                        ack.return_codes
                    ))
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
                topic: std::str::from_utf8(&publish.topic)
                    .context("Broker sent a non-UTF-8 MQTT topic.")?
                    .to_owned(),
                payload: publish.payload,
                qos: publish.qos as u8,
                retained: publish.retain,
                received_at: Local::now(),
                properties: MessageProperties::from_publish(publish.properties),
            }),
            Ok(_) => continue,
            Err(error) => {
                tracing::error!(broker = %config.host, port = config.port, error = %error, "MQTT connection failed; retrying");
                pending_subscription = None;
                session = SessionState::Disconnected;
                capabilities = PublishCapabilities::from_connack(None);
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

    fn variable_integer(bytes: &[u8]) -> (usize, usize) {
        let mut value = 0;
        for (index, byte) in bytes.iter().copied().enumerate().take(4) {
            value |= usize::from(byte & 0x7f) << (7 * index);
            if byte & 0x80 == 0 {
                return (value, index + 1);
            }
        }
        panic!("invalid MQTT variable integer");
    }

    fn write_variable_integer(packet: &mut Vec<u8>, mut length: usize) {
        loop {
            let mut byte = (length % 128) as u8;
            length /= 128;
            if length != 0 {
                byte |= 0x80;
            }
            packet.push(byte);
            if length == 0 {
                break;
            }
        }
    }

    fn wire_packet(header: u8, body: &[u8]) -> Vec<u8> {
        let mut packet = vec![header];
        write_variable_integer(&mut packet, body.len());
        packet.extend_from_slice(body);
        packet
    }

    fn write_string(bytes: &mut Vec<u8>, value: &str) {
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }

    fn metadata_publish() -> Vec<u8> {
        let mut properties = vec![0x01, 1, 0x02, 0, 0, 0, 60, 0x03];
        write_string(&mut properties, "application/json");
        properties.push(0x08);
        write_string(&mut properties, "reply/é");
        properties.extend_from_slice(&[0x09, 0, 3, 0, 0xff, 42]);
        for value in ["first".repeat(30), "second".into()] {
            properties.push(0x26);
            write_string(&mut properties, "source");
            write_string(&mut properties, &value);
        }
        properties.extend_from_slice(&[0x0b, 7, 0x0b, 0xc1, 2]); // subscription IDs 7, 321
        assert!(properties.len() > 127, "exercise multi-byte property lengths");
        let mut body = Vec::new();
        write_string(&mut body, "test/é");
        body.extend_from_slice(&[0, 42]); // QoS 1 packet ID
        write_variable_integer(&mut body, properties.len());
        body.extend_from_slice(&properties);
        body.extend_from_slice(b"{\"value\":42}");
        wire_packet(0x33, &body) // retained QoS 1
    }

    fn assert_metadata_message(message: &Message) {
        assert_eq!(message.topic, "test/é");
        assert_eq!(message.payload.as_ref(), b"{\"value\":42}");
        assert_eq!(message.qos, 1);
        assert!(message.retained);
        let properties = &message.properties;
        assert_eq!(properties.content_type(), Some("application/json"));
        assert_eq!(properties.payload_format_indicator(), Some(1));
        assert_eq!(properties.message_expiry_interval(), Some(60));
        assert_eq!(properties.response_topic(), Some("reply/é"));
        assert_eq!(properties.correlation_data(), Some([0, 0xff, 42].as_slice()));
        assert_eq!(
            properties.user_properties(),
            &[("source".into(), "first".repeat(30)), ("source".into(), "second".into())]
        );
        assert_eq!(properties.subscription_identifiers(), &[7, 321]);
    }

    fn connect_client_id(body: &[u8]) -> &str {
        assert_eq!(&body[..7], b"\0\x04MQTT\x05");
        assert_eq!(body[7] & 0x02, 0x02, "CONNECT must request a clean start");
        assert_eq!(&body[8..10], &[0, 30]);
        let (properties_len, length_bytes) = variable_integer(&body[10..]);
        let offset = 10 + length_bytes + properties_len;
        assert_eq!(&body[10 + length_bytes..offset], &[0x27, 1, 0, 0, 0, 0x22, 0, 0]);
        let length = usize::from(u16::from_be_bytes([body[offset], body[offset + 1]]));
        std::str::from_utf8(&body[offset + 2..offset + 2 + length]).unwrap()
    }

    fn handshake(listener: &TcpListener, granted: u8) -> TcpStream {
        handshake_with_subscriptions(listener, &[TopicSubscription::default()], &[granted])
    }

    fn handshake_with_subscriptions(listener: &TcpListener, topics: &[TopicSubscription], granted: &[u8]) -> TcpStream {
        handshake_with_properties(listener, topics, granted, &[])
    }

    fn handshake_with_properties(listener: &TcpListener, topics: &[TopicSubscription], granted: &[u8], properties: &[u8]) -> TcpStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (header, body) = read_packet(&mut stream);
        assert_eq!(header, 0x10); // CONNECT
        connect_client_id(&body);
        let mut connack = vec![0, 0];
        write_variable_integer(&mut connack, properties.len());
        connack.extend_from_slice(properties);
        stream.write_all(&wire_packet(0x20, &connack)).unwrap();
        let mut pings = 0;
        let (header, body) = loop {
            let packet = read_packet(&mut stream);
            if packet.0 != 0xc0 {
                break packet;
            }
            pings += 1;
            assert!(pings <= 1, "server keepalive zero must not cause repeated immediate pings");
            stream.write_all(&[0xd0, 0]).unwrap();
        };
        assert_eq!(header, 0x82); // SUBSCRIBE, never PUBLISH
        let mut expected = Vec::new();
        for subscription in topics {
            expected.extend_from_slice(&(subscription.topic.len() as u16).to_be_bytes());
            expected.extend_from_slice(subscription.topic.as_bytes());
            expected.push(subscription.qos);
        }
        assert_eq!(body[2], 0, "SUBSCRIBE properties must be empty");
        assert_eq!(&body[3..], expected.as_slice(), "all filters and their QoS must be in one packet");
        let mut ack = vec![body[0], body[1], 0]; // SUBACK packet ID, empty properties
        ack.extend_from_slice(granted);
        stream.write_all(&wire_packet(0x90, &ack)).unwrap();
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
                    let event = connection
                        .events
                        .as_mut()
                        .expect("broker event receiver has been taken")
                        .recv()
                        .await
                        .expect("MQTT worker stopped");
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

    fn publish_command(qos: QoS, retain: bool, payload_len: usize) -> Command {
        Command::Publish(PublishCommandData {
            topic: "test/value".into(),
            payload: vec![b'x'; payload_len],
            qos,
            retain,
            content_type: None,
        })
    }

    fn assert_capability_rejections(properties: Vec<u8>, allowed_qos: Qos, rejected: Vec<(Command, &str)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (started, received) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake_with_properties(&listener, &[TopicSubscription::default()], &[0], &properties);
            let (header, body) = read_packet(&mut stream);
            let qos = QoS::from(allowed_qos);
            assert_eq!(header, 0x30 | ((qos as u8) << 1));
            assert_eq!(&body[2..12], b"test/value");
            assert_eq!(&body[body.len() - 3..], b"abc");
            started.send(()).unwrap();
            finished.recv_timeout(Duration::from_secs(5)).unwrap();
            if qos == QoS::AtLeastOnce {
                stream.write_all(&[0x40, 2, body[12], body[13]]).unwrap();
            }
            // No rejected publish or partial deletion batch may reach this socket.
            assert_eq!(read_packet(&mut stream), (0x30, b"\0\x0atest/value\x0012345".to_vec()));
            stream.write_all(b"\x30\x17\0\x10test/ack-barrier\0done").unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        connection
            .publish("test/value".into(), b"abc".to_vec(), allowed_qos, false)
            .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        for (command, expected) in rejected {
            let kind = command.kind();
            assert!(connection.commands.try_send(command).is_ok());
            match (kind, next_event(&runtime, &mut connection)) {
                (OperationKind::Publish, BrokerEvent::PublishError(error))
                | (OperationKind::DeleteTopics, BrokerEvent::OperationError(error)) => assert!(error.contains(expected), "{error}"),
                _ => panic!("unsupported command must report only its own operation error"),
            }
        }
        // The existing inflight publish and subsequent commands must remain usable.
        connection
            .publish("test/value".into(), b"12345".to_vec(), Qos::AtMostOnce, false)
            .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
        finish.send(()).unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Message(_)));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn connack_maximum_qos_rejects_unsupported_publishes_without_cancelling_other_commands() {
        for maximum in [0, 1] {
            let rejected = [QoS::AtLeastOnce, QoS::ExactlyOnce]
                .into_iter()
                .filter(|qos| *qos as u8 > maximum)
                .map(|qos| (publish_command(qos, false, 0), "maximum publish QoS"))
                .collect();
            let allowed = if maximum == 0 { Qos::AtMostOnce } else { Qos::AtLeastOnce };
            assert_capability_rejections(vec![0x24, maximum], allowed, rejected);
        }
    }

    #[test]
    fn connack_retain_unavailable_rejects_retained_publishes_and_deletions_without_disconnect() {
        assert_capability_rejections(
            vec![0x25, 0],
            Qos::AtLeastOnce,
            vec![
                (publish_command(QoS::AtMostOnce, true, 0), "retained publishes"),
                (publish_command(QoS::AtLeastOnce, true, 0), "retained publishes"),
                (
                    Command::DeleteTopics(DeleteTopicsData {
                        topics: vec!["test/value".into()],
                    }),
                    "retained publishes",
                ),
            ],
        );
    }

    #[test]
    fn connack_packet_limit_checks_full_publish_size_and_entire_deletion_batch_without_disconnect() {
        assert_capability_rejections(
            vec![0x27, 0, 0, 0, 20],
            Qos::AtLeastOnce,
            vec![
                (publish_command(QoS::AtMostOnce, false, 6), "maximum packet size of 20 bytes"),
                (publish_command(QoS::AtLeastOnce, false, 4), "maximum packet size of 20 bytes"),
                (
                    Command::DeleteTopics(DeleteTopicsData {
                        topics: vec!["ok".into(), "a".repeat(16)],
                    }),
                    "maximum packet size of 20 bytes",
                ),
            ],
        );
    }

    #[test]
    fn connack_packet_limit_includes_content_type_and_multi_byte_property_length_without_disconnect() {
        for (limit, content_type) in [(20u32, "image/png".into()), (144, "x".repeat(125))] {
            let mut properties = vec![0x27];
            properties.extend_from_slice(&limit.to_be_bytes());
            assert_capability_rejections(
                properties,
                Qos::AtLeastOnce,
                vec![(
                    Command::Publish(PublishCommandData {
                        topic: "test/value".into(),
                        payload: Vec::new(),
                        qos: QoS::AtMostOnce,
                        retain: false,
                        content_type: Some(content_type),
                    }),
                    "maximum packet size",
                )],
            );
        }
    }

    #[test]
    fn connack_capabilities_use_protocol_defaults_and_cap_broker_packet_limit_locally() {
        let defaults = PublishCapabilities::from_connack(None);
        assert_eq!(defaults.max_qos, 2);
        assert!(defaults.retain_available);
        assert_eq!(defaults.max_packet_size, MAX_PACKET_SIZE);
        let mut wire = vec![0x20, 8, 0, 0, 5, 0x27];
        wire.extend_from_slice(&(MAX_PACKET_SIZE + 1).to_be_bytes());
        let Packet::ConnAck(ack) = Packet::read(&mut bytes::BytesMut::from(wire.as_slice()), None).unwrap() else {
            panic!("expected CONNACK");
        };
        let capabilities = PublishCapabilities::from_connack(ack.properties.as_ref());
        assert_eq!(capabilities.max_packet_size, MAX_PACKET_SIZE);
        assert!(
            capabilities
                .validate_publish("test/value", MAX_PACKET_SIZE as usize, QoS::AtMostOnce, false, None)
                .is_err()
        );
    }

    #[test]
    fn reconnect_replaces_negotiated_publish_capabilities_with_new_connack_defaults() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (disconnect, disconnected) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let stream = handshake_with_properties(
                &listener,
                &[TopicSubscription::default()],
                &[0],
                &[0x24, 0, 0x25, 0, 0x27, 0, 0, 0, 20],
            );
            disconnected.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(stream);
            let mut stream = handshake(&listener, 0);
            let (header, body) = read_packet(&mut stream);
            assert_eq!(header, 0x33); // QoS 1, retained, exceeds the previous 20-byte limit.
            assert_eq!(&body[2..12], b"test/value");
            assert_eq!(&body[15..], &[b'x'; 20]);
            finished.recv_timeout(Duration::from_secs(5)).unwrap();
            stream.write_all(&[0x40, 2, body[12], body[13]]).unwrap();
            stream.write_all(b"\x30\x17\0\x10test/ack-barrier\0done").unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        disconnect.send(()).unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        connection
            .publish("test/value".into(), vec![b'x'; 20], Qos::AtLeastOnce, true)
            .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
        finish.send(()).unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Message(_)));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn server_keepalive_zero_keeps_optional_pings_bounded_and_session_usable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready, received) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake_with_properties(&listener, &[TopicSubscription::default()], &[0], &[0x13, 0, 0]);
            stream.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            let mut pings = 0;
            loop {
                let mut header = [0];
                match stream.read_exact(&mut header) {
                    Ok(()) => {
                        assert_eq!(header[0], 0xc0);
                        stream.read_exact(&mut header).unwrap();
                        assert_eq!(header[0], 0);
                        pings += 1;
                        assert!(pings <= 1, "zero keepalive must not cause a rapid ping loop");
                        stream.write_all(&[0xd0, 0]).unwrap();
                    }
                    Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
                    Err(error) => panic!("zero keepalive unexpectedly disconnected: {error}"),
                }
            }
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            ready.send(()).unwrap();
            assert_eq!(read_packet(&mut stream), (0x30, b"\0\x0atest/value\0data".to_vec()));
            finished.recv_timeout(Duration::from_secs(5)).unwrap();
            stream.write_all(b"\x30\x17\0\x10test/ack-barrier\0done").unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        connection
            .publish("test/value".into(), b"data".to_vec(), Qos::AtMostOnce, false)
            .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
        finish.send(()).unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Message(_)));
        drop(connection);
        broker.join().unwrap();
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
    fn mqtt_options_use_tls_when_certificate_validation_is_disabled() {
        let config = ConnectionConfig {
            tls: true,
            validate_certificate: false,
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert!(matches!(options.transport(), Transport::Tls(_)));
    }

    #[test]
    fn mqtt_options_use_system_trusted_tls_for_secure_websocket() {
        let config = ConnectionConfig {
            port: 9002,
            tls: true,
            websocket: true,
            validate_certificate: true,
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert!(matches!(options.transport(), Transport::Wss(rumqttc::TlsConfiguration::Rustls(_))));
    }

    #[test]
    fn mqtt_options_use_secure_websocket_when_certificate_validation_is_disabled() {
        let config = ConnectionConfig {
            port: 9002,
            tls: true,
            websocket: true,
            validate_certificate: false,
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert!(matches!(options.transport(), Transport::Wss(rumqttc::TlsConfiguration::Rustls(_))));
    }

    #[test]
    fn mqtt_options_ignore_certificate_validation_for_tcp_and_unencrypted_websocket() {
        for validate_certificate in [true, false] {
            let config = ConnectionConfig {
                validate_certificate,
                ..Default::default()
            };
            assert!(matches!(mqtt_options(&config).unwrap().transport(), Transport::Tcp));
            let config = ConnectionConfig {
                websocket: true,
                tls: false,
                ..config
            };
            assert!(matches!(mqtt_options(&config).unwrap().transport(), Transport::Ws));
        }
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
    fn mqtt_options_use_secure_websocket_urls_for_hostnames_ipv4_and_ipv6() {
        for (host, expected) in [
            ("broker.example", "wss://broker.example:9002/mqtt"),
            ("127.0.0.1", "wss://127.0.0.1:9002/mqtt"),
            ("::1", "wss://[::1]:9002/mqtt"),
            ("[::1]", "wss://[::1]:9002/mqtt"),
        ] {
            let config = ConnectionConfig {
                host: host.into(),
                port: 9002,
                tls: true,
                websocket: true,
                validate_certificate: false,
                ..Default::default()
            };
            let options = mqtt_options(&config).unwrap();
            assert_eq!(options.broker_address(), (expected.into(), 9002));
            assert!(matches!(options.transport(), Transport::Wss(rumqttc::TlsConfiguration::Rustls(_))));
        }
    }

    #[test]
    fn mqtt_options_keep_custom_secure_websocket_ports() {
        let config = ConnectionConfig {
            host: "::1".into(),
            port: 9443,
            tls: true,
            websocket: true,
            validate_certificate: false,
            ..Default::default()
        };
        assert_eq!(
            mqtt_options(&config).unwrap().broker_address(),
            ("wss://[::1]:9443/mqtt".into(), 9443)
        );
    }

    #[test]
    fn mqtt_options_use_secure_websocket_when_tls_is_enabled_and_keep_mqtt_settings() {
        let config = ConnectionConfig {
            websocket: true,
            tls: true,
            client_id: "wss-client".into(),
            username: "mqtt-user".into(),
            password: "mqtt-secret".into(),
            ..Default::default()
        };
        let options = mqtt_options(&config).unwrap();
        assert!(matches!(options.transport(), Transport::Wss(rumqttc::TlsConfiguration::Rustls(_))));
        assert_eq!(options.client_id(), config.client_id);
        assert_eq!(
            options.credentials(),
            Some(rumqttc::v5::mqttbytes::v5::Login::new(config.username, config.password))
        );
        assert_eq!(options.keep_alive(), Duration::from_secs(30));
        assert!(options.clean_start());
        assert_eq!(options.max_packet_size(), Some(MAX_PACKET_SIZE));
        assert_eq!(options.topic_alias_max(), Some(0));
        assert_eq!(options.session_expiry_interval(), None); // MQTT 5 defaults to zero.
        assert_eq!(options.connection_timeout(), 10);
        assert_eq!(options.get_outgoing_inflight_upper_limit(), Some(100));
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
            socket.send(WsMessage::Binary(vec![0x20, 3, 0, 0, 0].into())).unwrap();
            let subscribe = socket.read().unwrap().into_data();
            assert_eq!(subscribe[0], 0x82);
            assert_eq!(&subscribe[4..], b"\0\0\x01#\0");
            socket
                .send(WsMessage::Binary(vec![0x90, 4, subscribe[2], subscribe[3], 0, 0].into()))
                .unwrap();
            socket.send(WsMessage::Binary(metadata_publish().into())).unwrap();
            assert_eq!(socket.read().unwrap().into_data().as_ref(), &[0x40, 2, 0, 42]);
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
            BrokerEvent::Message(message) => assert_metadata_message(&message),
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
    fn tcp_transport_receives_mqtt5_publish_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 1);
            stream.write_all(&metadata_publish()).unwrap();
            assert_eq!(read_packet(&mut stream), (0x40, vec![0, 42]));
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        match next_event(&runtime, &mut connection) {
            BrokerEvent::Message(message) => assert_metadata_message(&message),
            _ => panic!("expected a publish with MQTT 5 properties"),
        }
        drop(connection);
        broker.join().unwrap();
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
            stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
            let (header, subscribe) = read_packet(&mut stream);
            assert_eq!(header, 0x82);
            stream.write_all(&[0x90, 4, subscribe[0], subscribe[1], 0, 0]).unwrap();
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
                stream.write_all(b"\x31\x0f\x00\x0atest/value\x0042").unwrap();
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
                    assert_eq!(message.properties, MessageProperties::default());
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
        for rejected in [0x80, 0x83, 0x87, 0x8f, 0x91, 0x97, 0x9e, 0xa1, 0xa2] {
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
                let mut stream = handshake_with_subscriptions(&listener, &expected, &[0, rejected, 2]);
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
                events: Some(events),
                stop: Some(stop),
                commands,
            },
            receiver,
            stopped,
        )
    }

    #[test]
    fn take_events_transfers_the_receiver_only_once() {
        let (mut connection, _, _) = command_connection();
        let _events = connection.take_events().expect("broker event receiver must be available");
        assert!(connection.take_events().is_none());
    }

    #[test]
    fn taking_events_preserves_commands_and_drop_cancellation() {
        let (mut connection, mut commands, mut stopped) = command_connection();
        let _events = connection.take_events().expect("broker event receiver must be available");
        assert_eq!(stopped.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        connection.publish("test/value".into(), Vec::new(), Qos::AtMostOnce, false).unwrap();
        assert!(matches!(commands.try_recv(), Ok(Command::Publish(_))));
        drop(connection);
        assert_eq!(stopped.try_recv(), Ok(()));
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)));
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
    fn publish_packet_limit_includes_mqtt5_headers_properties_and_packet_id() {
        for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
            // At this size the remaining-length field occupies four bytes.
            let overhead = 1 + 4 + 2 + "test/é".len() + usize::from(qos != QoS::AtMostOnce) * 2 + 1;
            let maximum_payload = MAX_PACKET_SIZE as usize - overhead;
            assert!(ensure_publish_packet_size("test/é", maximum_payload, qos, None, MAX_PACKET_SIZE).is_ok());
            assert!(ensure_publish_packet_size("test/é", maximum_payload + 1, qos, None, MAX_PACKET_SIZE).is_err());
        }
    }

    #[test]
    fn publish_packet_limit_matches_encoded_content_type_properties_and_variable_length_fields() {
        use rumqttc::v5::mqttbytes::v5::Publish;

        // Property lengths on both sides of the two- and three-byte boundaries.
        let content_types = std::iter::once(None).chain([0, 9, 124, 125, 16380, 16381, 65535].map(|len| Some("x".repeat(len))));
        for content_type in content_types {
            for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
                // With a nine-byte content type these cross remaining lengths 127 and 16383.
                for payload_len in [0, 103, 104, 105, 106, 16359, 16360, 16361, 16362] {
                    let properties = content_type.as_ref().map(|value| PublishProperties {
                        content_type: Some(value.clone()),
                        ..Default::default()
                    });
                    let mut publish = Publish::new("test/é", qos, vec![0xff; payload_len], properties);
                    publish.pkid = 1; // rumqttc assigns this before encoding QoS 1/2 packets.
                    let mut wire = bytes::BytesMut::new();
                    publish.write(&mut wire).unwrap();
                    let size = wire.len() as u32;
                    assert!(ensure_publish_packet_size("test/é", payload_len, qos, content_type.as_deref(), size).is_ok());
                    assert!(ensure_publish_packet_size("test/é", payload_len, qos, content_type.as_deref(), size - 1).is_err());
                }
            }
        }
    }

    #[test]
    fn publish_with_content_type_enforces_the_local_packet_limit_before_queueing() {
        let (connection, mut commands, _) = command_connection();
        // Four-byte remaining length, topic, properties length, content-type ID/string.
        let overhead = 1 + 4 + 2 + "test/é".len() + 1 + 1 + 2 + "image/png".len();
        let payload = vec![0xff; MAX_PACKET_SIZE as usize - overhead + 1];
        let error = connection
            .publish_with_content_type("test/é".into(), payload.clone(), Qos::AtMostOnce, false, Some("image/png".into()))
            .unwrap_err();
        assert!(error.to_string().contains("maximum packet size"));
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        connection.publish("test/é".into(), payload, Qos::AtMostOnce, false).unwrap();
        commands.try_recv().unwrap();
        connection
            .publish_with_content_type(
                "test/é".into(),
                vec![0xff; MAX_PACKET_SIZE as usize - overhead],
                Qos::AtMostOnce,
                false,
                Some("image/png".into()),
            )
            .unwrap();
        assert!(matches!(commands.try_recv(), Ok(Command::Publish(_))));
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
                    assert_eq!(command.content_type, None);
                }
            }
        }
    }

    #[test]
    fn publish_with_content_type_preserves_raw_bytes_topic_options_and_optional_property() {
        let (connection, mut commands, _) = command_connection();
        for content_type in [None, Some(String::new()), Some(" image/png; note=é ".into())] {
            for qos in [Qos::AtMostOnce, Qos::AtLeastOnce, Qos::ExactlyOnce] {
                for retain in [false, true] {
                    let payload = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0, 0xff];
                    connection
                        .publish_with_content_type(" raw//é ".into(), payload.clone(), qos, retain, content_type.clone())
                        .unwrap();
                    let Command::Publish(command) = commands.try_recv().unwrap() else {
                        panic!("expected a publish command");
                    };
                    assert_eq!(command.topic, " raw//é ");
                    assert_eq!(command.payload, payload);
                    assert_eq!(command.qos, qos.into());
                    assert_eq!(command.retain, retain);
                    assert_eq!(command.content_type, content_type);
                }
            }
        }
    }

    #[test]
    fn publish_with_content_type_rejects_null_and_oversized_utf8_strings_before_queueing() {
        let (connection, mut commands, _) = command_connection();
        for (content_type, expected) in [
            ("image/\0png".into(), "null character"),
            ("x".repeat(65536), "65535 UTF-8 bytes"),
            ("é".repeat(32768), "65535 UTF-8 bytes"),
        ] {
            let error = connection
                .publish_with_content_type("test/value".into(), Vec::new(), Qos::AtMostOnce, false, Some(content_type))
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
        assert!(matches!(commands.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    }

    #[test]
    fn publish_with_content_type_accepts_empty_and_maximum_utf8_byte_length_strings() {
        let (connection, mut commands, _) = command_connection();
        for content_type in [String::new(), "x".repeat(65535), format!("{}a", "é".repeat(32767))] {
            connection
                .publish_with_content_type("test/value".into(), Vec::new(), Qos::AtMostOnce, false, Some(content_type.clone()))
                .unwrap();
            let Command::Publish(command) = commands.try_recv().unwrap() else {
                panic!("expected a publish command");
            };
            assert_eq!(command.content_type, Some(content_type));
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
        let (disconnect, disconnected) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let stream = handshake(&listener, 0);
            disconnected.recv().unwrap();
            drop(stream);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        next_matching_event(&runtime, &mut connection, |event| matches!(event, BrokerEvent::Connected));
        disconnect.send(()).unwrap();
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
                    let properties_offset = offset + if qos == 0 { 0 } else { 2 };
                    assert_eq!(body[properties_offset], 0);
                    assert_eq!(&body[properties_offset + 1..], &[0, 0xff, b'\n']);
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
            stream.write_all(b"\x30\x17\x00\x10test/ack-barrier\0done").unwrap();
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
        assert!(matches!(
            connection.events.as_mut().unwrap().try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn publish_with_content_type_sends_raw_image_bytes_and_only_the_requested_property() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (acknowledge, acknowledged) = std::sync::mpsc::channel();
        let (finished, received) = std::sync::mpsc::channel();
        let content_types = [None, Some(String::new()), Some("image/png".into()), Some("é".repeat(63))];
        let expected_content_types = content_types.clone();
        let payload = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0, 0xff];
        let expected_payload = payload.clone();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            for content_type in expected_content_types {
                for qos in 0..=2 {
                    for retain in [false, true] {
                        let (header, body) = read_packet(&mut stream);
                        assert_eq!(header, 0x30 | (qos << 1) | u8::from(retain));
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        assert_eq!(&body[2..2 + topic_len], "test/é".as_bytes());
                        let offset = 2 + topic_len;
                        let properties_offset = offset + if qos == 0 { 0 } else { 2 };
                        let (properties_len, length_bytes) = variable_integer(&body[properties_offset..]);
                        let mut expected = Vec::new();
                        if let Some(content_type) = content_type.as_deref() {
                            expected.push(0x03);
                            write_string(&mut expected, content_type);
                        }
                        assert_eq!(properties_len, expected.len());
                        if properties_len >= 128 {
                            assert_eq!(length_bytes, 2, "exercise multi-byte property lengths on the wire");
                        }
                        let start = properties_offset + length_bytes;
                        assert_eq!(&body[start..start + properties_len], expected.as_slice());
                        assert_eq!(&body[start + properties_len..], expected_payload.as_slice());
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
            }
            stream.write_all(b"\x30\x17\0\x10test/ack-barrier\0done").unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        for content_type in content_types {
            for qos in [Qos::AtMostOnce, Qos::AtLeastOnce, Qos::ExactlyOnce] {
                for retain in [false, true] {
                    connection
                        .publish_with_content_type("test/é".into(), payload.clone(), qos, retain, content_type.clone())
                        .unwrap();
                    assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
                    acknowledge.send(()).unwrap();
                    received.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            }
        }
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Message(_)));
        assert!(matches!(
            connection.events.as_mut().unwrap().try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn negative_puback_reports_reason_without_disconnect_and_no_matching_subscribers_is_success() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (acknowledge, acknowledged) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            for reason in [0x87, 0x10] {
                let (header, body) = read_packet(&mut stream);
                assert_eq!(header, 0x32);
                let pkid = &body[12..14];
                acknowledged.recv_timeout(Duration::from_secs(5)).unwrap();
                let mut ack = vec![pkid[0], pkid[1], reason];
                let mut properties = vec![0x1f];
                write_string(&mut properties, "publish permission denied");
                write_variable_integer(&mut ack, properties.len());
                ack.extend_from_slice(&properties);
                stream.write_all(&wire_packet(0x40, &ack)).unwrap();
            }
            stream.write_all(b"\x30\x17\0\x10test/ack-barrier\0done").unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        for pass in 0..2 {
            connection
                .publish("test/value".into(), b"data".to_vec(), Qos::AtLeastOnce, false)
                .unwrap();
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
            acknowledge.send(()).unwrap();
            if pass == 0 {
                match next_event(&runtime, &mut connection) {
                    BrokerEvent::PublishError(error) => {
                        assert!(error.contains("PUBACK: NotAuthorized"));
                        assert!(error.contains("publish permission denied"));
                    }
                    _ => panic!("a rejected QoS 1 publish must report a publish error"),
                }
            }
        }
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Message(_)));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn negative_qos_two_acknowledgements_report_errors_and_reset_without_replay() {
        for acknowledgement in [0x50, 0x70] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (acknowledge, acknowledged) = std::sync::mpsc::channel();
            let broker = std::thread::spawn(move || {
                let mut stream = handshake(&listener, 0);
                let (header, body) = read_packet(&mut stream);
                assert_eq!(header, 0x34);
                let pkid = &body[12..14];
                acknowledged.recv_timeout(Duration::from_secs(5)).unwrap();
                if acknowledgement == 0x70 {
                    stream.write_all(&[0x50, 2, pkid[0], pkid[1]]).unwrap();
                    assert_eq!(read_packet(&mut stream), (0x62, pkid.to_vec()));
                }
                let reason = if acknowledgement == 0x50 { 0x87 } else { 0x92 };
                stream.write_all(&[acknowledgement, 4, pkid[0], pkid[1], reason, 0]).unwrap();
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
                let mut stream = handshake(&listener, 0);
                assert_eq!(stream.read(&mut [0]).unwrap(), 0, "rejected publishes must not replay");
            });
            let runtime = runtime();
            let mut connection = connect(ConnectionConfig {
                host: "127.0.0.1".into(),
                port,
                ..Default::default()
            })
            .unwrap();
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
            connection
                .publish("test/value".into(), b"data".to_vec(), Qos::ExactlyOnce, false)
                .unwrap();
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
            acknowledge.send(()).unwrap();
            match next_event(&runtime, &mut connection) {
                BrokerEvent::PublishError(error) => assert!(error.contains(if acknowledgement == 0x50 {
                    "PUBREC: NotAuthorized"
                } else {
                    "PUBCOMP: PacketIdentifierNotFound"
                })),
                _ => panic!("a rejected QoS 2 publish must report a publish error"),
            }
            match next_event(&runtime, &mut connection) {
                BrokerEvent::Status(status) => assert!(status.contains("resetting the session")),
                _ => panic!("negative QoS 2 acknowledgement must reset the stalled rumqttc session"),
            }
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connecting));
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
            drop(connection);
            broker.join().unwrap();
        }
    }

    #[test]
    fn broker_disconnect_reports_reason_cancels_inflight_publish_and_reconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (disconnect, disconnected) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            assert_eq!(read_packet(&mut stream).0, 0x32);
            disconnected.recv_timeout(Duration::from_secs(5)).unwrap();
            let mut properties = vec![0x1f];
            write_string(&mut properties, "maintenance");
            let mut body = vec![0x8b];
            write_variable_integer(&mut body, properties.len());
            body.extend_from_slice(&properties);
            stream.write_all(&wire_packet(0xe0, &body)).unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            let mut stream = handshake(&listener, 0);
            assert_eq!(stream.read(&mut [0]).unwrap(), 0, "inflight publish must not replay");
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        connection
            .publish("test/value".into(), b"data".to_vec(), Qos::AtLeastOnce, false)
            .unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishQueued));
        disconnect.send(()).unwrap();
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::PublishError(_)));
        match next_event(&runtime, &mut connection) {
            BrokerEvent::Status(status) => {
                assert!(status.contains("ServerShuttingDown"));
                assert!(status.contains("maintenance"));
            }
            _ => panic!("broker DISCONNECT must report its reason"),
        }
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connecting));
        assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
        drop(connection);
        broker.join().unwrap();
    }

    #[test]
    fn invalid_utf8_topics_and_unnegotiated_aliases_reconnect_without_delivering_messages() {
        for (body, expected) in [
            (vec![0, 1, 0xff, 0, b'x'], "non-UTF-8"),
            (vec![0, 1, b'x', 3, 0x23, 0, 1], "topic alias"),
            (vec![0, 0, 3, 0x23, 0, 1], "topic alias"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (publish, published) = std::sync::mpsc::channel();
            let broker = std::thread::spawn(move || {
                let mut stream = handshake(&listener, 0);
                published.recv_timeout(Duration::from_secs(5)).unwrap();
                stream.write_all(&wire_packet(0x30, &body)).unwrap();
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
                let mut stream = handshake(&listener, 0);
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            });
            let runtime = runtime();
            let mut connection = connect(ConnectionConfig {
                host: "127.0.0.1".into(),
                port,
                ..Default::default()
            })
            .unwrap();
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
            publish.send(()).unwrap();
            match next_event(&runtime, &mut connection) {
                BrokerEvent::Status(status) => assert!(status.contains(expected), "{status}"),
                _ => panic!("invalid topic must not be delivered as a message"),
            }
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connecting));
            assert!(matches!(next_event(&runtime, &mut connection), BrokerEvent::Connected));
            drop(connection);
            broker.join().unwrap();
        }
    }

    #[test]
    fn publish_worker_failure_reports_publish_error_without_changing_the_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (finished, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0);
            assert_eq!(read_packet(&mut stream), (0x30, b"\x00\x0atest/value\0\x00\xff".to_vec()));
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
                    content_type: None,
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
                assert_eq!(body.len(), 15);
                assert_eq!(body[14], 0);
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
                assert_eq!(body[2 + length], 0);
                assert_eq!(body.len(), 3 + length, "deletion payload must be empty");
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
            matches!(
                connection.events.as_mut().unwrap().try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
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
            assert_eq!(read_packet(&mut stream), (0x31, b"\x00\x0atest/value\0".to_vec()));
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

    // Self-signed P-256 certificate for broker.example and its signature over TLS_TEST_MESSAGE.
    // Only public material is embedded; tests do not need OpenSSL, a private key, or the clock.
    const TLS_TEST_CERT: &[u8] = b"\x30\x82\x01\x8b\x30\x82\x01\x32\xa0\x03\x02\x01\x02\x02\x01\x01\x30\x0a\x06\x08\x2a\x86\x48\xce\
        \x3d\x04\x03\x02\x30\x19\x31\x17\x30\x15\x06\x03\x55\x04\x03\x0c\x0e\x62\x72\x6f\x6b\x65\x72\x2e\
        \x65\x78\x61\x6d\x70\x6c\x65\x30\x1e\x17\x0d\x32\x36\x31\x30\x30\x36\x30\x38\x31\x36\x33\x33\x5a\
        \x17\x0d\x33\x36\x31\x30\x30\x33\x30\x38\x31\x36\x33\x33\x5a\x30\x19\x31\x17\x30\x15\x06\x03\x55\
        \x04\x03\x0c\x0e\x62\x72\x6f\x6b\x65\x72\x2e\x65\x78\x61\x6d\x70\x6c\x65\x30\x59\x30\x13\x06\x07\
        \x2a\x86\x48\xce\x3d\x02\x01\x06\x08\x2a\x86\x48\xce\x3d\x03\x01\x07\x03\x42\x00\x04\x72\x17\xf9\
        \xa1\x75\x39\x9b\xb4\xb0\x4c\x1b\xcd\x23\x9d\xd3\x56\xde\xac\x61\xfb\x36\x3b\x73\xab\x43\x4e\xda\
        \x5e\xf7\x81\x5b\x01\x0f\x8c\xd3\x98\xaa\xcf\xfc\x12\xe5\x4d\xf6\x68\xc7\x8e\xcd\xad\xcc\x79\x83\
        \x53\x57\xce\x9b\x70\x95\x12\xb0\x08\x9a\xa6\xd3\xb6\xa3\x6b\x30\x69\x30\x1d\x06\x03\x55\x1d\x0e\
        \x04\x16\x04\x14\x9b\x0d\x65\x35\xcb\xd6\x5b\x88\x38\x9d\xc7\x09\x72\x19\xc3\x41\x5d\xd3\x98\x01\
        \x30\x1f\x06\x03\x55\x1d\x23\x04\x18\x30\x16\x80\x14\x9b\x0d\x65\x35\xcb\xd6\x5b\x88\x38\x9d\xc7\
        \x09\x72\x19\xc3\x41\x5d\xd3\x98\x01\x30\x19\x06\x03\x55\x1d\x11\x04\x12\x30\x10\x82\x0e\x62\x72\
        \x6f\x6b\x65\x72\x2e\x65\x78\x61\x6d\x70\x6c\x65\x30\x0c\x06\x03\x55\x1d\x13\x01\x01\xff\x04\x02\
        \x30\x00\x30\x0a\x06\x08\x2a\x86\x48\xce\x3d\x04\x03\x02\x03\x47\x00\x30\x44\x02\x20\x6d\xf7\xa1\
        \x24\x43\x74\x6b\x8f\xbd\xf5\x73\xcb\x71\x8b\xb2\x1b\xf0\xa7\x79\xd8\x85\x1a\x8c\xad\x19\xe2\x27\
        \x45\xda\x9f\xc4\x2e\x02\x20\x23\xbd\x17\xa1\xaa\xe6\x7b\x32\x95\x8c\xef\x06\x68\x4b\x8f\xde\x8e\
        \x4f\xa6\xe7\x55\x2d\xc6\x4d\xb2\x0d\xeb\x84\xeb\xb2\x5e\x2a";
    const TLS_TEST_MESSAGE: &[u8] = b"TLS handshake test message";
    const TLS_TEST_SIGNATURE: &[u8] = b"\x30\x44\x02\x20\x6d\x11\x26\xde\x32\x92\x92\xc7\x4e\x85\xcd\x47\x87\xc3\x19\x57\x35\xd3\x05\x82\
        \x6d\xb7\xc8\x61\xc5\x65\x0a\x8d\x06\x82\xda\xad\x02\x20\x26\x2a\x66\x7f\x84\x37\x1a\x59\xed\x80\
        \xae\xba\x89\x3f\xe6\xf8\x4c\xf7\x36\x17\xd5\x24\x45\xde\x6d\x5a\x88\x6a\x8f\xd2\x58\xa6";

    fn unchecked_verifier() -> UncheckedServerCertVerifier {
        UncheckedServerCertVerifier {
            provider: Arc::new(aws_lc_rs::default_provider()),
        }
    }

    fn tls_test_signature(scheme: SignatureScheme) -> DigitallySignedStruct {
        use rustls::internal::msgs::codec::Codec;

        // DigitallySignedStruct has no public constructor; decode its TLS wire representation.
        let mut encoded = u16::from(scheme).to_be_bytes().to_vec();
        encoded.extend_from_slice(&u16::try_from(TLS_TEST_SIGNATURE.len()).unwrap().to_be_bytes());
        encoded.extend_from_slice(TLS_TEST_SIGNATURE);
        DigitallySignedStruct::read_bytes(&encoded).unwrap()
    }

    #[test]
    fn unchecked_verifier_accepts_a_self_signed_certificate_with_the_wrong_hostname() {
        let cert = CertificateDer::from(TLS_TEST_CERT);
        let name = ServerName::try_from("other.example").unwrap();
        let result = unchecked_verifier().verify_server_cert(&cert, &[], &name, &[], UnixTime::since_unix_epoch(Duration::ZERO));
        assert!(result.is_ok(), "Unchecked server identity verification failed: {result:?}");
    }

    #[test]
    fn unchecked_verifier_accepts_valid_tls12_and_tls13_handshake_signatures() {
        let verifier = unchecked_verifier();
        let cert = CertificateDer::from(TLS_TEST_CERT);
        let signature = tls_test_signature(SignatureScheme::ECDSA_NISTP256_SHA256);
        let tls12 = verifier.verify_tls12_signature(TLS_TEST_MESSAGE, &cert, &signature);
        let tls13 = verifier.verify_tls13_signature(TLS_TEST_MESSAGE, &cert, &signature);
        assert!(tls12.is_ok(), "Valid TLS 1.2 signature rejected: {tls12:?}");
        assert!(tls13.is_ok(), "Valid TLS 1.3 signature rejected: {tls13:?}");
    }

    #[test]
    fn unchecked_verifier_rejects_forged_tls12_and_tls13_handshake_signatures() {
        let verifier = unchecked_verifier();
        let cert = CertificateDer::from(TLS_TEST_CERT);
        let signature = tls_test_signature(SignatureScheme::ECDSA_NISTP256_SHA256);
        assert!(verifier.verify_tls12_signature(b"tampered handshake", &cert, &signature).is_err());
        assert!(verifier.verify_tls13_signature(b"tampered handshake", &cert, &signature).is_err());
    }

    #[test]
    fn unchecked_verifier_rejects_malformed_certificates_during_signature_verification() {
        let verifier = unchecked_verifier();
        let cert = CertificateDer::from(&b"not a certificate"[..]);
        let signature = tls_test_signature(SignatureScheme::ECDSA_NISTP256_SHA256);
        assert!(verifier.verify_tls12_signature(TLS_TEST_MESSAGE, &cert, &signature).is_err());
        assert!(verifier.verify_tls13_signature(TLS_TEST_MESSAGE, &cert, &signature).is_err());
    }

    #[test]
    fn unchecked_verifier_rejects_tls12_only_signature_schemes_for_tls13() {
        let cert = CertificateDer::from(TLS_TEST_CERT);
        let signature = tls_test_signature(SignatureScheme::RSA_PKCS1_SHA256);
        assert!(matches!(
            unchecked_verifier().verify_tls13_signature(TLS_TEST_MESSAGE, &cert, &signature),
            Err(rustls::Error::PeerMisbehaved(_))
        ));
    }

    #[test]
    fn unchecked_verifier_advertises_only_the_crypto_providers_signature_schemes() {
        let verifier = unchecked_verifier();
        assert_eq!(
            verifier.supported_verify_schemes(),
            verifier.provider.signature_verification_algorithms.supported_schemes()
        );
    }

    #[test]
    fn tls_transport_allows_empty_roots_when_certificate_validation_is_disabled() {
        assert!(matches!(
            tls_transport(RootCertStore::empty(), false, false).unwrap(),
            Transport::Tls(_)
        ));
    }

    #[test]
    fn wss_transport_allows_empty_roots_when_certificate_validation_is_disabled() {
        assert!(matches!(
            tls_transport(RootCertStore::empty(), false, true).unwrap(),
            Transport::Wss(rumqttc::TlsConfiguration::Rustls(_))
        ));
    }

    #[test]
    fn wss_transport_returns_an_error_when_no_trusted_certificates_are_available() {
        assert_eq!(
            tls_transport(RootCertStore::empty(), true, true)
                .err()
                .expect("WSS without trusted roots must fail")
                .to_string(),
            "Could not load trusted system certificates for TLS."
        );
    }

    #[test]
    fn tls_transport_returns_an_error_when_no_trusted_certificates_are_available() {
        assert_eq!(
            tls_transport(RootCertStore::empty(), true, false)
                .err()
                .expect("TLS without trusted roots must fail")
                .to_string(),
            "Could not load trusted system certificates for TLS."
        );
    }
}
