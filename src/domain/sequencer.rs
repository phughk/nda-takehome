use crate::domain::amount::Amount;
use crate::message::InputMessage;
use crate::service::error::TransactionError;
use crate::{ClientId, ServiceMessage, TransactionId, TransactionType};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Default)]
pub struct MessageSequencer(u64);

impl MessageSequencer {
    pub fn create_message(
        &mut self,
        client_id: ClientId,
        transaction_id: TransactionId,
        amount: i64,
        transaction_type: TransactionType,
        sx: UnboundedSender<(ClientId, TransactionId, Result<(), TransactionError>)>,
    ) -> ServiceMessage {
        let chrono_order = self.0;
        self.0 += 1;
        ServiceMessage::Incoming(
            Box::new(InputMessage {
                chrono_order,
                transaction_type,
                client_id,
                transaction_id,
                amount: Amount::from_major(amount),
            }),
            sx,
        )
    }
}
