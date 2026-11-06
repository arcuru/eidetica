//! Streaming CBOR encoding and resource-bounded decoding of wire messages.
//!
//! Encoding and decoding use cbor4ii's noncanonical Serde codec directly. This is
//! a wire format only: stored Entry encoding, IDs and signatures do not pass
//! through it.
//!
//! [`Bounded`] wraps the native deserializer so the accounting reaches every
//! nested value. It charges each value before its seed runs, hides untrusted
//! collection size hints, and scopes byte allowances on [`WireReader`] around
//! protocol identifiers and discarded values. Those allowances count bytes
//! consumed, never bytes prefetched.

use std::{
    cell::{Cell, RefCell},
    fmt,
    io::{self, BufRead, Write},
};

use cbor4ii::core::dec::{Read as CborRead, Reference};
use serde::{
    Serialize,
    de::{self, DeserializeOwned, DeserializeSeed, Deserializer, Visitor},
};

use super::{ResourceLimit, TransferError, TransferPolicy};

/// Native I/O unit. Internal: unrelated to any message or Entry limit.
pub(crate) const CHUNK_BYTES: usize = 16 * 1024;

/// Encoded messages this small are moved through one buffer without a worker.
pub(crate) const INLINE_MESSAGE_BYTES: usize = 64 * 1024;

/// Name a classifying `Deserialize` passes to `deserialize_newtype_struct`. It
/// tells [`Bounded`] that the value is an enum discriminant or a one-key object,
/// so a string-form name gets the identifier allowance. Formats that do not know
/// the name call `visit_newtype_struct`, where the visitor continues normally.
pub(crate) const CLASSIFIER_NAME: &str = "$eidetica::message_io::discriminant";

