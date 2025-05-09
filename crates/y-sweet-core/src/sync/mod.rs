//! Forked from [y-sync](https://github.com/y-crdt/y-sync/tree/master)

pub mod awareness;

use awareness::{Awareness, AwarenessUpdate};
use thiserror::Error;
use yrs::updates::decoder::{Decode, Decoder};
use yrs::updates::encoder::{Encode, Encoder};
use yrs::{ReadTxn, StateVector, Transact, Update};

/*
 Core Yjs defines two message types:
 • YjsSyncStep1: Includes the State Set of the sending client. When received, the client should reply with YjsSyncStep2.
 • YjsSyncStep2: Includes all missing structs and the complete delete set. When received, the client is assured that it
   received all information from the remote client.

 In a peer-to-peer network, you may want to introduce a SyncDone message type. Both parties should initiate the connection
 with SyncStep1. When a client received SyncStep2, it should reply with SyncDone. When the local client received both
 SyncStep2 and SyncDone, it is assured that it is synced to the remote client.

 In a client-server model, you want to handle this differently: The client should initiate the connection with SyncStep1.
 When the server receives SyncStep1, it should reply with SyncStep2 immediately followed by SyncStep1. The client replies
 with SyncStep2 when it receives SyncStep1. Optionally the server may send a SyncDone after it received SyncStep2, so the
 client knows that the sync is finished.  There are two reasons for this more elaborated sync model: 1. This protocol can
 easily be implemented on top of http and websockets. 2. The server should only reply to requests, and not initiate them.
 Therefore, it is necessary that the client initiates the sync.

 Construction of a message:
 [messageType : varUint, message definition..]

 Note: A message does not include information about the room name. This must be handled by the upper layer protocol!

 stringify[messageType] stringifies a message definition (messageType is already read from the buffer)
*/

/// A default implementation of y-sync [Protocol].
pub struct DefaultProtocol;

impl Protocol for DefaultProtocol {}

/// Trait implementing a y-sync protocol. The default implementation can be found in
/// [DefaultProtocol], but its implementation steps can be potentially changed by the user if
/// necessary.
pub trait Protocol {
    /// To be called whenever a new connection has been accepted. Returns an encoded list of
    /// messages to be sent back to initiator. This binary may contain multiple messages inside,
    /// stored one after another.
    fn start<E: Encoder>(&self, awareness: &Awareness, encoder: &mut E) -> Result<(), Error> {
        let (sv, update) = {
            let txn = awareness.doc().try_transact();
            match txn {
                Ok(txn) => {
                    let sv = txn.state_vector();
                    let update = awareness.update()?;
                    (sv, update)
                }
                Err(e) => return Err(Error::Other(Box::new(e))),
            }
        };
        Message::Sync(SyncMessage::SyncStep1(sv)).encode(encoder);
        Message::Awareness(update).encode(encoder);
        Ok(())
    }

    /// Y-sync protocol sync-step-1 - given a [StateVector] of a remote side, calculate missing
    /// updates. Returns a sync-step-2 message containing a calculated update.
    fn handle_sync_step1(
        &self,
        awareness: &Awareness,
        sv: StateVector,
    ) -> Result<Option<Message>, Error> {
        let txn = awareness.doc().try_transact();
        match txn {
            Ok(txn) => {
                let update = txn.encode_state_as_update_v1(&sv);
                Ok(Some(Message::Sync(SyncMessage::SyncStep2(update))))
            }
            Err(e) => Err(Error::Other(Box::new(e))),
        }
    }

    /// Handle reply for a sync-step-1 send from this replica previously. By default, just apply
    /// an update to current `awareness` document instance.
    fn handle_sync_step2(
        &self,
        awareness: &mut Awareness,
        update: Update,
    ) -> Result<Option<Message>, Error> {
        let txn = awareness.doc().try_transact_mut();
        match txn {
            Ok(mut txn) => {
                txn.apply_update(update);
                Ok(None)
            }
            Err(e) => Err(Error::Other(Box::new(e))),
        }
    }

