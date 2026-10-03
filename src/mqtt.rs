//! MQTT I/O on a dedicated runtime, with bounded delivery and drop-based cancellation.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use chrono::Local;
use rumqttc::tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::aws_lc_rs};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, SubscribeReasonCode, Transport};
use tokio::sync::{mpsc, oneshot};

use crate::{config::ConnectionConfig, topics::Message};

pub enum BrokerEvent {
    Connecting,
    Connected,
    Status(String),
    Message(Message),
}

/// Receives broker events and cancels the MQTT worker when dropped.
pub struct Connection {
    pub events: mpsc::Receiver<BrokerEvent>,
    stop: Option<oneshot::Sender<()>>,
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
    std::thread::Builder::new()
        .name("mqtt".into())
        .spawn(move || {
            runtime.block_on(async {
                let result = tokio::select! {
                    _ = stopped => return,
                    result = run(config, &sender) => result,
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
    })
}

fn tls_transport(roots: RootCertStore) -> Result<Transport> {
    ensure!(
        !roots.is_empty(),
        "Could not load trusted system certificates for TLS."
    );
    let config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .context("Could not configure the TLS protocol versions.")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Transport::tls_with_config(config.into()))
}

async fn run(config: ConnectionConfig, sender: &mpsc::Sender<BrokerEvent>) -> Result<()> {
    let id = format!(
        "mqtt-ui-{}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut options = MqttOptions::new(id, config.host, config.port);
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
    loop {
        let event = match event_loop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                client
                    .subscribe("#", QoS::ExactlyOnce)
                    .await
                    .context("Could not subscribe to topics.")?;
                continue;
            }
            Ok(Event::Incoming(Packet::SubAck(ack))) => {
                if ack
                    .return_codes
                    .iter()
                    .any(|code| matches!(code, SubscribeReasonCode::Failure))
                {
                    BrokerEvent::Status(
                        "Broker refused the topic subscription. Check access permissions.".into(),
                    )
                } else {
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
                if sender
                    .send(BrokerEvent::Status(format!(
                        "{error} · retrying in 2 seconds"
                    )))
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

    fn handshake(listener: &TcpListener, granted: u8) -> TcpStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(read_packet(&mut stream).0, 0x10); // CONNECT
        stream.write_all(&[0x20, 2, 0, 0]).unwrap(); // CONNACK
        let (header, body) = read_packet(&mut stream);
        assert_eq!(header, 0x82); // SUBSCRIBE, never PUBLISH
        assert_eq!(&body[2..], &[0, 1, b'#', 2]);
        stream
            .write_all(&[0x90, 3, body[0], body[1], granted])
            .unwrap();
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
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn receives_retained_messages_resubscribes_and_stops_on_drop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (delivered, received) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            for pass in 0..2 {
                let mut stream = handshake(&listener, 2);
                // Retained QoS 0 PUBLISH: topic `test/value`, payload `42`.
                stream.write_all(b"\x31\x0e\x00\x0atest/value42").unwrap();
                if pass == 0 {
                    received.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                if pass == 1 {
                    let mut byte = [0];
                    assert_eq!(
                        stream.read(&mut byte).unwrap(),
                        0,
                        "dropping the connection must close its socket"
                    );
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
            next_matching_event(&runtime, &mut connection, |event| {
                matches!(event, BrokerEvent::Connected)
            });
            match next_event(&runtime, &mut connection) {
                BrokerEvent::Message(message) => {
                    assert_eq!(message.topic, "test/value");
                    assert_eq!(message.payload.as_ref(), b"42");
                    assert_eq!(message.qos, 0);
                    assert!(message.retained);
                }
                BrokerEvent::Status(status) => panic!("expected retained message, got: {status}"),
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
    fn reports_rejected_subscriptions() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let mut stream = handshake(&listener, 0x80);
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let runtime = runtime();
        let mut connection = connect(ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        })
        .unwrap();
        let event = next_matching_event(&runtime, &mut connection, |event| match event {
            BrokerEvent::Status(status) => status.contains("refused"),
            BrokerEvent::Connected => true,
            BrokerEvent::Message(_) | BrokerEvent::Connecting => false,
        });
        assert!(
            matches!(event, BrokerEvent::Status(_)),
            "a refused subscription is not connected"
        );
        drop(connection);
        broker.join().unwrap();
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
