#![cfg(test)]

pub mod messages {
    use crate::SnowflakePosition;
    use crate::messages::{BatchType, Message};
    use crate::utils::get_next_message_position_sync;

    /// Create a simple two-message batch: user then assistant.
    /// Returns (user_msg, assistant_msg, batch_id).
    pub fn simple_user_assistant_batch(
        user_text: impl Into<String>,
        assistant_text: impl Into<String>,
    ) -> (Message, Message, SnowflakePosition) {
        let batch_id = get_next_message_position_sync();
        let user = Message::user_in_batch(batch_id, 0, user_text.into());
        let mut assistant = Message::assistant_in_batch(batch_id, 1, assistant_text.into());
        if assistant.batch_type.is_none() {
            assistant.batch_type = Some(BatchType::UserRequest);
        }
        (user, assistant, batch_id)
    }
}