    /// Handle continuous update send from the client. By default just apply an update to a current
    /// `awareness` document instance.
    fn handle_update(
        &self,
        awareness: &mut Awareness,
        update: Update,
    ) -> Result<Option<Message>, Error> {
        self.handle_sync_step2(awareness, update)
    }

    /// Handle authorization message. By default, if reason for auth denial has been provided,
    /// send back [Error::PermissionDenied].
    fn handle_auth(
        &self,
        _awareness: &Awareness,
        deny_reason: Option<String>,
    ) -> Result<Option<Message>, Error> {
        if let Some(reason) = deny_reason {
            Err(Error::PermissionDenied { reason })
        } else {
            Ok(None)
        }
    }

    /// Returns an [AwarenessUpdate] which is a serializable representation of a current `awareness`
    /// instance.
    fn handle_awareness_query(&self, awareness: &Awareness) -> Result<Option<Message>, Error> {
        let update = awareness.update()?;
        Ok(Some(Message::Awareness(update)))
    }

    /// Reply to awareness query or just incoming [AwarenessUpdate], where current `awareness`
    /// instance is being updated with incoming data.
    fn handle_awareness_update(
        &self,
        awareness: &mut Awareness,
        update: AwarenessUpdate,
    ) -> Result<Option<Message>, Error> {
        awareness.apply_update(update)?;
        Ok(None)
    }

    /// Y-sync protocol enables to extend its own settings with custom handles. These can be
    /// implemented here. By default, it returns an [Error::Unsupported].
    fn missing_handle(
        &self,
        _awareness: &mut Awareness,
        tag: u8,
        _data: Vec<u8>,
    ) -> Result<Option<Message>, Error> {
        Err(Error::Unsupported(tag))
    }
}

/// Tag id for [Message::Sync].
pub const MSG_SYNC: u8 = 0;
/// Tag id for [Message::Awareness].
pub const MSG_AWARENESS: u8 = 1;
/// Tag id for [Message::Auth].
pub const MSG_AUTH: u8 = 2;
/// Tag id for [Message::AwarenessQuery].
pub const MSG_QUERY_AWARENESS: u8 = 3;

pub const PERMISSION_DENIED: u8 = 0;
pub const PERMISSION_GRANTED: u8 = 1;

#[derive(Debug, Eq, PartialEq)]
pub enum Message {
    Sync(SyncMessage),
    Auth(Option<String>),
    AwarenessQuery,
    Awareness(AwarenessUpdate),
    Custom(u8, Vec<u8>),
}

impl Encode for Message {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        match self {
            Message::Sync(msg) => {
                encoder.write_var(MSG_SYNC);
                msg.encode(encoder);
            }
            Message::Auth(reason) => {
                encoder.write_var(MSG_AUTH);
                if let Some(reason) = reason {
                    encoder.write_var(PERMISSION_DENIED);
                    encoder.write_string(reason);
                } else {
                    encoder.write_var(PERMISSION_GRANTED);
                }
            }
            Message::AwarenessQuery => {
                encoder.write_var(MSG_QUERY_AWARENESS);
            }
            Message::Awareness(update) => {
                encoder.write_var(MSG_AWARENESS);
                encoder.write_buf(update.encode_v1())
            }
            Message::Custom(tag, data) => {
                encoder.write_u8(*tag);
                encoder.write_buf(data);
            }
        }
    }
}

impl Decode for Message {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, yrs::encoding::read::Error> {
        let tag: u8 = decoder.read_var()?;
        match tag {
            MSG_SYNC => {
                let msg = SyncMessage::decode(decoder)?;
                Ok(Message::Sync(msg))
            }
            MSG_AWARENESS => {
                let data = decoder.read_buf()?;
                let update = AwarenessUpdate::decode_v1(data)?;
                Ok(Message::Awareness(update))
            }
            MSG_AUTH => {
                let reason = if decoder.read_var::<u8>()? == PERMISSION_DENIED {
                    Some(decoder.read_string()?.to_string())
                } else {
                    None
                };
                Ok(Message::Auth(reason))
            }
            MSG_QUERY_AWARENESS => Ok(Message::AwarenessQuery),
            tag => {
                let data = decoder.read_buf()?;
                Ok(Message::Custom(tag, data.to_vec()))
            }
        }
    }
}

