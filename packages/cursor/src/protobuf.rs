use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{Map, Value};
pub(crate) const MAX_FRAME: usize = 64 * 1024 * 1024;
pub(crate) fn malformed() -> PluginError {
    PluginError::transport("Cursor returned malformed or truncated protobuf.")
}
#[derive(Clone, Default)]
pub(crate) struct Pb(pub Vec<u8>);
impl Pb {
    pub fn new() -> Self {
        Self::default()
    }
    fn uv(mut self, mut n: u64) -> Self {
        while n >= 128 {
            self.0.push((n as u8 & 127) | 128);
            n >>= 7;
        }
        self.0.push(n as u8);
        self
    }
    fn tag(self, n: u32, w: u8) -> Self {
        self.uv((u64::from(n) << 3) | u64::from(w))
    }
    pub fn num(self, n: u32, v: u64) -> Self {
        self.tag(n, 0).uv(v)
    }
    pub fn bytes(self, n: u32, bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        let mut out = self.tag(n, 2).uv(bytes.len() as u64);
        out.0.extend_from_slice(bytes);
        out
    }
    pub fn str(self, n: u32, text: &str) -> Self {
        self.bytes(n, text.as_bytes())
    }
    fn double(self, n: u32, v: f64) -> Self {
        let mut out = self.tag(n, 1);
        out.0.extend_from_slice(&v.to_le_bytes());
        out
    }
}
impl AsRef<[u8]> for Pb {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Field<'a> {
    pub id: u32,
    pub wire: u8,
    pub n: u64,
    pub data: &'a [u8],
}
fn uv(bytes: &[u8], at: &mut usize) -> PluginResult<u64> {
    let mut n = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*at).ok_or_else(malformed)?;
        *at += 1;
        if shift == 63 && b > 1 {
            return Err(malformed());
        }
        n |= u64::from(b & 127) << shift;
        if b < 128 {
            return Ok(n);
        }
    }
    Err(malformed())
}
pub(crate) fn fields(bytes: &[u8]) -> PluginResult<Vec<Field<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if out.len() >= 100_000 {
            return Err(malformed());
        }
        let key = uv(bytes, &mut at)?;
        let id = u32::try_from(key >> 3).map_err(|_| malformed())?;
        let wire = (key & 7) as u8;
        if id == 0 || id > 0x1fffffff {
            return Err(malformed());
        }
        let (mut n, mut data) = (0, &bytes[0..0]);
        if wire == 0 {
            n = uv(bytes, &mut at)?;
        } else {
            let len = match wire {
                1 => 8,
                2 => usize::try_from(uv(bytes, &mut at)?).map_err(|_| malformed())?,
                5 => 4,
                _ => return Err(malformed()),
            };
            let end = at
                .checked_add(len)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(malformed)?;
            data = &bytes[at..end];
            at = end;
        }
        out.push(Field { id, wire, n, data });
    }
    Ok(out)
}
pub(crate) fn field<'a, 'b>(fs: &'a [Field<'b>], id: u32) -> Option<&'a Field<'b>> {
    fs.iter().rev().find(|f| f.id == id)
}
pub(crate) fn num(fs: &[Field<'_>], id: u32) -> PluginResult<u64> {
    match field(fs, id) {
        Some(f) if f.wire == 0 => Ok(f.n),
        None => Ok(0),
        _ => Err(malformed()),
    }
}
pub(crate) fn string(fs: &[Field<'_>], id: u32) -> PluginResult<String> {
    match field(fs, id) {
        Some(f) if f.wire == 2 => std::str::from_utf8(f.data)
            .map(str::to_owned)
            .map_err(|_| malformed()),
        None => Ok(String::new()),
        _ => Err(malformed()),
    }
}
pub(crate) fn value(v: &Value) -> Pb {
    match v {
        Value::Null => Pb::new().num(1, 0),
        Value::Bool(b) => Pb::new().num(4, u64::from(*b)),
        Value::Number(n) => Pb::new().double(2, n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Pb::new().str(3, s),
        Value::Array(a) => {
            let mut list = Pb::new();
            for v in a {
                list = list.bytes(1, value(v));
            }
            Pb::new().bytes(6, list)
        }
        Value::Object(o) => {
            let mut map = Pb::new();
            for (k, v) in o {
                map = map.bytes(1, Pb::new().str(1, k).bytes(2, value(v)));
            }
            Pb::new().bytes(5, map)
        }
    }
}
fn decoded(bytes: &[u8], depth: usize, budget: &mut usize) -> PluginResult<Value> {
    if depth > 64 {
        return Err(malformed());
    }
    let fs = fields(bytes)?;
    *budget = budget.checked_sub(fs.len()).ok_or_else(malformed)?;
    let mut out = Value::Null;
    for f in fs {
        out = match f.id {
            1 if f.wire == 0 && f.n == 0 => Value::Null,
            2 if f.wire == 1 => {
                let n = f64::from_le_bytes(f.data.try_into().map_err(|_| malformed())?);
                if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0 {
                    if n >= 0.0 {
                        Value::from(n as u64)
                    } else {
                        Value::from(n as i64)
                    }
                } else {
                    serde_json::Number::from_f64(n)
                        .map(Value::Number)
                        .ok_or_else(malformed)?
                }
            }
            3 if f.wire == 2 => {
                Value::String(std::str::from_utf8(f.data).map_err(|_| malformed())?.into())
            }
            4 if f.wire == 0 => Value::Bool(f.n != 0),
            5 if f.wire == 2 => {
                let mut map = Map::new();
                for entry in fields(f.data)? {
                    if entry.id != 1 {
                        continue;
                    }
                    if entry.wire != 2 {
                        return Err(malformed());
                    }
                    let entry = fields(entry.data)?;
                    *budget = budget.checked_sub(entry.len() + 1).ok_or_else(malformed)?;
                    let key = string(&entry, 1)?;
                    let value = match field(&entry, 2) {
                        Some(v) if v.wire == 2 => decoded(v.data, depth + 1, budget)?,
                        None => Value::Null,
                        _ => return Err(malformed()),
                    };
                    map.insert(key, value);
                }
                Value::Object(map)
            }
            6 if f.wire == 2 => {
                let mut list = Vec::new();
                for item in fields(f.data)? {
                    *budget = budget.checked_sub(1).ok_or_else(malformed)?;
                    if item.id == 1 {
                        if item.wire != 2 {
                            return Err(malformed());
                        }
                        list.push(decoded(item.data, depth + 1, budget)?);
                    }
                }
                Value::Array(list)
            }
            1..=6 => return Err(malformed()),
            _ => out,
        };
    }
    Ok(out)
}
pub(crate) fn arguments(bytes: &[u8]) -> PluginResult<(Vec<Field<'_>>, Value)> {
    if bytes.len() > 1024 * 1024 {
        return Err(malformed());
    }
    let fs = fields(bytes)?;
    let mut out = Map::new();
    let mut budget = 100_000usize.checked_sub(fs.len()).ok_or_else(malformed)?;
    for f in &fs {
        if f.id == 2 {
            if f.wire != 2 {
                return Err(malformed());
            }
            let kv = fields(f.data)?;
            budget = budget.checked_sub(kv.len()).ok_or_else(malformed)?;
            let k = string(&kv, 1)?;
            let v = match field(&kv, 2) {
                Some(v) if v.wire == 2 => decoded(v.data, 0, &mut budget)?,
                None => Value::Null,
                _ => return Err(malformed()),
            };
            out.insert(k, v);
        } else if [1, 3, 4, 5, 9].contains(&f.id) {
            if f.wire != 2 || std::str::from_utf8(f.data).is_err() {
                return Err(malformed());
            }
        } else if f.id == 6 && f.wire != 2 || f.id == 7 && f.wire != 0 {
            return Err(malformed());
        }
    }
    Ok((fs, Value::Object(out)))
}
pub(crate) fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0];
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}
#[derive(Default)]
pub(crate) struct Frames {
    bytes: Vec<u8>,
}
impl Frames {
    pub fn push(&mut self, bytes: &[u8]) -> PluginResult<Vec<(bool, Vec<u8>)>> {
        self.bytes.extend_from_slice(bytes);
        let mut at = 0;
        let mut out = Vec::new();
        while self.bytes.len() - at >= 5 {
            let flags = self.bytes[at];
            if flags & !3 != 0 {
                return Err(malformed());
            }
            let len = u32::from_be_bytes(
                self.bytes[at + 1..at + 5]
                    .try_into()
                    .map_err(|_| malformed())?,
            ) as usize;
            if len > MAX_FRAME {
                return Err(malformed());
            }
            if self.bytes.len() - at < 5 + len {
                break;
            }
            let mut data = self.bytes[at + 5..at + 5 + len].to_vec();
            if flags & 1 != 0 {
                use std::io::Read;
                let mut bytes = Vec::new();
                flate2::read::GzDecoder::new(data.as_slice())
                    .take((MAX_FRAME + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|_| malformed())?;
                if bytes.len() > MAX_FRAME {
                    return Err(malformed());
                }
                data = bytes;
            }
            out.push((flags & 2 != 0, data));
            at += 5 + len;
        }
        if at > 0 {
            self.bytes.drain(..at);
        }
        if self.bytes.len() > MAX_FRAME + 5 {
            return Err(malformed());
        }
        Ok(out)
    }
    pub fn finish(&self) -> PluginResult<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(malformed())
        }
    }
}

#[cfg(test)]
#[path = "../test/protobuf.test.rs"]
mod tests;