/// A `Write` that fails past a fixed size, so sizing stops early.
pub(super) struct CountingWriter<'a> {
    pub(super) written: u64,
    pub(super) limit: u64,
    pub(super) cancelled: &'a dyn Fn() -> bool,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            // Not `Interrupted`, which `write_all` retries forever.
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        self.written = self.written.saturating_add(input.len() as u64);
        if self.written > self.limit {
            return Err(io::Error::other("message exceeds its limit"));
        }
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encode into a bounded buffer. `None` means the message is not inline-sized.
pub(crate) fn encode_inline<T: Serialize>(value: &T) -> Option<Vec<u8>> {
    struct Limited(Vec<u8>);
    impl Write for Limited {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if self.0.len() + input.len() > INLINE_MESSAGE_BYTES {
                return Err(io::ErrorKind::WriteZero.into());
            }
            self.0.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut buffer = Limited(Vec::new());
    // A single large item fails without filling the buffer, so this cannot tell
    // overflow from an encoder failure. Either way the sizing pass of the
    // streaming path decides, and reports a real failure.
    cbor4ii::serde::to_writer(&mut buffer, value)
        .ok()
        .map(|()| buffer.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Identifier,
    Ignored,
}

/// Decode accounting shared by the deserializer wrappers and the reader.
struct DecodeState {
    nodes_left: Cell<u64>,
    depth_left: Cell<usize>,
    max_depth: usize,
    identifier_bytes: usize,
    ignored_bytes: usize,
    /// Active byte allowance: its kind and the bytes left.
    scope: Cell<Option<(ScopeKind, usize)>>,
    /// Bytes of the discarded value being skipped. Fixed capacity: `Vec`'s
    /// geometric growth would otherwise overshoot the allowance.
    capture: RefCell<Vec<u8>>,
    /// The limit that stopped this decode, if one did.
    refused: Cell<Option<ResourceLimit>>,
}

impl DecodeState {
    fn new(policy: &TransferPolicy) -> Self {
        Self {
            nodes_left: Cell::new(policy.max_nodes),
            depth_left: Cell::new(policy.max_depth),
            max_depth: policy.max_depth,
            identifier_bytes: policy.max_identifier_bytes,
            ignored_bytes: policy.max_ignored_bytes,
            scope: Cell::new(None),
            capture: RefCell::new(Vec::new()),
            refused: Cell::new(None),
        }
    }

    fn refuse(&self, limit: ResourceLimit) {
        if self.refused.get().is_none() {
            self.refused.set(Some(limit));
        }
    }

    fn charge<E: de::Error>(&self) -> Result<(), E> {
        match self.nodes_left.get().checked_sub(1) {
            Some(left) => {
                self.nodes_left.set(left);
                Ok(())
            }
            None => {
                self.refuse(ResourceLimit::Nodes);
                Err(E::custom("decoded value limit"))
            }
        }
    }

    fn begin_identifier(&self) {
        self.scope
            .set(Some((ScopeKind::Identifier, self.identifier_bytes)));
    }

    fn end_identifier(&self) {
        if matches!(self.scope.get(), Some((ScopeKind::Identifier, _))) {
            self.scope.set(None);
        }
    }

    fn begin_ignored(&self) {
        let mut capture = self.capture.borrow_mut();
        capture.clear();
        capture.reserve_exact(self.ignored_bytes);
        self.scope
            .set(Some((ScopeKind::Ignored, self.ignored_bytes)));
    }

    /// End a discarded value, validating what the native skip consumed. That
    /// skip does not check UTF-8; an unbounded validating skip would allocate
    /// the whole scalar. The allowance makes the two together safe.
    fn end_ignored<E: de::Error>(&self) -> Result<(), E> {
        self.scope.set(None);
        let capture = self.capture.borrow();
        let mut remaining = capture.as_slice();
        let parsed: Result<de::IgnoredAny, _> =
            ciborium::de::from_reader_with_recursion_limit(&mut remaining, self.max_depth);
        if parsed.is_err() || !remaining.is_empty() {
            return Err(E::custom("malformed discarded value"));
        }
        Ok(())
    }
}

/// Native reader over a byte source, enforcing the active allowance.
struct WireReader<'s, S> {
    source: S,
    state: &'s DecodeState,
}

impl<'de, S: BufRead> CborRead<'de> for WireReader<'_, S> {
    type Error = io::Error;

    fn fill<'short>(&'short mut self, _want: usize) -> io::Result<Reference<'de, 'short>> {
        let allowance = self.state.scope.get();
        if let Some((kind, 0)) = allowance {
            self.state.refuse(match kind {
                ScopeKind::Identifier => ResourceLimit::IdentifierBytes,
                ScopeKind::Ignored => ResourceLimit::IgnoredBytes,
            });
            return Err(io::Error::other("wire byte allowance exhausted"));
        }
        let available = self.source.fill_buf()?;
        let exposed = match allowance {
            Some((_, left)) => available.len().min(left),
            None => available.len(),
        };
        Ok(Reference::Short(&available[..exposed]))
    }

    fn advance(&mut self, consumed: usize) {
        if consumed == 0 {
            return;
        }
        if let Some((kind, left)) = self.state.scope.get() {
            if kind == ScopeKind::Ignored {
                // Capturing here, not at the source, because the source may
                // already hold bytes of whatever follows this value.
                let buffered = self
                    .source
                    .fill_buf()
                    .expect("advance follows a successful fill");
                self.state
                    .capture
                    .borrow_mut()
                    .extend_from_slice(&buffered[..consumed]);
            }
            self.state.scope.set(Some((kind, left - consumed)));
        }
        self.source.consume(consumed);
    }

    fn step_in(&mut self) -> bool {
        match self.state.depth_left.get().checked_sub(1) {
            Some(left) => {
                self.state.depth_left.set(left);
                true
            }
            None => {
                self.state.refuse(ResourceLimit::Depth);
                false
            }
        }
    }

    fn step_out(&mut self) {
        self.state.depth_left.set(self.state.depth_left.get() + 1);
    }
}

/// Decode one complete value from `source` under `policy`.
///
/// `source` must end where the message ends: bytes left after the value are an
/// error, and nothing is returned until the whole message has been accepted.
pub(crate) fn decode<T, S>(source: S, policy: &TransferPolicy) -> Result<T, TransferError>
where
    T: DeserializeOwned,
    S: BufRead,
{
    let state = DecodeState::new(policy);
    let mut deserializer = cbor4ii::serde::Deserializer::new(WireReader {
        source,
        state: &state,
    });
    let decoded = T::deserialize(Bounded {
        inner: &mut deserializer,
        state: &state,
    });
    if let Some(limit) = state.refused.get() {
        return Err(TransferError::resource(limit, "while decoding"));
    }
    let value = decoded.map_err(|error| TransferError::Malformed(error.to_string()))?;
    let mut reader = deserializer.into_inner();
    if !reader.source.fill_buf()?.is_empty() {
        return Err(TransferError::Malformed(
            "bytes follow the encoded value".to_string(),
        ));
    }
    Ok(value)
}