/// Tag id for [SyncMessage::SyncStep1].
pub const MSG_SYNC_STEP_1: u8 = 0;
/// Tag id for [SyncMessage::SyncStep2].
pub const MSG_SYNC_STEP_2: u8 = 1;
/// Tag id for [SyncMessage::Update].
pub const MSG_SYNC_UPDATE: u8 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum SyncMessage {
    SyncStep1(StateVector),
    SyncStep2(Vec<u8>),
    Update(Vec<u8>),
}

impl Encode for SyncMessage {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        match self {
            SyncMessage::SyncStep1(sv) => {
                encoder.write_var(MSG_SYNC_STEP_1);
                encoder.write_buf(sv.encode_v1());
            }
            SyncMessage::SyncStep2(u) => {
                encoder.write_var(MSG_SYNC_STEP_2);
                encoder.write_buf(u);
            }
            SyncMessage::Update(u) => {
                encoder.write_var(MSG_SYNC_UPDATE);
                encoder.write_buf(u);
            }
        }
    }
}

impl Decode for SyncMessage {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, yrs::encoding::read::Error> {
        let tag: u8 = decoder.read_var()?;
        match tag {
            MSG_SYNC_STEP_1 => {
                let buf = decoder.read_buf()?;
                let sv = StateVector::decode_v1(buf)?;
                Ok(SyncMessage::SyncStep1(sv))
            }
            MSG_SYNC_STEP_2 => {
                let buf = decoder.read_buf()?;
                Ok(SyncMessage::SyncStep2(buf.into()))
            }
            MSG_SYNC_UPDATE => {
                let buf = decoder.read_buf()?;
                Ok(SyncMessage::Update(buf.into()))
            }
            _ => Err(yrs::encoding::read::Error::UnexpectedValue),
        }
    }
}

