//! MQTT failures, stable diagnostic codes, and protocol error conversion.

use std::fmt;

use rumqttc::tokio_rustls::rustls;
use rumqttc::v5::{
    ClientError, ConnectionError, StateError,
    mqttbytes::v5::{ConnectReturnCode, Packet, PubAckReason, PubCompReason, PubRecReason, SubscribeReasonCode},
};
use tokio::sync::mpsc;

use super::config::ConnectionValidationError;

/// MQTT failures carry data for localization; `Display` is a stable diagnostic code only.
/// `Debug` includes retained sources and broker details; use localized messages for UI presentation.
#[derive(Debug)]
#[expect(dead_code, reason = "Some error context is only used by derived Debug diagnostics.")]
pub enum MqttError {
    Validation(ConnectionValidationError),
    MissingPassword,
    TopicEmpty,
    TopicWildcard,
    TopicTooLong {
        length: usize,
        limit: usize,
    },
    TopicNull,
    ContentTypeTooLong {
        length: usize,
        limit: usize,
    },
    ContentTypeNull,
    PublishQosUnsupported {
        requested: u8,
        maximum: u8,
    },
    RetainUnsupported,
    PublishPacketTooLarge {
        size: usize,
        limit: u32,
    },
    SubscriptionPacketTooLarge {
        size: usize,
        limit: u32,
    },
    CommandQueueFull,
    WorkerStopped,
    RuntimeCreation {
        source: std::io::Error,
    },
    WorkerStart {
        source: std::io::Error,
    },
    TestRuntimeCreation {
        source: std::io::Error,
    },
    TestWorkerStart {
        source: std::io::Error,
    },
    ConnectionTestTimeout {
        seconds: u64,
    },
    TrustedCertificatesUnavailable,
    TlsConfiguration {
        source: rustls::Error,
    },
    PublishQueue {
        source: Box<ClientError>,
    },
    DeletionQueue {
        source: Box<ClientError>,
    },
    SubscriptionQueue {
        source: Box<ClientError>,
    },
    PublishDisconnected,
    DeletionDisconnected,
    PublishInterrupted,
    DeletionInterrupted,
    UnexpectedTopicAlias {
        alias: u16,
        maximum: u16,
    },
    IncomingTopicUtf8 {
        source: std::str::Utf8Error,
    },
    IncomingTopicInvalid {
        source: Box<MqttError>,
    },
    ConnectionRejected {
        reason_code: u8,
    },
    BrokerDisconnected {
        reason_code: u8,
        diagnostic_reason: Option<String>,
    },
    PublishRejected {
        packet_id: u16,
        acknowledgement: PublishAcknowledgement,
        reason_code: u8,
        diagnostic_reason: Option<String>,
    },
    SubscriptionsRejected {
        results: Vec<SubscriptionResult>,
        diagnostic_reason: Option<String>,
    },
    SubscriptionAcknowledgementCount {
        expected: usize,
        received: usize,
    },
    SessionReset,
    ConnectionFailed {
        source: Box<ConnectionError>,
    },
    Reconnecting {
        source: Box<MqttError>,
        retry_after_seconds: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishAcknowledgement {
    PubAck,
    PubRec,
    PubComp,
}

/// A SUBACK result paired with its requested filter, if the broker returned a matching entry.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SubscriptionResult {
    pub topic_filter: Option<String>,
    pub reason_code: u8,
}

impl MqttError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "mqtt.connection_validation",
            Self::MissingPassword => "mqtt.missing_password",
            Self::TopicEmpty => "mqtt.topic_empty",
            Self::TopicWildcard => "mqtt.topic_wildcard",
            Self::TopicTooLong { .. } => "mqtt.topic_too_long",
            Self::TopicNull => "mqtt.topic_null",
            Self::ContentTypeTooLong { .. } => "mqtt.content_type_too_long",
            Self::ContentTypeNull => "mqtt.content_type_null",
            Self::PublishQosUnsupported { .. } => "mqtt.publish_qos_unsupported",
            Self::RetainUnsupported => "mqtt.retain_unsupported",
            Self::PublishPacketTooLarge { .. } => "mqtt.publish_packet_too_large",
            Self::SubscriptionPacketTooLarge { .. } => "mqtt.subscription_packet_too_large",
            Self::CommandQueueFull => "mqtt.command_queue_full",
            Self::WorkerStopped => "mqtt.worker_stopped",
            Self::RuntimeCreation { .. } => "mqtt.runtime_creation",
            Self::WorkerStart { .. } => "mqtt.worker_start",
            Self::TestRuntimeCreation { .. } => "mqtt.test_runtime_creation",
            Self::TestWorkerStart { .. } => "mqtt.test_worker_start",
            Self::ConnectionTestTimeout { .. } => "mqtt.connection_test_timeout",
            Self::TrustedCertificatesUnavailable => "mqtt.trusted_certificates_unavailable",
            Self::TlsConfiguration { .. } => "mqtt.tls_configuration",
            Self::PublishQueue { .. } => "mqtt.publish_queue",
            Self::DeletionQueue { .. } => "mqtt.deletion_queue",
            Self::SubscriptionQueue { .. } => "mqtt.subscription_queue",
            Self::PublishDisconnected => "mqtt.publish_disconnected",
            Self::DeletionDisconnected => "mqtt.deletion_disconnected",
            Self::PublishInterrupted => "mqtt.publish_interrupted",
            Self::DeletionInterrupted => "mqtt.deletion_interrupted",
            Self::UnexpectedTopicAlias { .. } => "mqtt.unexpected_topic_alias",
            Self::IncomingTopicUtf8 { .. } => "mqtt.incoming_topic_utf8",
            Self::IncomingTopicInvalid { .. } => "mqtt.incoming_topic_invalid",
            Self::ConnectionRejected { .. } => "mqtt.connection_rejected",
            Self::BrokerDisconnected { .. } => "mqtt.broker_disconnected",
            Self::PublishRejected { .. } => "mqtt.publish_rejected",
            Self::SubscriptionsRejected { .. } => "mqtt.subscriptions_rejected",
            Self::SubscriptionAcknowledgementCount { .. } => "mqtt.subscription_acknowledgement_count",
            Self::SessionReset => "mqtt.session_reset",
            Self::ConnectionFailed { .. } => "mqtt.connection_failed",
            Self::Reconnecting { .. } => "mqtt.reconnecting",
        }
    }
}