/// Decodes a discriminant as an identifier, so it gets the identifier allowance
/// on every format rather than becoming an unbounded `String`.
pub(crate) struct DiscriminantSeed;

impl<'de> DeserializeSeed<'de> for DiscriminantSeed {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        struct Name;
        impl Visitor<'_> for Name {
            type Value = String;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an enum discriminant")
            }
            fn visit_str<E: de::Error>(self, name: &str) -> Result<String, E> {
                Ok(name.to_owned())
            }
        }
        deserializer.deserialize_identifier(Name)
    }
}

/// [`Deserializer`] applying [`DecodeState`] to everything beneath it.
struct Bounded<'s, D> {
    inner: D,
    state: &'s DecodeState,
}

macro_rules! bounded_methods {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
            self.inner.$method(BoundedVisitor::new(visitor, self.state))
        }
    )*};
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Bounded<'_, D> {
    type Error = D::Error;

    bounded_methods!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
    );

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.inner
            .deserialize_unit_struct(name, BoundedVisitor::new(visitor, self.state))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        if name == CLASSIFIER_NAME {
            self.state.begin_identifier();
            let mut wrapped = BoundedVisitor::new(visitor, self.state);
            wrapped.ends_identifier = true;
            let result = self.inner.deserialize_any(wrapped);
            self.state.end_identifier();
            return result;
        }
        self.inner
            .deserialize_newtype_struct(name, BoundedVisitor::new(visitor, self.state))
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.inner
            .deserialize_tuple(len, BoundedVisitor::new(visitor, self.state))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.inner
            .deserialize_tuple_struct(name, len, BoundedVisitor::new(visitor, self.state))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.inner
            .deserialize_struct(name, fields, BoundedVisitor::new(visitor, self.state))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.inner
            .deserialize_enum(name, variants, BoundedVisitor::new(visitor, self.state))
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.state.begin_identifier();
        let result = self
            .inner
            .deserialize_identifier(BoundedVisitor::new(visitor, self.state));
        self.state.end_identifier();
        result
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.state.begin_ignored();
        let result = self.inner.deserialize_ignored_any(de::IgnoredAny);
        match result {
            Ok(_) => {
                self.state.end_ignored()?;
                visitor.visit_unit()
            }
            Err(error) => {
                self.state.scope.set(None);
                Err(error)
            }
        }
    }

    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

struct BoundedVisitor<'s, V> {
    inner: V,
    state: &'s DecodeState,
    ends_identifier: bool,
}

impl<'s, V> BoundedVisitor<'s, V> {
    fn new(inner: V, state: &'s DecodeState) -> Self {
        Self {
            inner,
            state,
            ends_identifier: false,
        }
    }

    /// A classified value turned out to be a container: its header is the
    /// discriminant's only part, and its content has its own accounting.
    fn enter_container(&self) {
        if self.ends_identifier {
            self.state.end_identifier();
        }
    }
}

macro_rules! forward_visits {
    ($($method:ident($ty:ty)),* $(,)?) => {$(
        fn $method<E: de::Error>(self, value: $ty) -> Result<V::Value, E> {
            self.inner.$method(value)
        }
    )*};
}

