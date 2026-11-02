//! Capped compact JSON encoding shared by storage paging and service framing.

use std::io::Write;

use serde::Serialize;

/// A serializer must stop before allocating an out-of-limit encoded value.
/// Counting uses the same writer as encoding, with a non-allocating sink.
struct CappedJsonWriter<W> {
    inner: W,
    len: usize,
    limit: usize,
    exceeded: bool,
}

impl<W: Write> Write for CappedJsonWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(len) = self
            .len
            .checked_add(bytes.len())
            .filter(|len| *len <= self.limit)
        else {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "encoded JSON exceeds its byte limit",
            ));
        };
        self.inner.write_all(bytes)?;
        self.len = len;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(crate) fn serialize_capped<T: Serialize, W: Write>(
    value: &T,
    inner: W,
    limit: usize,
) -> serde_json::Result<Option<(W, usize)>> {
    let mut writer = CappedJsonWriter {
        inner,
        len: 0,
        limit,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if writer.exceeded {
        return Ok(None);
    }
    result?;
    Ok(Some((writer.inner, writer.len)))
}

/// Exact compact JSON length, or `None` if encoding exceeds `limit`.
/// No encoded body is allocated, and an over-limit producer is stopped early.
pub(crate) fn json_encoded_len<T: Serialize>(
    value: &T,
    limit: usize,
) -> serde_json::Result<Option<usize>> {
    Ok(serialize_capped(value, std::io::sink(), limit)?.map(|(_, len)| len))
}

/// Encode at most `limit` bytes; `None` means the complete value did not fit.
#[cfg(any(test, all(unix, feature = "service")))]
pub(crate) fn encode_json<T: Serialize>(
    value: &T,
    limit: usize,
) -> serde_json::Result<Option<Vec<u8>>> {
    Ok(serialize_capped(value, Vec::new(), limit)?.map(|(bytes, _)| bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_json_counts_the_complete_encoding_at_the_boundary() {
        let value = serde_json::json!({"key": [255, 255, 0], "escaped": "\"\\\n"});
        let expected = serde_json::to_vec(&value).unwrap();
        let size = expected.len();
        for limit in [size - 1, size, size + 1] {
            let fits = limit >= size;
            assert_eq!(
                json_encoded_len(&value, limit).unwrap(),
                fits.then_some(size)
            );
            assert_eq!(
                encode_json(&value, limit).unwrap(),
                fits.then(|| expected.clone())
            );
        }
        assert_eq!(json_encoded_len(&Vec::<u8>::new(), 1).unwrap(), None);
        assert_eq!(json_encoded_len(&vec![255u8; 256], 1024).unwrap(), None);
        assert_eq!(
            json_encoded_len(&vec![255u8; 256], 1025).unwrap(),
            Some(1025)
        );
    }

    #[test]
    fn capped_json_stops_a_producer_without_collecting_it() {
        use serde::ser::SerializeSeq;
        use std::cell::Cell;

        struct ManyBytes(Cell<usize>);
        impl Serialize for ManyBytes {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(Some(usize::MAX))?;
                loop {
                    self.0.set(self.0.get() + 1);
                    seq.serialize_element(&255u8)?;
                }
            }
        }
        for counting in [true, false] {
            let producer = ManyBytes(Cell::new(0));
            if counting {
                assert_eq!(json_encoded_len(&producer, 1024).unwrap(), None);
            } else {
                assert_eq!(encode_json(&producer, 1024).unwrap(), None);
            }
            assert_eq!(producer.0.get(), 257, "producer must stop at the byte cap");
        }
    }

    #[test]
    fn capped_json_preserves_serialization_errors_and_checks_overflow() {
        struct BadValue;
        impl Serialize for BadValue {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("bad test value"))
            }
        }
        assert!(
            json_encoded_len(&BadValue, 1024)
                .unwrap_err()
                .to_string()
                .contains("bad test value")
        );
        assert!(encode_json(&BadValue, 1024).is_err());
        let mut writer = CappedJsonWriter {
            inner: Vec::new(),
            len: usize::MAX,
            limit: usize::MAX,
            exceeded: false,
        };
        assert!(writer.write(&[1]).is_err());
        assert!(writer.exceeded);
        assert!(writer.inner.is_empty());
    }
}