impl fmt::Display for MqttError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for MqttError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(source) => Some(source),
            Self::RuntimeCreation { source }
            | Self::WorkerStart { source }
            | Self::TestRuntimeCreation { source }
            | Self::TestWorkerStart { source } => Some(source),
            Self::TlsConfiguration { source } => Some(source),
            Self::PublishQueue { source } | Self::DeletionQueue { source } | Self::SubscriptionQueue { source } => Some(source.as_ref()),
            Self::IncomingTopicUtf8 { source } => Some(source),
            Self::IncomingTopicInvalid { source } | Self::Reconnecting { source, .. } => Some(source.as_ref()),
            Self::ConnectionFailed { source } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<ConnectionValidationError> for MqttError {
    fn from(error: ConnectionValidationError) -> Self {
        Self::Validation(error)
    }
}

impl From<ConnectionError> for MqttError {
    fn from(error: ConnectionError) -> Self {
        match error {
            ConnectionError::ConnectionRefused(reason) | ConnectionError::MqttState(StateError::ConnFail { reason }) => {
                Self::ConnectionRejected {
                    reason_code: connect_reason_code(reason),
                }
            }
            ConnectionError::MqttState(StateError::ServerDisconnect {
                reason_code,
                reason_string,
            }) => Self::BrokerDisconnected {
                reason_code: reason_code as u8,
                diagnostic_reason: reason_string,
            },
            source => Self::ConnectionFailed { source: Box::new(source) },
        }
    }
}

