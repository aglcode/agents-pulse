//! Minimal protobuf wire-format reader.
//!
//! It walks a message's fields by number and hands back borrowed slices, so a
//! length-delimited field that holds content (a prompt, a tool call) is skipped
//! without being copied, decoded or kept. Callers pick out the few numeric fields
//! and model-name strings they need (docs/providers/antigravity.md).

/// The input isn't valid protobuf wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Malformed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value<'a> {
    Varint(u64),
    Fixed64,
    Len(&'a [u8]),
    Fixed32,
}

/// Iterator over `(field number, value)` pairs. Stops after the first error.
pub(crate) struct Fields<'a> {
    buf: &'a [u8],
    pos: usize,
    failed: bool,
}

pub(crate) fn fields(buf: &[u8]) -> Fields<'_> {
    Fields { buf, pos: 0, failed: false }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u32, Value<'a>), Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }
        let r = self.field();
        if r.is_err() {
            self.failed = true;
        }
        Some(r)
    }
}

impl<'a> Fields<'a> {
    fn field(&mut self) -> Result<(u32, Value<'a>), Malformed> {
        let key = self.varint()?;
        let number = u32::try_from(key >> 3).map_err(|_| Malformed)?;
        if number == 0 {
            return Err(Malformed);
        }
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.skip(8)?;
                Value::Fixed64
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| Malformed)?;
                let start = self.pos;
                self.skip(len)?;
                Value::Len(&self.buf[start..self.pos])
            }
            5 => {
                self.skip(4)?;
                Value::Fixed32
            }
            // Groups (3, 4) are deprecated and never used by these messages.
            _ => return Err(Malformed),
        };
        Ok((number, value))
    }

    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or(Malformed)?;
            self.pos += 1;
            out |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(out);
            }
        }
        Err(Malformed)
    }

    fn skip(&mut self, n: usize) -> Result<(), Malformed> {
        let end = self.pos.checked_add(n).ok_or(Malformed)?;
        if end > self.buf.len() {
            return Err(Malformed);
        }
        self.pos = end;
        Ok(())
    }
}

/// Test-only encoder for building synthetic messages.
#[cfg(any(test, feature = "testkit"))]
pub mod encode {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// Builder for one message.
    #[derive(Default, Clone)]
    pub struct Msg(pub Vec<u8>);

    impl Msg {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn varint(mut self, field: u32, v: u64) -> Self {
            varint(&mut self.0, u64::from(field) << 3);
            varint(&mut self.0, v);
            self
        }
        pub fn bytes(mut self, field: u32, b: &[u8]) -> Self {
            varint(&mut self.0, (u64::from(field) << 3) | 2);
            varint(&mut self.0, b.len() as u64);
            self.0.extend_from_slice(b);
            self
        }
        pub fn str(self, field: u32, s: &str) -> Self {
            self.bytes(field, s.as_bytes())
        }
        pub fn msg(self, field: u32, m: &Msg) -> Self {
            self.bytes(field, &m.0)
        }
        pub fn fixed64(mut self, field: u32, v: u64) -> Self {
            varint(&mut self.0, (u64::from(field) << 3) | 1);
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        pub fn fixed32(mut self, field: u32, v: u32) -> Self {
            varint(&mut self.0, (u64::from(field) << 3) | 5);
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::encode::Msg;
    use super::*;

    #[test]
    fn walks_every_wire_type_and_borrows_length_delimited_fields() {
        let inner = Msg::new().varint(1, 7);
        let m = Msg::new().varint(1, 300).fixed64(2, 9).str(3, "x").fixed32(4, 1).msg(5, &inner).varint(536_870_911, 1);
        let got: Vec<_> = fields(&m.0).collect::<Result<_, _>>().unwrap();
        assert_eq!(got[0], (1, Value::Varint(300)));
        assert_eq!(got[1], (2, Value::Fixed64));
        assert_eq!(got[2], (3, Value::Len(b"x")));
        assert_eq!(got[3], (4, Value::Fixed32));
        assert_eq!(got[4], (5, Value::Len(&inner.0[..])));
        assert_eq!(got[5], (536_870_911, Value::Varint(1)));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in [
            &[0x08][..],             // varint value missing
            &[0x12, 0x05, b'a'],     // length past the end
            &[0x00, 0x01],           // field number 0
            &[0x0b],                 // group start
            &[0x09, 1, 2, 3],        // short fixed64
            &[0xff; 11],             // varint too long
        ] {
            let r: Result<Vec<_>, _> = fields(bad).collect();
            assert_eq!(r, Err(Malformed), "{bad:?}");
        }
        assert_eq!(fields(&[]).count(), 0);
    }
}