impl<'de, V: Visitor<'de>> Visitor<'de> for BoundedVisitor<'_, V> {
    type Value = V::Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.inner.expecting(f)
    }

    forward_visits!(
        visit_bool(bool),
        visit_i8(i8),
        visit_i16(i16),
        visit_i32(i32),
        visit_i64(i64),
        visit_i128(i128),
        visit_u8(u8),
        visit_u16(u16),
        visit_u32(u32),
        visit_u64(u64),
        visit_u128(u128),
        visit_f32(f32),
        visit_f64(f64),
        visit_char(char),
        visit_str(&str),
        visit_borrowed_str(&'de str),
        visit_string(String),
        visit_bytes(&[u8]),
        visit_borrowed_bytes(&'de [u8]),
        visit_byte_buf(Vec<u8>),
    );

    fn visit_none<E: de::Error>(self) -> Result<V::Value, E> {
        self.inner.visit_none()
    }

    fn visit_unit<E: de::Error>(self) -> Result<V::Value, E> {
        self.inner.visit_unit()
    }

    fn visit_some<D: Deserializer<'de>>(self, inner: D) -> Result<V::Value, D::Error> {
        self.inner.visit_some(Bounded {
            inner,
            state: self.state,
        })
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, inner: D) -> Result<V::Value, D::Error> {
        self.inner.visit_newtype_struct(Bounded {
            inner,
            state: self.state,
        })
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, inner: A) -> Result<V::Value, A::Error> {
        self.enter_container();
        self.inner.visit_seq(BoundedAccess {
            inner,
            state: self.state,
        })
    }

    fn visit_map<A: de::MapAccess<'de>>(self, inner: A) -> Result<V::Value, A::Error> {
        self.enter_container();
        self.inner.visit_map(BoundedAccess {
            inner,
            state: self.state,
        })
    }

    fn visit_enum<A: de::EnumAccess<'de>>(self, inner: A) -> Result<V::Value, A::Error> {
        self.enter_container();
        self.inner.visit_enum(BoundedAccess {
            inner,
            state: self.state,
        })
    }
}

/// Charges one value, then decodes it under [`Bounded`].
struct BoundedSeed<'s, T> {
    inner: T,
    state: &'s DecodeState,
}

impl<'de, T: DeserializeSeed<'de>> DeserializeSeed<'de> for BoundedSeed<'_, T> {
    type Value = T::Value;

    fn deserialize<D: Deserializer<'de>>(self, inner: D) -> Result<T::Value, D::Error> {
        self.state.charge()?;
        self.inner.deserialize(Bounded {
            inner,
            state: self.state,
        })
    }
}

/// Wraps native sequence, map, enum and variant accessors alike.
struct BoundedAccess<'s, A> {
    inner: A,
    state: &'s DecodeState,
}

impl<'s, A> BoundedAccess<'s, A> {
    fn seed<T>(&self, inner: T) -> BoundedSeed<'s, T> {
        BoundedSeed {
            inner,
            state: self.state,
        }
    }
}

impl<'de, A: de::SeqAccess<'de>> de::SeqAccess<'de> for BoundedAccess<'_, A> {
    type Error = A::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, A::Error> {
        let seed = self.seed(seed);
        self.inner.next_element_seed(seed)
    }

    // A declared length is untrusted and must not size an allocation.
    fn size_hint(&self) -> Option<usize> {
        None
    }
}

impl<'de, A: de::MapAccess<'de>> de::MapAccess<'de> for BoundedAccess<'_, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, A::Error> {
        let seed = self.seed(seed);
        self.inner.next_key_seed(seed)
    }

    fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, A::Error> {
        let seed = self.seed(seed);
        self.inner.next_value_seed(seed)
    }

    fn size_hint(&self) -> Option<usize> {
        None
    }
}

impl<'de, 's, A: de::EnumAccess<'de>> de::EnumAccess<'de> for BoundedAccess<'s, A> {
    type Error = A::Error;
    type Variant = BoundedAccess<'s, A::Variant>;

    fn variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<(T::Value, Self::Variant), A::Error> {
        let state = self.state;
        let seed = self.seed(seed);
        let (value, inner) = self.inner.variant_seed(seed)?;
        Ok((value, BoundedAccess { inner, state }))
    }
}

impl<'de, A: de::VariantAccess<'de>> de::VariantAccess<'de> for BoundedAccess<'_, A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        self.inner.unit_variant()
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, A::Error> {
        let seed = self.seed(seed);
        self.inner.newtype_variant_seed(seed)
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, A::Error> {
        self.inner
            .tuple_variant(len, BoundedVisitor::new(visitor, self.state))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, A::Error> {
        self.inner
            .struct_variant(fields, BoundedVisitor::new(visitor, self.state))
    }
}
