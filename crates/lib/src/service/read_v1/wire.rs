use crate::{
    ID, Snapshot as Cut,
    auth::{
        DelegationStep, KeyHint, PublicKey, SigKey, validation::delegation::MAX_DELEGATION_STEPS,
    },
    state::{ComputedDoc, ReadContext, ReadScope, work::MAX_TIPS},
};

use super::ReadError;

// Keep the generated transport DTOs out of the Rust SDK surface.
pub(super) mod pb {
    tonic::include_proto!("eidetica.service.v1");
}
pub(super) use pb::*;

pub(super) const REVISION: u32 = 1;
pub(super) const CHUNK_BYTES: usize = 64 * 1024;
pub(super) const MESSAGE_BYTES: usize = 2 * CHUNK_BYTES;
pub(super) const OBJECT_BYTES: usize = 32 * 1024 * 1024;
pub(super) const HISTORY_BYTES: usize = 128 * 1024 * 1024;
pub(super) const HISTORY_ENTRIES: usize = 4096;

pub(super) fn id(s: &str) -> Result<ID, ReadError> {
    let id = ID::parse(s).map_err(|_| ReadError::InvalidData("invalid CID"))?;
    if !id
        .as_cid()
        .is_some_and(|cid| cid.codec() == 0x71 && cid.version() == cid::Version::V1)
        || id.to_string() != s
    {
        return Err(ReadError::InvalidData("expected canonical DAG-CBOR CID"));
    }
    Ok(id)
}

pub(super) fn cut(value: Option<&Snapshot>) -> Result<Cut, ReadError> {
    let tips = &value.ok_or(ReadError::InvalidBoundary)?.tips;
    if tips.is_empty() {
        return Err(ReadError::InvalidBoundary);
    }
    if tips.len() > MAX_TIPS {
        return Err(ReadError::Limit);
    }
    Ok(Cut::new(
        tips.iter().map(|tip| id(tip)).collect::<Result<_, _>>()?,
    ))
}

pub(super) fn snapshot(at: &Cut) -> Snapshot {
    Snapshot {
        tips: at.iter().map(ToString::to_string).collect(),
    }
}

pub(super) fn identity(value: &SigKey, pubkey: &PublicKey) -> Result<SignerIdentity, ReadError> {
    use signer_identity::Hint;
    let hint = value.hint();
    if hint.pubkey.as_ref().is_some_and(|k| k != pubkey) {
        return Err(ReadError::Denied);
    }
    let hint = match (&hint.pubkey, &hint.name, hint.is_global) {
        (_, None, true) => Hint::Global(true),
        (Some(key), None, false) => Hint::Key(key.to_prefixed_string()),
        (None, Some(name), false) => Hint::Name(name.clone()),
        _ => return Err(ReadError::InvalidData("invalid signer hint")),
    };
    let delegation = match value {
        SigKey::Direct { .. } => Vec::new(),
        SigKey::Delegation { path, .. } if path.is_empty() => {
            return Err(ReadError::InvalidData("empty delegation path"));
        }
        SigKey::Delegation { path, .. } => path.iter().map(|step| step.tree.to_string()).collect(),
    };
    Ok(SignerIdentity {
        hint: Some(hint),
        delegation,
    })
}

pub(super) struct Plan {
    pub database: ID,
    pub key: PublicKey,
    pub identity: SigKey,
    pub operation: Operation,
}

pub(super) enum Operation {
    Capture,
    State { at: Cut, store: String },
    History { at: Cut },
}