pub(crate) fn command_queue_error<T>(error: mpsc::error::TrySendError<T>) -> MqttError {
    match error {
        mpsc::error::TrySendError::Full(_) => MqttError::CommandQueueFull,
        mpsc::error::TrySendError::Closed(_) => MqttError::WorkerStopped,
    }
}

// rumqttc's CONNACK, SUBACK, PUBACK and PUBREC enum discriminants are not wire codes.
pub(crate) fn connect_reason_code(reason: ConnectReturnCode) -> u8 {
    match reason {
        ConnectReturnCode::Success => 0x00,
        ConnectReturnCode::RefusedProtocolVersion => 0x01,
        ConnectReturnCode::BadClientId => 0x02,
        ConnectReturnCode::ServiceUnavailable => 0x03,
        ConnectReturnCode::UnspecifiedError => 0x80,
        ConnectReturnCode::MalformedPacket => 0x81,
        ConnectReturnCode::ProtocolError => 0x82,
        ConnectReturnCode::ImplementationSpecificError => 0x83,
        ConnectReturnCode::UnsupportedProtocolVersion => 0x84,
        ConnectReturnCode::ClientIdentifierNotValid => 0x85,
        ConnectReturnCode::BadUserNamePassword => 0x86,
        ConnectReturnCode::NotAuthorized => 0x87,
        ConnectReturnCode::ServerUnavailable => 0x88,
        ConnectReturnCode::ServerBusy => 0x89,
        ConnectReturnCode::Banned => 0x8a,
        ConnectReturnCode::BadAuthenticationMethod => 0x8c,
        ConnectReturnCode::TopicNameInvalid => 0x90,
        ConnectReturnCode::PacketTooLarge => 0x95,
        ConnectReturnCode::QuotaExceeded => 0x97,
        ConnectReturnCode::PayloadFormatInvalid => 0x99,
        ConnectReturnCode::RetainNotSupported => 0x9a,
        ConnectReturnCode::QoSNotSupported => 0x9b,
        ConnectReturnCode::UseAnotherServer => 0x9c,
        ConnectReturnCode::ServerMoved => 0x9d,
        ConnectReturnCode::ConnectionRateExceeded => 0x9f,
    }
}

pub(crate) fn subscription_reason_code(reason: SubscribeReasonCode) -> u8 {
    match reason {
        SubscribeReasonCode::Success(qos) => qos as u8,
        SubscribeReasonCode::Failure | SubscribeReasonCode::Unspecified => 0x80,
        SubscribeReasonCode::ImplementationSpecific => 0x83,
        SubscribeReasonCode::NotAuthorized => 0x87,
        SubscribeReasonCode::TopicFilterInvalid => 0x8f,
        SubscribeReasonCode::PkidInUse => 0x91,
        SubscribeReasonCode::QuotaExceeded => 0x97,
        SubscribeReasonCode::SharedSubscriptionsNotSupported => 0x9e,
        SubscribeReasonCode::SubscriptionIdNotSupported => 0xa1,
        SubscribeReasonCode::WildcardSubscriptionsNotSupported => 0xa2,
    }
}

fn puback_reason_code(reason: PubAckReason) -> u8 {
    match reason {
        PubAckReason::Success => 0x00,
        PubAckReason::NoMatchingSubscribers => 0x10,
        PubAckReason::UnspecifiedError => 0x80,
        PubAckReason::ImplementationSpecificError => 0x83,
        PubAckReason::NotAuthorized => 0x87,
        PubAckReason::TopicNameInvalid => 0x90,
        PubAckReason::PacketIdentifierInUse => 0x91,
        PubAckReason::QuotaExceeded => 0x97,
        PubAckReason::PayloadFormatInvalid => 0x99,
    }
}

