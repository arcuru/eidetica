//! Public row-codec boundary tests, independent of Table and backend behavior.

use eidetica::store::{RawBytes, RowCodec, SerdeJson};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct IntegerRow {
    nested: IntegerFields,
    arrays: Vec<IntegerFields>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct IntegerFields {
    signed64: [i64; 3],
    unsigned64: [u64; 3],
    signed128: [i128; 3],
    unsigned128: [u128; 3],
}

fn integer_fields() -> IntegerFields {
    IntegerFields {
        signed64: [i64::MIN, 0, i64::MAX],
        unsigned64: [0, 1 << 63, u64::MAX],
        signed128: [i128::MIN, 0, i128::MAX],
        unsigned128: [0, 1 << 127, u128::MAX],
    }
}

#[test]
fn test_serde_json_full_range_integers() {
    let row = IntegerRow {
        nested: integer_fields(),
        arrays: vec![integer_fields(), integer_fields()],
    };
    let bytes = SerdeJson::encode(&row).unwrap();
    // This also pins direct Serde encoding rather than a lossy Value/JCS path.
    assert_eq!(bytes, serde_json::to_vec(&row).unwrap());
    let decoded: IntegerRow = SerdeJson::decode(&bytes).unwrap();
    assert_eq!(decoded, row);
}

#[test]
fn test_raw_bytes_identity() {
    for row in [vec![], vec![0], vec![0, 0, 0], vec![0xff, 0x80, 0, 0xfe]] {
        let encoded = RawBytes::encode(&row).unwrap();
        assert_eq!(encoded, row);
        assert_eq!(RawBytes::decode(&encoded).unwrap(), row);
    }
}

// Deliberately no Serialize/Deserialize impls: the trait must not require them.
#[derive(Debug, PartialEq)]
struct CustomRow(u8);

struct CustomCodec;

impl RowCodec<CustomRow> for CustomCodec {
    fn encode(row: &CustomRow) -> eidetica::Result<Vec<u8>> {
        Ok(vec![row.0])
    }

    fn decode(bytes: &[u8]) -> eidetica::Result<CustomRow> {
        match bytes {
            [value] => Ok(CustomRow(*value)),
            _ => Err(eidetica::store::StoreError::DeserializationFailed {
                store: "custom-row".into(),
                reason: "expected exactly one byte".into(),
            }
            .into()),
        }
    }
}

#[test]
fn test_custom_non_serde_row_codec() {
    let row = CustomRow(0xff);
    let bytes = CustomCodec::encode(&row).unwrap();
    assert_eq!(bytes, vec![0xff]);
    assert_eq!(CustomCodec::decode(&bytes).unwrap(), row);
    assert!(CustomCodec::decode(&[]).is_err());
    assert!(CustomCodec::decode(&[1, 2]).is_err());
}

#[test]
fn test_serde_json_decode_errors() {
    for bytes in [b"".as_slice(), b"{", b"null", b"{}", b"\xff"] {
        assert!(<SerdeJson as RowCodec<IntegerRow>>::decode(bytes).is_err());
    }
    // Reject out-of-range numbers and trailing input, not just invalid syntax.
    assert!(<SerdeJson as RowCodec<u64>>::decode(b"18446744073709551616").is_err());
    assert!(<SerdeJson as RowCodec<i64>>::decode(b"-9223372036854775809").is_err());
    assert!(
        <SerdeJson as RowCodec<u128>>::decode(b"340282366920938463463374607431768211456").is_err()
    );
    assert!(
        <SerdeJson as RowCodec<i128>>::decode(b"-170141183460469231731687303715884105729").is_err()
    );
    assert!(<SerdeJson as RowCodec<u8>>::decode(b"1 2").is_err());
}

fn assert_dag_cbor_bytes<T: std::fmt::Debug + PartialEq, C: RowCodec<T>>(row: T) {
    let encoded_row = C::encode(&row).unwrap();
    let envelope = serde_ipld_dagcbor::to_vec(serde_bytes::Bytes::new(&encoded_row)).unwrap();
    // Major type 2 is a CBOR byte string, not an array or a UTF-8 text string.
    assert_eq!(envelope[0] >> 5, 2);
    let ipld: ipld_core::ipld::Ipld = serde_ipld_dagcbor::from_slice(&envelope).unwrap();
    assert_eq!(ipld, ipld_core::ipld::Ipld::Bytes(encoded_row.clone()));
    let decoded: serde_bytes::ByteBuf = serde_ipld_dagcbor::from_slice(&envelope).unwrap();
    assert_eq!(decoded.as_ref(), encoded_row.as_slice());
    assert_eq!(C::decode(decoded.as_ref()).unwrap(), row);
}

#[test]
fn test_row_codec_dag_cbor_byte_string_roundtrip() {
    assert_dag_cbor_bytes::<_, SerdeJson>(IntegerRow {
        nested: integer_fields(),
        arrays: vec![integer_fields()],
    });
    for row in [vec![], vec![0, 0, 0], vec![0xff, 0x80, 0]] {
        assert_dag_cbor_bytes::<_, RawBytes>(row);
    }
    assert_dag_cbor_bytes::<_, CustomCodec>(CustomRow(0xff));
}
