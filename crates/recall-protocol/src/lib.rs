//! Bounded, incremental RESP2 request decoding and response encoding.
//! No sockets, clocks, engine state, or asynchronous runtime dependencies.

use bytes::{Buf, Bytes, BytesMut};
use std::fmt;
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_bulk_bytes: usize,
    pub max_arguments: usize,
    pub max_header_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 1024 * 1024,
            max_bulk_bytes: 1024 * 1024 - 64,
            max_arguments: 1024,
            max_header_bytes: 24,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.max_frame_bytes < 16
            || self.max_bulk_bytes > self.max_frame_bytes
            || self.max_arguments == 0
            || self.max_header_bytes < 4
            || self.max_header_bytes > self.max_frame_bytes
        {
            return Err(ProtocolError("invalid protocol limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError(pub &'static str);

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug)]
pub struct Request {
    pub arguments: Vec<Bytes>,
    pub wire_bytes: usize,
}

/// Input must not be externally consumed while a partial frame is being decoded.
/// After an error the connection must close; the decoder is not resynchronizable.
pub struct Decoder {
    limits: Limits,
    cursor: usize,
    scan: usize,
    argument_count: Option<usize>,
    bulk_length: Option<usize>,
    spans: Vec<Range<usize>>,
}

impl Decoder {
    pub fn new(limits: Limits) -> Result<Self, ProtocolError> {
        limits.validate()?;
        Ok(Self {
            limits,
            cursor: 0,
            scan: 0,
            argument_count: None,
            bulk_length: None,
            spans: Vec::new(),
        })
    }

    pub fn decode(&mut self, input: &mut BytesMut) -> Result<Option<Request>, ProtocolError> {
        loop {
            if self.argument_count.is_none() {
                let Some(end) = self.header_end(input)? else {
                    return Ok(None);
                };
                let count = self.length_header(input, end, b'*')?;
                if count == 0 || count > self.limits.max_arguments {
                    return Err(ProtocolError("invalid or excessive argument count"));
                }
                self.argument_count = Some(count);
                self.advance_header(end);
            }

            if self.bulk_length.is_none() {
                let Some(end) = self.header_end(input)? else {
                    return Ok(None);
                };
                let length = self.length_header(input, end, b'$')?;
                if length > self.limits.max_bulk_bytes {
                    return Err(ProtocolError("bulk argument exceeds configured limit"));
                }
                self.advance_header(end);
                self.bulk_length = Some(length);
            }

            let length = self.bulk_length.expect("bulk header has been decoded");
            let end = self
                .cursor
                .checked_add(length)
                .and_then(|end| end.checked_add(2))
                .ok_or(ProtocolError("frame length overflow"))?;
            if end > self.limits.max_frame_bytes {
                return Err(ProtocolError("request exceeds configured limit"));
            }
            if input.len() < end {
                return Ok(None);
            }
            if &input[end - 2..end] != b"\r\n" {
                return Err(ProtocolError("bulk argument is not CRLF terminated"));
            }
            self.spans.push(self.cursor..end - 2);
            self.cursor = end;
            self.scan = end;
            self.bulk_length = None;

            if Some(self.spans.len()) == self.argument_count {
                // Separate allocations deliberately avoid retaining an entire
                // pipeline slab for a tiny stored key or a long-lived value.
                let arguments = self
                    .spans
                    .iter()
                    .map(|span| Bytes::copy_from_slice(&input[span.clone()]))
                    .collect();
                let wire_bytes = self.cursor;
                input.advance(wire_bytes);
                self.cursor = 0;
                self.scan = 0;
                self.argument_count = None;
                self.spans.clear();
                return Ok(Some(Request {
                    arguments,
                    wire_bytes,
                }));
            }
        }
    }

    fn header_end(&mut self, input: &[u8]) -> Result<Option<usize>, ProtocolError> {
        while self.scan + 1 < input.len() {
            if self.scan - self.cursor + 2 > self.limits.max_header_bytes
                || self.scan + 2 > self.limits.max_frame_bytes
            {
                return Err(ProtocolError("protocol header exceeds configured limit"));
            }
            if input[self.scan] == b'\r' && input[self.scan + 1] == b'\n' {
                return Ok(Some(self.scan));
            }
            if input[self.scan] == b'\n' {
                return Err(ProtocolError("invalid protocol line ending"));
            }
            self.scan += 1;
        }
        if input.len().saturating_sub(self.cursor) >= self.limits.max_header_bytes
            || input.len() >= self.limits.max_frame_bytes
        {
            return Err(ProtocolError("unterminated or excessive protocol header"));
        }
        Ok(None)
    }

    fn length_header(
        &self,
        input: &[u8],
        end: usize,
        prefix: u8,
    ) -> Result<usize, ProtocolError> {
        if end <= self.cursor + 1 || input[self.cursor] != prefix {
            return Err(ProtocolError("expected a nonnegative RESP2 length"));
        }
        let mut length = 0_usize;
        for &digit in &input[self.cursor + 1..end] {
            if !digit.is_ascii_digit() {
                return Err(ProtocolError("invalid RESP2 length"));
            }
            length = length
                .checked_mul(10)
                .and_then(|value| value.checked_add(usize::from(digit - b'0')))
                .ok_or(ProtocolError("RESP2 length overflow"))?;
        }
        Ok(length)
    }

    fn advance_header(&mut self, end: usize) {
        self.cursor = end + 2;
        self.scan = self.cursor;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Simple(Bytes),
    Error(Bytes),
    Integer(i64),
    Bulk(Option<Bytes>),
    Array(Vec<Reply>),
}

impl Reply {
    pub fn ok() -> Self {
        Self::Simple(Bytes::from_static(b"OK"))
    }

    pub fn error(message: impl Into<Bytes>) -> Self {
        Self::Error(message.into())
    }

    pub fn bulk(value: impl Into<Bytes>) -> Self {
        Self::Bulk(Some(value.into()))
    }

    pub fn encoded_len(&self) -> Option<usize> {
        match self {
            Self::Simple(value) | Self::Error(value) => value.len().checked_add(3),
            Self::Integer(value) => decimal_digits(value.unsigned_abs())
                .checked_add(3 + usize::from(*value < 0)),
            Self::Bulk(None) => Some(5),
            Self::Bulk(Some(value)) => decimal_digits(value.len() as u64)
                .checked_add(5)
                .and_then(|header| header.checked_add(value.len())),
            Self::Array(values) => values.iter().try_fold(
                decimal_digits(values.len() as u64).checked_add(3)?,
                |total, value| total.checked_add(value.encoded_len()?),
            ),
        }
    }

    /// Call encoded_len and enforce the output budget before encoding.
    /// Simple/error strings are trusted server messages, never raw client input.
    pub fn encode(&self, output: &mut Vec<u8>) {
        match self {
            Self::Simple(value) | Self::Error(value) => {
                output.push(if matches!(self, Self::Simple(_)) {
                    b'+'
                } else {
                    b'-'
                });
                output.extend_from_slice(value);
                output.extend_from_slice(b"\r\n");
            }
            Self::Integer(value) => {
                output.push(b':');
                output.extend_from_slice(value.to_string().as_bytes());
                output.extend_from_slice(b"\r\n");
            }
            Self::Bulk(None) => output.extend_from_slice(b"$-1\r\n"),
            Self::Bulk(Some(value)) => {
                output.push(b'$');
                output.extend_from_slice(value.len().to_string().as_bytes());
                output.extend_from_slice(b"\r\n");
                output.extend_from_slice(value);
                output.extend_from_slice(b"\r\n");
            }
            Self::Array(values) => {
                output.push(b'*');
                output.extend_from_slice(values.len().to_string().as_bytes());
                output.extend_from_slice(b"\r\n");
                for value in values {
                    value.encode(output);
                }
            }
        }
    }
}

fn decimal_digits(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> Decoder {
        Decoder::new(Limits::default()).unwrap()
    }

    #[test]
    fn every_possible_fragment_boundary() {
        let wire = b"*3\r\n$3\r\nSET\r\n$0\r\n\r\n$5\r\na\0\r\nb\r\n";
        for split in 0..wire.len() {
            let mut decoder = decoder();
            let mut input = BytesMut::from(&wire[..split]);
            assert!(decoder.decode(&mut input).unwrap().is_none());
            input.extend_from_slice(&wire[split..]);
            let request = decoder.decode(&mut input).unwrap().unwrap();
            assert_eq!(request.arguments[0], &b"SET"[..]);
            assert_eq!(request.arguments[1], &b""[..]);
            assert_eq!(request.arguments[2], &b"a\0\r\nb"[..]);
            assert_eq!(request.wire_bytes, wire.len());
            assert!(input.is_empty());
        }
    }

    #[test]
    fn one_byte_reads_do_not_restart_parsing() {
        let wire = b"*2\r\n$4\r\nECHO\r\n$6\r\nrecall\r\n";
        let mut decoder = decoder();
        let mut input = BytesMut::new();
        for (index, byte) in wire.iter().enumerate() {
            input.extend_from_slice(&[*byte]);
            let result = decoder.decode(&mut input).unwrap();
            assert_eq!(result.is_some(), index + 1 == wire.len());
        }
    }

    #[test]
    fn consumes_one_pipelined_request_at_a_time() {
        let frame = b"*1\r\n$4\r\nPING\r\n";
        let mut input = BytesMut::new();
        input.extend_from_slice(frame);
        input.extend_from_slice(frame);
        let mut decoder = decoder();
        assert!(decoder.decode(&mut input).unwrap().is_some());
        assert_eq!(input.len(), frame.len());
        assert!(decoder.decode(&mut input).unwrap().is_some());
        assert!(input.is_empty());
    }

    #[test]
    fn rejects_invalid_lengths_before_payload_arrives() {
        for wire in [
            &b"*0\r\n"[..],
            &b"*-1\r\n"[..],
            &b"*999999\r\n"[..],
            &b"*1\r\n$-1\r\n"[..],
            &b"*1\r\n$9999999999\r\n"[..],
            &b"*1\r\n:1\r\n"[..],
            &b"*1\r\n$1\r\naXX"[..],
        ] {
            assert!(decoder().decode(&mut BytesMut::from(wire)).is_err());
        }
    }

    #[test]
    fn enforces_aggregate_frame_limit() {
        let limits = Limits {
            max_frame_bytes: 24,
            max_bulk_bytes: 20,
            ..Limits::default()
        };
        let mut decoder = Decoder::new(limits).unwrap();
        let mut input = BytesMut::from(&b"*2\r\n$5\r\n12345\r\n$5\r\n"[..]);
        assert!(decoder.decode(&mut input).is_err());
    }

    #[test]
    fn encoded_lengths_match_the_wire() {
        let replies = [
            Reply::ok(),
            Reply::error("ERR example"),
            Reply::Integer(i64::MIN),
            Reply::Integer(i64::MAX),
            Reply::Integer(0),
            Reply::Bulk(None),
            Reply::bulk(Bytes::from_static(b"a\0\r\nb")),
            Reply::Array(vec![Reply::Integer(-1), Reply::Bulk(None)]),
            Reply::Array(vec![]),
        ];
        for reply in replies {
            let mut encoded = Vec::new();
            reply.encode(&mut encoded);
            assert_eq!(reply.encoded_len(), Some(encoded.len()));
        }
    }
}