fn pubrec_reason_code(reason: PubRecReason) -> u8 {
    match reason {
        PubRecReason::Success => 0x00,
        PubRecReason::NoMatchingSubscribers => 0x10,
        PubRecReason::UnspecifiedError => 0x80,
        PubRecReason::ImplementationSpecificError => 0x83,
        PubRecReason::NotAuthorized => 0x87,
        PubRecReason::TopicNameInvalid => 0x90,
        PubRecReason::PacketIdentifierInUse => 0x91,
        PubRecReason::QuotaExceeded => 0x97,
        PubRecReason::PayloadFormatInvalid => 0x99,
    }
}

pub(crate) fn publish_rejection(packet: &Packet) -> Option<(u16, MqttError)> {
    let (pkid, acknowledgement, reason_code, detail) = match packet {
        Packet::PubAck(ack) if !matches!(ack.reason, PubAckReason::Success | PubAckReason::NoMatchingSubscribers) => (
            ack.pkid,
            PublishAcknowledgement::PubAck,
            puback_reason_code(ack.reason),
            ack.properties.as_ref().and_then(|properties| properties.reason_string.as_deref()),
        ),
        Packet::PubRec(ack) if !matches!(ack.reason, PubRecReason::Success | PubRecReason::NoMatchingSubscribers) => (
            ack.pkid,
            PublishAcknowledgement::PubRec,
            pubrec_reason_code(ack.reason),
            ack.properties.as_ref().and_then(|properties| properties.reason_string.as_deref()),
        ),
        Packet::PubComp(ack) if ack.reason != PubCompReason::Success => (
            ack.pkid,
            PublishAcknowledgement::PubComp,
            0x92,
            ack.properties.as_ref().and_then(|properties| properties.reason_string.as_deref()),
        ),
        _ => return None,
    };
    Some((
        pkid,
        MqttError::PublishRejected {
            packet_id: pkid,
            acknowledgement,
            reason_code,
            diagnostic_reason: detail.map(str::to_owned),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConnectionConfig;

    #[test]
    fn mqtt_errors_are_send_sync_and_display_stable_codes_only() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MqttError>();
        assert!(
            std::mem::size_of::<MqttError>() <= 128,
            "diagnostic sources must not inflate MQTT results"
        );
        let io_error = || std::io::Error::other("private dependency prose");
        let invalid_utf8 = vec![0xff];
        let utf8_error = std::str::from_utf8(&invalid_utf8).unwrap_err();
        let validation = ConnectionConfig {
            host: String::new(),
            ..Default::default()
        }
        .validate()
        .unwrap_err();
        let cases = [
            (MqttError::Validation(validation), "mqtt.connection_validation"),
            (MqttError::MissingPassword, "mqtt.missing_password"),
            (MqttError::TopicEmpty, "mqtt.topic_empty"),
            (MqttError::TopicWildcard, "mqtt.topic_wildcard"),
            (
                MqttError::TopicTooLong {
                    length: 65536,
                    limit: 65535,
                },
                "mqtt.topic_too_long",
            ),
            (MqttError::TopicNull, "mqtt.topic_null"),
            (
                MqttError::ContentTypeTooLong {
                    length: 65536,
                    limit: 65535,
                },
                "mqtt.content_type_too_long",
            ),
            (MqttError::ContentTypeNull, "mqtt.content_type_null"),
            (
                MqttError::PublishQosUnsupported { requested: 2, maximum: 1 },
                "mqtt.publish_qos_unsupported",
            ),
            (MqttError::RetainUnsupported, "mqtt.retain_unsupported"),
            (
                MqttError::PublishPacketTooLarge { size: 21, limit: 20 },
                "mqtt.publish_packet_too_large",
            ),
            (
                MqttError::SubscriptionPacketTooLarge { size: 21, limit: 20 },
                "mqtt.subscription_packet_too_large",
            ),
            (MqttError::CommandQueueFull, "mqtt.command_queue_full"),
            (MqttError::WorkerStopped, "mqtt.worker_stopped"),
            (MqttError::RuntimeCreation { source: io_error() }, "mqtt.runtime_creation"),
            (MqttError::WorkerStart { source: io_error() }, "mqtt.worker_start"),
            (MqttError::TestRuntimeCreation { source: io_error() }, "mqtt.test_runtime_creation"),
            (MqttError::TestWorkerStart { source: io_error() }, "mqtt.test_worker_start"),
            (MqttError::ConnectionTestTimeout { seconds: 10 }, "mqtt.connection_test_timeout"),
            (MqttError::TrustedCertificatesUnavailable, "mqtt.trusted_certificates_unavailable"),
            (
                MqttError::TlsConfiguration {
                    source: rustls::Error::General("private dependency prose".into()),
                },
                "mqtt.tls_configuration",
            ),
            (
                MqttError::PublishQueue {
                    source: Box::new(ClientError::Request(rumqttc::v5::Request::Disconnect)),
                },
                "mqtt.publish_queue",
            ),
            (
                MqttError::DeletionQueue {
                    source: Box::new(ClientError::Request(rumqttc::v5::Request::Disconnect)),
                },
                "mqtt.deletion_queue",
            ),
            (
                MqttError::SubscriptionQueue {
                    source: Box::new(ClientError::Request(rumqttc::v5::Request::Disconnect)),
                },
                "mqtt.subscription_queue",
            ),
            (MqttError::PublishDisconnected, "mqtt.publish_disconnected"),
            (MqttError::DeletionDisconnected, "mqtt.deletion_disconnected"),
            (MqttError::PublishInterrupted, "mqtt.publish_interrupted"),
            (MqttError::DeletionInterrupted, "mqtt.deletion_interrupted"),
            (
                MqttError::UnexpectedTopicAlias { alias: 1, maximum: 0 },
                "mqtt.unexpected_topic_alias",
            ),
            (MqttError::IncomingTopicUtf8 { source: utf8_error }, "mqtt.incoming_topic_utf8"),
            (
                MqttError::IncomingTopicInvalid {
                    source: Box::new(MqttError::TopicEmpty),
                },
                "mqtt.incoming_topic_invalid",
            ),
            (MqttError::ConnectionRejected { reason_code: 0x87 }, "mqtt.connection_rejected"),
            (
                MqttError::BrokerDisconnected {
                    reason_code: 0x8b,
                    diagnostic_reason: Some("private broker prose".into()),
                },
                "mqtt.broker_disconnected",
            ),
            (
                MqttError::PublishRejected {
                    packet_id: 1,
                    acknowledgement: PublishAcknowledgement::PubAck,
                    reason_code: 0x87,
                    diagnostic_reason: Some("private broker prose".into()),
                },
                "mqtt.publish_rejected",
            ),
            (
                MqttError::SubscriptionsRejected {
                    results: vec![SubscriptionResult {
                        topic_filter: Some("home/#".into()),
                        reason_code: 0x87,
                    }],
                    diagnostic_reason: Some("private broker prose".into()),
                },
                "mqtt.subscriptions_rejected",
            ),
            (
                MqttError::SubscriptionAcknowledgementCount { expected: 2, received: 1 },
                "mqtt.subscription_acknowledgement_count",
            ),
            (MqttError::SessionReset, "mqtt.session_reset"),
            (
                MqttError::ConnectionFailed {
                    source: Box::new(ConnectionError::Io(io_error())),
                },
                "mqtt.connection_failed",
            ),
            (
                MqttError::Reconnecting {
                    source: Box::new(MqttError::SessionReset),
                    retry_after_seconds: 2,
                },
                "mqtt.reconnecting",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.code(), expected);
            assert_eq!(error.to_string(), expected);
            assert_eq!(format!("{error:#}"), expected);
        }
    }
}