impl Plan {
    pub fn parse(request: Query) -> Result<Self, ReadError> {
        if request.revision != REVISION || !request.require.is_empty() {
            return Err(ReadError::Unsupported);
        }
        let database = id(&request.database)?;
        let key =
            PublicKey::from_prefixed_string(&request.public_key).map_err(|_| ReadError::Denied)?;
        if key.to_prefixed_string() != request.public_key {
            return Err(ReadError::Denied);
        }
        let identity = request.identity.ok_or(ReadError::Denied)?;
        if identity.delegation.len() > MAX_DELEGATION_STEPS {
            return Err(ReadError::Limit);
        }
        use signer_identity::Hint;
        let hint = match identity.hint {
            Some(Hint::Key(k)) if k == request.public_key => KeyHint::from_pubkey(&key),
            Some(Hint::Name(name)) if !name.is_empty() && name.len() <= 256 && name != "*" => {
                KeyHint::from_name(name)
            }
            Some(Hint::Global(true)) if identity.delegation.is_empty() => KeyHint::global(&key),
            _ => return Err(ReadError::Denied),
        };
        let path = identity
            .delegation
            .iter()
            .map(|tree| {
                Ok(DelegationStep {
                    tree: id(tree)?,
                    tips: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, ReadError>>()?;
        let identity = if path.is_empty() {
            SigKey::Direct { hint }
        } else {
            SigKey::Delegation { path, hint }
        };
        let operation = match request.operation {
            Some(query::Operation::Capture(_)) => Operation::Capture,
            Some(query::Operation::State(state)) => {
                if state.projection != ComputedDoc::PROJECTION
                    || state.store.is_empty()
                    || state.store.len() > 256
                {
                    return Err(ReadError::Unsupported);
                }
                Operation::State {
                    at: cut(state.at.as_ref())?,
                    store: state.store,
                }
            }
            Some(query::Operation::History(h)) => Operation::History {
                at: cut(h.at.as_ref())?,
            },
            None => return Err(ReadError::Unsupported),
        };
        Ok(Self {
            database,
            key,
            identity,
            operation,
        })
    }

    // A canonical, length-delimited semantic transcript; not Protobuf bytes.
    pub fn transcript(&self, challenge: &Challenge) -> Vec<u8> {
        fn field(dst: &mut Vec<u8>, data: &[u8]) {
            dst.extend_from_slice(&(data.len() as u32).to_be_bytes());
            dst.extend_from_slice(data);
        }
        let mut dst = b"eidetica.service.v1/Reads/Read\0".to_vec();
        dst.extend_from_slice(&REVISION.to_be_bytes());
        field(&mut dst, &challenge.producer);
        field(&mut dst, &challenge.nonce);
        field(&mut dst, self.database.to_string().as_bytes());
        field(&mut dst, self.key.to_prefixed_string().as_bytes());
        let hint = self.identity.hint();
        if hint.is_global {
            dst.push(3);
        } else if let Some(name) = &hint.name {
            dst.push(2);
            field(&mut dst, name.as_bytes());
        } else {
            dst.push(1); // The only possible key hint is the key above.
        }
        let path = match &self.identity {
            SigKey::Direct { .. } => &[][..],
            SigKey::Delegation { path, .. } => &path[..],
        };
        dst.extend_from_slice(&(path.len() as u32).to_be_bytes());
        for step in path {
            field(&mut dst, step.tree.to_string().as_bytes());
        }
        let at = match &self.operation {
            Operation::Capture => {
                dst.push(1);
                None
            }
            Operation::State { at, store } => {
                dst.push(2);
                field(&mut dst, store.as_bytes());
                field(&mut dst, ComputedDoc::PROJECTION.as_bytes());
                Some(at)
            }
            Operation::History { at } => {
                dst.push(3);
                Some(at)
            }
        };
        if let Some(at) = at {
            dst.extend_from_slice(&(at.len() as u32).to_be_bytes());
            for tip in at {
                field(&mut dst, tip.to_string().as_bytes());
            }
        }
        dst
    }

    pub fn context(&self, at: &Cut) -> ReadContext {
        ReadContext::at(self.database.clone(), at.clone(), ReadScope::Verified)
    }

    pub fn wire_context(&self, producer: [u8; 16], captured: &Cut) -> Context {
        let (at, format, store, projection) = match &self.operation {
            Operation::Capture => (captured, Format::Snapshot, "", ""),
            Operation::State { at, store } => {
                (at, Format::Doc, store.as_str(), ComputedDoc::PROJECTION)
            }
            Operation::History { at } => (at, Format::CarV1, "", ""),
        };
        Context {
            producer: producer.to_vec(),
            database: self.database.to_string(),
            at: Some(snapshot(at)),
            format: format.into(),
            store: store.to_owned(),
            projection: projection.to_owned(),
        }
    }
}
