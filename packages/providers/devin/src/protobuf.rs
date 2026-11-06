use nora_provider_plugin_sdk::{PluginError, PluginResult};
use std::io::{Read, Write};
pub(crate) const MAX_FRAME: usize = 64 * 1024 * 1024;
fn invalid() -> PluginError {
    PluginError::transport("Devin returned malformed or truncated protobuf data.")
}
#[derive(Default, Clone)]
pub(crate) struct Pb(pub Vec<u8>);
impl Pb {
    pub fn new() -> Self {
        Self::default()
    }
    fn raw(&mut self, mut n: u64) {
        while n >= 128 {
            self.0.push(n as u8 | 128);
            n >>= 7;
        }
        self.0.push(n as u8);
    }
    pub fn int(mut self, key: u32, n: u64) -> Self {
        self.raw((key as u64) << 3);
        self.raw(n);
        self
    }
    pub fn bytes(mut self, key: u32, data: impl AsRef<[u8]>) -> Self {
        let data = data.as_ref();
        self.raw((key as u64) << 3 | 2);
        self.raw(data.len() as u64);
        self.0.extend_from_slice(data);
        self
    }
    pub fn text(self, key: u32, value: &str) -> Self {
        self.bytes(key, value.as_bytes())
    }
    pub fn double(mut self, key: u32, n: f64) -> Self {
        self.raw((key as u64) << 3 | 1);
        self.0.extend_from_slice(&n.to_le_bytes());
        self
    }
}
impl AsRef<[u8]> for Pb {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Field<'a> {
    pub key: u32,
    pub wire: u8,
    pub number: u64,
    pub data: &'a [u8],
}
impl Field<'_> {
    pub fn text(&self) -> PluginResult<&str> {
        if self.wire != 2 {
            return Err(invalid());
        }
        std::str::from_utf8(self.data).map_err(|_| invalid())
    }
}
fn varint(data: &[u8], at: &mut usize) -> PluginResult<u64> {
    let mut out = 0u64;
    for i in 0..10 {
        let byte = *data.get(*at).ok_or_else(invalid)?;
        *at += 1;
        if i == 9 && byte > 1 {
            return Err(invalid());
        }
        out |= ((byte & 127) as u64) << (i * 7);
        if byte < 128 {
            return Ok(out);
        }
    }
    Err(invalid())
}
pub(crate) fn fields(data: &[u8]) -> PluginResult<Vec<Field<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        if out.len() >= 65536 {
            return Err(invalid());
        }
        let tag = varint(data, &mut at)?;
        let key = u32::try_from(tag >> 3).map_err(|_| invalid())?;
        let wire = (tag & 7) as u8;
        if key == 0 || key > 0x1fffffff {
            return Err(invalid());
        }
        let mut field = Field {
            key,
            wire,
            number: 0,
            data: &[],
        };
        if wire == 0 {
            field.number = varint(data, &mut at)?;
        } else {
            let length = match wire {
                1 => 8,
                2 => usize::try_from(varint(data, &mut at)?).map_err(|_| invalid())?,
                5 => 4,
                _ => return Err(invalid()),
            };
            let end = at
                .checked_add(length)
                .filter(|end| *end <= data.len())
                .ok_or_else(invalid)?;
            field.data = &data[at..end];
            at = end;
        }
        out.push(field);
    }
    Ok(out)
}
pub(crate) fn frame(data: &[u8]) -> PluginResult<Vec<u8>> {
    if data.len() > MAX_FRAME {
        return Err(PluginError::invalid("The Devin request exceeds 64 MiB."));
    }
    let (zip, body) = if data.len() > 8 * 1024 {
        let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        zip.write_all(data).map_err(|_| invalid())?;
        (true, zip.finish().map_err(|_| invalid())?)
    } else {
        (false, data.to_vec())
    };
    let mut out = vec![u8::from(zip)];
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}
pub(crate) fn inflate(data: &[u8]) -> PluginResult<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(MAX_FRAME as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| invalid())?;
    if out.len() > MAX_FRAME {
        return Err(invalid());
    }
    Ok(out)
}
#[cfg(test)]
#[path = "../test/protobuf.test.rs"]
mod tests;
