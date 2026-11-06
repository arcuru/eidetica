//! Small-message fast paths obey the same byte policy as streamed messages.

use super::*;
use crate::sync::protocol::SyncResponse;

#[tokio::test]
async fn inline_messages_cannot_bypass_the_message_limit() {
    let policy = TransferPolicy {
        max_message_bytes: 1,
        ..Default::default()
    };
    let budget = TransferBudget::new(policy).unwrap();
    assert!(matches!(
        prepare_message(SyncResponse::Count(7), &budget).await,
        Err(TransferError::Resource {
            limit: ResourceLimit::MessageBytes,
            ..
        })
    ));
}

#[tokio::test]
async fn inline_messages_share_the_reserved_byte_budget() {
    let policy = TransferPolicy {
        max_message_bytes: 32,
        max_reserved_bytes: 32,
        ..Default::default()
    };
    let budget = TransferBudget::new(policy).unwrap();
    let mut messages = Vec::new();
    for _ in 0..8 {
        match prepare_message(SyncResponse::Count(7), &budget).await {
            Ok(message) => messages.push(message),
            Err(TransferError::Resource {
                limit: ResourceLimit::ReservedBytes,
                ..
            }) => {
                assert!(!messages.is_empty());
                drop(messages);
                assert_eq!(budget.usage(), TransferUsage::default());
                return;
            }
            Err(other) => panic!("unexpected: {other}"),
        }
    }
    panic!("inline messages escaped byte accounting");
}