/// An error type returned in response from y-sync [Protocol].
#[derive(Debug, Error)]
pub enum Error {
    /// Incoming Y-protocol message couldn't be deserialized.
    #[error("failed to deserialize message: {0}")]
    EncodingError(#[from] yrs::encoding::read::Error),

    /// Applying incoming Y-protocol awareness update has failed.
    #[error("failed to process awareness update: {0}")]
    AwarenessEncoding(#[from] awareness::Error),

    /// An incoming Y-protocol authorization request has been denied.
    #[error("permission denied to access: {reason}")]
    PermissionDenied { reason: String },

    /// Thrown whenever an unknown message tag has been sent.
    #[error("unsupported message tag identifier: {0}")]
    Unsupported(u8),

    /// Thrown whenever a poisoned lock has been encountered.
    #[error("poisoned lock")]
    PoisonedLock,

    /// Thrown whenever a poisoned lock has been encountered.
    #[error("no next client id")]
    NoNextClientId,

    /// Custom dynamic kind of error, usually related to a warp internal error messages.
    #[error("internal failure: {0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Since y-sync protocol enables for a multiple messages to be packed into a singe byte payload,
/// [MessageReader] can be used over the decoder to read these messages one by one in iterable
/// fashion.
pub struct MessageReader<'a, D: Decoder>(&'a mut D);

impl<'a, D: Decoder> MessageReader<'a, D> {
    pub fn new(decoder: &'a mut D) -> Self {
        MessageReader(decoder)
    }
}

impl<'a, D: Decoder> Iterator for MessageReader<'a, D> {
    type Item = Result<Message, yrs::encoding::read::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match Message::decode(self.0) {
            Ok(msg) => Some(Ok(msg)),
            Err(yrs::encoding::read::Error::EndOfBuffer(_)) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::{Message, SyncMessage};
    use crate::sync::awareness::Awareness;
    use crate::sync::{DefaultProtocol, MessageReader, Protocol};
    use std::collections::HashMap;
    use yrs::encoding::read::Cursor;
    use yrs::updates::decoder::{Decode, DecoderV1};
    use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
    use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

    #[test]
    fn message_encoding() {
        let doc = Doc::new();
        let txt = doc.get_or_insert_text("text");
        txt.push(&mut doc.transact_mut(), "hello world");
        let mut awareness = Awareness::new(doc);
        awareness.set_local_state("{\"user\":{\"name\":\"Anonymous 50\",\"color\":\"#30bced\",\"colorLight\":\"#30bced33\"}}");

        let messages = [
            Message::Sync(SyncMessage::SyncStep1(
                awareness.doc().transact().state_vector(),
            )),
            Message::Sync(SyncMessage::SyncStep2(
                awareness
                    .doc()
                    .transact()
                    .encode_state_as_update_v1(&StateVector::default()),
            )),
            Message::Awareness(awareness.update().unwrap()),
            Message::Auth(Some("reason".to_string())),
            Message::AwarenessQuery,
        ];

        for msg in messages {
            let encoded = msg.encode_v1();
            let decoded = Message::decode_v1(&encoded)
                .unwrap_or_else(|_| panic!("failed to decode {:?}", msg));
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn protocol_init() {
        let awareness = Awareness::default();
        let protocol = DefaultProtocol;
        let mut encoder = EncoderV1::new();
        protocol.start(&awareness, &mut encoder).unwrap();
        let data = encoder.to_vec();
        let mut decoder = DecoderV1::new(Cursor::new(&data));
        let mut reader = MessageReader::new(&mut decoder);

        assert_eq!(
            reader.next().unwrap().unwrap(),
            Message::Sync(SyncMessage::SyncStep1(StateVector::default()))
        );

        assert_eq!(
            reader.next().unwrap().unwrap(),
            Message::Awareness(awareness.update().unwrap())
        );

        assert!(reader.next().is_none());
    }

    #[test]
    fn protocol_sync_steps() {
        let protocol = DefaultProtocol;

        let mut a1 = Awareness::new(Doc::with_client_id(1));
        let mut a2 = Awareness::new(Doc::with_client_id(2));

        let expected = {
            let txt = a1.doc_mut().get_or_insert_text("test");
            let mut txn = a1.doc_mut().transact_mut();
            txt.push(&mut txn, "hello");
            txn.encode_state_as_update_v1(&StateVector::default())
        };

        let result = protocol
            .handle_sync_step1(&a1, a2.doc().transact().state_vector())
            .unwrap();

        assert_eq!(
            result,
            Some(Message::Sync(SyncMessage::SyncStep2(expected)))
        );

        if let Some(Message::Sync(SyncMessage::SyncStep2(u))) = result {
            let result2 = protocol
                .handle_sync_step2(&mut a2, Update::decode_v1(&u).unwrap())
                .unwrap();

            assert!(result2.is_none());
        }

        let txt = a2.doc().transact().get_text("test").unwrap();
        assert_eq!(txt.get_string(&a2.doc().transact()), "hello".to_owned());
    }

    #[test]
    fn protocol_sync_step_update() {
        let protocol = DefaultProtocol;

        let mut a1 = Awareness::new(Doc::with_client_id(1));
        let mut a2 = Awareness::new(Doc::with_client_id(2));

        let data = {
            let txt = a1.doc_mut().get_or_insert_text("test");
            let mut txn = a1.doc_mut().transact_mut();
            txt.push(&mut txn, "hello");
            txn.encode_update_v1()
        };

        let result = protocol
            .handle_update(&mut a2, Update::decode_v1(&data).unwrap())
            .unwrap();

        assert!(result.is_none());

        let txt = a2.doc().transact().get_text("test").unwrap();
        assert_eq!(txt.get_string(&a2.doc().transact()), "hello".to_owned());
    }

    #[test]
    fn protocol_awareness_sync() {
        let protocol = DefaultProtocol;

        let mut a1 = Awareness::new(Doc::with_client_id(1));
        let mut a2 = Awareness::new(Doc::with_client_id(2));

        a1.set_local_state("{x:3}");
        let result = protocol.handle_awareness_query(&a1).unwrap();

        assert_eq!(result, Some(Message::Awareness(a1.update().unwrap())));

        if let Some(Message::Awareness(u)) = result {
            let result = protocol.handle_awareness_update(&mut a2, u).unwrap();
            assert!(result.is_none());
        }

        assert_eq!(a2.clients(), &HashMap::from([(1, "{x:3}".to_owned())]));
    }
}
