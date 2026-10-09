//! English fallback messages for MQTT failures. Backend displays remain stable codes.

use crate::{
    mqtt::{MqttError, PublishAcknowledgement},
    ui::messages::LocalizedError,
};

impl LocalizedError for MqttError {
    fn localized_message(&self) -> String {
        match self {
            Self::Validation(error) => error.localized_message(),
            Self::MissingPassword => "No password is configured for this connection.".into(),
            Self::TopicEmpty => "MQTT topic names must not be empty.".into(),
            Self::TopicWildcard => "Publishing requires an exact topic name, not MQTT wildcards.".into(),
            Self::TopicTooLong { limit, .. } => format!("MQTT topic names must not exceed {limit} UTF-8 bytes."),
            Self::TopicNull => "MQTT topic names must not contain a null character.".into(),
            Self::ContentTypeTooLong { limit, .. } => format!("MQTT content type must not exceed {limit} UTF-8 bytes."),
            Self::ContentTypeNull => "MQTT content type must not contain a null character.".into(),
            Self::PublishQosUnsupported { maximum, .. } => format!("Broker supports a maximum publish QoS of {maximum}."),
            Self::RetainUnsupported => "Broker does not support retained publishes.".into(),
            Self::PublishPacketTooLarge { size, limit } => {
                format!("MQTT publish packet is {size} bytes; the maximum packet size is {limit} bytes.")
            }
            Self::SubscriptionPacketTooLarge { size, limit } => format!(
                "MQTT subscription packet is {size} bytes; the maximum packet size is {limit} bytes. Reduce the topic subscriptions and try again."
            ),
            Self::CommandQueueFull => "The MQTT command queue is full. Wait and try again.".into(),
            Self::WorkerStopped => "The MQTT worker has stopped. Reconnect and try again.".into(),
            Self::RuntimeCreation { .. } => "Could not create the MQTT runtime.".into(),
            Self::WorkerStart { .. } => "Could not start the MQTT worker thread.".into(),
            Self::TestRuntimeCreation { .. } => "Could not create the MQTT test runtime.".into(),
            Self::TestWorkerStart { .. } => "Could not start the MQTT connection test thread.".into(),
            Self::ConnectionTestTimeout { seconds } => format!(
                "The broker did not accept the connection within {seconds} seconds. Check the broker address and connection settings."
            ),
            Self::TrustedCertificatesUnavailable => "Could not load trusted system certificates for TLS.".into(),
            Self::TlsConfiguration { .. } => "Could not configure the TLS protocol versions.".into(),
            Self::PublishQueue { .. } => "Could not queue the MQTT publish.".into(),
            Self::DeletionQueue { .. } => "Could not queue a retained-topic deletion publish.".into(),
            Self::SubscriptionQueue { .. } => "Could not subscribe to topics.".into(),
            Self::PublishDisconnected => "Publish cancelled while disconnected. Publish again while connected.".into(),
            Self::DeletionDisconnected => "Retained-topic deletion cancelled while disconnected. Retry while connected.".into(),
            Self::PublishInterrupted => {
                "Publish interrupted by disconnect; it may already have reached the broker. It will not be retried.".into()
            }
            Self::DeletionInterrupted => {
                "Retained-topic deletion interrupted by disconnect; some topics may already have been cleared. It will not be retried."
                    .into()
            }
            Self::UnexpectedTopicAlias { alias, maximum } => {
                format!("Broker sent topic alias {alias}, but the negotiated alias maximum is {maximum}.")
            }
            Self::IncomingTopicUtf8 { .. } => "Broker sent a non-UTF-8 MQTT topic.".into(),
            Self::IncomingTopicInvalid { .. } => "Broker sent an invalid MQTT topic.".into(),
            Self::ConnectionRejected { reason_code } => {
                format!("Broker rejected the connection (MQTT reason 0x{reason_code:02X}). Check your credentials and connection settings.")
            }
            Self::BrokerDisconnected { reason_code, .. } => format!("Broker disconnected (MQTT reason 0x{reason_code:02X})."),
            Self::PublishRejected {
                acknowledgement,
                reason_code,
                ..
            } => {
                let acknowledgement = match acknowledgement {
                    PublishAcknowledgement::PubAck => "PUBACK",
                    PublishAcknowledgement::PubRec => "PUBREC",
                    PublishAcknowledgement::PubComp => "PUBCOMP",
                };
                format!(
                    "Broker rejected publish ({acknowledgement}, MQTT reason 0x{reason_code:02X}). Check access permissions and broker capabilities."
                )
            }
            Self::SubscriptionsRejected { results, .. } => {
                let rejected = results
                    .iter()
                    .filter(|result| result.reason_code >= 0x80)
                    .map(|result| {
                        let reason_code = result.reason_code;
                        match &result.topic_filter {
                            Some(filter) => format!("“{filter}” (MQTT reason 0x{reason_code:02X})"),
                            None => format!("MQTT reason 0x{reason_code:02X}"),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Broker refused topic subscriptions: {rejected}. Check access permissions and broker capabilities.")
            }
            Self::SubscriptionAcknowledgementCount { expected, received } => {
                format!("Broker returned {received} subscription acknowledgements; expected {expected}. The connection is not ready.")
            }
            Self::SessionReset => "MQTT publish rejected; resetting the session.".into(),
            Self::ConnectionFailed { .. } => "MQTT connection failed. Check the broker address, network, and connection settings.".into(),
            Self::Reconnecting {
                source,
                retry_after_seconds,
            } => {
                let message = source.localized_message();
                format!("{message} Reconnecting in {retry_after_seconds} seconds.")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt::SubscriptionResult;

    #[test]
    fn dependency_prose_is_diagnostic_only() {
        let error = MqttError::RuntimeCreation {
            source: std::io::Error::other("dependency implementation detail"),
        };
        assert_eq!(error.localized_message(), "Could not create the MQTT runtime.");
        assert_eq!(error.to_string(), "mqtt.runtime_creation");
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn broker_prose_and_debug_names_are_not_localized() {
        let errors = [
            MqttError::BrokerDisconnected {
                reason_code: 0x8b,
                diagnostic_reason: Some("ServerShuttingDown: private broker prose".into()),
            },
            MqttError::PublishRejected {
                packet_id: 17,
                acknowledgement: PublishAcknowledgement::PubAck,
                reason_code: 0x87,
                diagnostic_reason: Some("NotAuthorized: private broker prose".into()),
            },
            MqttError::SubscriptionsRejected {
                results: vec![SubscriptionResult {
                    topic_filter: Some("home/#".into()),
                    reason_code: 0x87,
                }],
                diagnostic_reason: Some("NotAuthorized: private broker prose".into()),
            },
        ];
        for error in errors {
            let message = error.localized_message();
            assert!(!message.contains("private broker prose"));
            assert!(!message.contains("ServerShuttingDown"));
            assert!(!message.contains("NotAuthorized"));
            assert_eq!(error.to_string(), error.code());
        }
    }

    #[test]
    fn subscription_messages_identify_only_rejected_filters_and_numeric_reasons() {
        let error = MqttError::SubscriptionsRejected {
            results: vec![
                SubscriptionResult {
                    topic_filter: Some("accepted/#".into()),
                    reason_code: 1,
                },
                SubscriptionResult {
                    topic_filter: Some("denied/#".into()),
                    reason_code: 0x87,
                },
            ],
            diagnostic_reason: None,
        };
        assert_eq!(
            error.localized_message(),
            "Broker refused topic subscriptions: “denied/#” (MQTT reason 0x87). Check access permissions and broker capabilities."
        );
    }

    #[test]
    fn subscription_packet_message_explains_size_limit_and_recovery() {
        let error = MqttError::SubscriptionPacketTooLarge { size: 21, limit: 20 };
        assert_eq!(
            error.localized_message(),
            "MQTT subscription packet is 21 bytes; the maximum packet size is 20 bytes. Reduce the topic subscriptions and try again."
        );
    }

    #[test]
    fn limits_and_retry_delay_are_interpolated_from_structured_data() {
        let error = MqttError::Reconnecting {
            source: Box::new(MqttError::PublishPacketTooLarge { size: 21, limit: 20 }),
            retry_after_seconds: 2,
        };
        assert_eq!(
            error.localized_message(),
            "MQTT publish packet is 21 bytes; the maximum packet size is 20 bytes. Reconnecting in 2 seconds."
        );
        assert_eq!(error.to_string(), "mqtt.reconnecting");
    }
}
