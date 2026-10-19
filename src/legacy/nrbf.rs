//! Minimal reader for the .NET BinaryFormatter format (MS-NRBF).
//!
//! The Windows RDP-Manager stores protocol settings (port, resolution,
//! redirections …) as BinaryFormatter blobs in the column `BinVal`. For the import
//! it is enough to read the object graph and look up the simple fields of the
//! root object by name. Nothing is instantiated or executed.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Ref(i32),
}

#[derive(Debug, Clone)]
pub enum Obj {
    Class { name: String, members: Vec<(String, Value)> },
    Array(Vec<Value>),
    Str(String),
}

#[derive(Debug, Default)]
pub struct Graph {
    pub root: i32,
    pub objects: HashMap<i32, Obj>,
}

#[derive(Clone)]
enum MemberType {
    Primitive(u8),
    Record,
}

#[derive(Clone)]
struct ClassMeta {
    name: String,
    members: Vec<String>,
    types: Option<Vec<MemberType>>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    metas: HashMap<i32, ClassMeta>,
    graph: Graph,
    depth: u32,
    /// Remaining number of values the stream may produce; guards against inputs that
    /// declare billions of zero-byte elements (nulls) and would exhaust memory.
    budget: usize,
}

/// Settings blobs of the original are a few hundred values; real nesting is a handful of levels.
const MAX_DEPTH: u32 = 64;
const MAX_VALUES: usize = 1_000_000;

impl<'a> Reader<'a> {
    fn spend(&mut self, n: usize) -> Result<()> {
        self.budget = self.budget.checked_sub(n).ok_or_else(|| anyhow!("Stream declares too many values"))?;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| anyhow!("Overflow"))?;
        if end > self.data.len() {
            bail!("Unexpected end of data at offset {}", self.pos);
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn string(&mut self) -> Result<String> {
        let mut len: usize = 0;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            len |= ((b & 0x7F) as usize) << shift;
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn primitive(&mut self, t: u8) -> Result<Value> {
        let le = |b: &[u8]| -> [u8; 8] {
            let mut a = [0u8; 8];
            a[..b.len()].copy_from_slice(b);
            a
        };
        Ok(match t {
            1 => Value::Bool(self.u8()? != 0),
            2 => Value::UInt(self.u8()? as u64),
            3 => {
                // Char: one UTF-8 character
                let first = self.u8()?;
                let extra = match first {
                    0xC0..=0xDF => 1,
                    0xE0..=0xEF => 2,
                    0xF0..=0xF7 => 3,
                    _ => 0,
                };
                let mut bytes = vec![first];
                bytes.extend_from_slice(self.take(extra)?);
                Value::Str(String::from_utf8_lossy(&bytes).into_owned())
            }
            5 => Value::Str(self.string()?),
            6 => Value::Float(f64::from_le_bytes(self.take(8)?.try_into()?)),
            7 => Value::Int(i16::from_le_bytes(self.take(2)?.try_into()?) as i64),
            8 => Value::Int(self.i32()? as i64),
            9 | 12 | 13 => Value::Int(i64::from_le_bytes(self.take(8)?.try_into()?)),
            10 => Value::Int(self.u8()? as i8 as i64),
            11 => Value::Float(f32::from_le_bytes(self.take(4)?.try_into()?) as f64),
            14 => Value::UInt(u64::from_le_bytes(le(self.take(2)?))),
            15 => Value::UInt(u64::from_le_bytes(le(self.take(4)?))),
            16 => Value::UInt(u64::from_le_bytes(self.take(8)?.try_into()?)),
            17 => Value::Null,
            18 => Value::Str(self.string()?),
            _ => bail!("Unknown primitive type {t}"),
        })
    }

    fn class_info(&mut self) -> Result<(i32, String, Vec<String>)> {
        let id = self.i32()?;
        let name = self.string()?;
        let count = self.i32()?;
        if !(0..=10_000).contains(&count) {
            bail!("Implausible member count {count}");
        }
        let members = (0..count).map(|_| self.string()).collect::<Result<_>>()?;
        Ok((id, name, members))
    }

    fn member_types(&mut self, count: usize) -> Result<Vec<MemberType>> {
        let kinds: Vec<u8> = (0..count).map(|_| self.u8()).collect::<Result<_>>()?;
        let mut out = Vec::with_capacity(count);
        for k in kinds {
            out.push(match k {
                0 => MemberType::Primitive(self.u8()?),
                1 | 2 | 5 | 6 => MemberType::Record,
                3 => {
                    self.string()?;
                    MemberType::Record
                }
                4 => {
                    self.string()?;
                    self.i32()?;
                    MemberType::Record
                }
                7 => {
                    self.u8()?;
                    MemberType::Record
                }
                _ => bail!("Unknown BinaryType {k}"),
            });
        }
        Ok(out)
    }

    fn class_values(&mut self, id: i32, meta: &ClassMeta) -> Result<()> {
        self.spend(meta.members.len())?;
        let mut members = Vec::with_capacity(meta.members.len());
        let mut pending_nulls = 0usize;
        for (i, name) in meta.members.iter().enumerate() {
            let v = if pending_nulls > 0 {
                pending_nulls -= 1;
                Value::Null
            } else {
                match meta.types.as_ref().map(|t| t[i].clone()) {
                    Some(MemberType::Primitive(t)) => self.primitive(t)?,
                    _ => match self.record()? {
                        Rec::Value(v) => v,
                        Rec::Nulls(n) => {
                            pending_nulls = n.saturating_sub(1);
                            Value::Null
                        }
                        Rec::End => bail!("MessageEnd inside an object"),
                    },
                }
            };
            members.push((name.clone(), v));
        }
        self.graph.objects.insert(id, Obj::Class { name: meta.name.clone(), members });
        Ok(())
    }

    fn array_records(&mut self, id: i32, len: usize) -> Result<()> {
        self.spend(len)?;
        let mut items = Vec::with_capacity(len.min(4096));
        while items.len() < len {
            match self.record()? {
                Rec::Value(v) => items.push(v),
                Rec::Nulls(n) => items.extend(std::iter::repeat_n(Value::Null, n.min(len - items.len()))),
                Rec::End => bail!("MessageEnd inside an array"),
            }
        }
        self.graph.objects.insert(id, Obj::Array(items));
        Ok(())
    }

    fn record(&mut self) -> Result<Rec> {
        if self.depth >= MAX_DEPTH {
            bail!("Object graph nested too deeply");
        }
        self.depth += 1;
        let r = self.record_inner();
        self.depth -= 1;
        r
    }

    fn record_inner(&mut self) -> Result<Rec> {
        let kind = self.u8()?;
        Ok(match kind {
            0 => {
                self.graph.root = self.i32()?;
                self.take(12)?;
                return self.record();
            }
            1 => {
                let id = self.i32()?;
                let meta_id = self.i32()?;
                let meta = self.metas.get(&meta_id).cloned().ok_or_else(|| anyhow!("Unknown metadata {meta_id}"))?;
                self.class_values(id, &meta)?;
                Rec::Value(Value::Ref(id))
            }
            2..=5 => {
                let (id, name, members) = self.class_info()?;
                let types = if kind == 4 || kind == 5 { Some(self.member_types(members.len())?) } else { None };
                if kind == 3 || kind == 5 {
                    self.i32()?; // LibraryId
                }
                let meta = ClassMeta { name, members, types };
                self.metas.insert(id, meta.clone());
                self.class_values(id, &meta)?;
                Rec::Value(Value::Ref(id))
            }
            6 => {
                let id = self.i32()?;
                let s = self.string()?;
                self.graph.objects.insert(id, Obj::Str(s));
                Rec::Value(Value::Ref(id))
            }
            7 => {
                let id = self.i32()?;
                let array_type = self.u8()?;
                let rank = self.i32()?;
                if !(1..=32).contains(&rank) {
                    bail!("Implausible array rank {rank}");
                }
                let mut total: usize = 1;
                for _ in 0..rank {
                    total = total.saturating_mul(self.i32()?.max(0) as usize);
                }
                if matches!(array_type, 3..=5) {
                    self.take(4 * rank as usize)?;
                }
                let item_type = self.member_types(1)?;
                match item_type[0] {
                    MemberType::Primitive(t) => {
                        self.spend(total)?;
                        let items = (0..total).map(|_| self.primitive(t)).collect::<Result<_>>()?;
                        self.graph.objects.insert(id, Obj::Array(items));
                    }
                    MemberType::Record => self.array_records(id, total)?,
                }
                Rec::Value(Value::Ref(id))
            }
            8 => {
                let t = self.u8()?;
                Rec::Value(self.primitive(t)?)
            }
            9 => Rec::Value(Value::Ref(self.i32()?)),
            10 => Rec::Value(Value::Null),
            11 => Rec::End,
            12 => {
                self.i32()?;
                self.string()?;
                return self.record();
            }
            13 => Rec::Nulls(self.u8()? as usize),
            14 => Rec::Nulls(self.i32()?.max(0) as usize),
            15 => {
                let id = self.i32()?;
                let len = self.i32()?.max(0) as usize;
                let t = self.u8()?;
                if t == 2 {
                    // Do not expand byte arrays element by element
                    self.take(len)?;
                    self.graph.objects.insert(id, Obj::Array(Vec::new()));
                } else {
                    self.spend(len)?;
                    let items = (0..len).map(|_| self.primitive(t)).collect::<Result<_>>()?;
                    self.graph.objects.insert(id, Obj::Array(items));
                }
                Rec::Value(Value::Ref(id))
            }
            16 | 17 => {
                let id = self.i32()?;
                let len = self.i32()?.max(0) as usize;
                self.array_records(id, len)?;
                Rec::Value(Value::Ref(id))
            }
            _ => bail!("Unsupported record type {kind} at offset {}", self.pos - 1),
        })
    }
}

enum Rec {
    Value(Value),
    Nulls(usize),
    End,
}

pub fn parse(data: &[u8]) -> Result<Graph> {
    let mut r = Reader { data, pos: 0, metas: HashMap::new(), graph: Graph::default(), depth: 0, budget: MAX_VALUES };
    if data.first() != Some(&0) {
        bail!("Not a BinaryFormatter stream");
    }
    loop {
        match r.record()? {
            Rec::End => break,
            _ if r.pos >= data.len() => break,
            _ => {}
        }
    }
    Ok(r.graph)
}

/// `<Port>k__BackingField` → `Port`, `Base+<X>k__BackingField` → `X`.
fn clean_name(raw: &str) -> &str {
    let n = raw.rsplit('+').next().unwrap_or(raw);
    n.strip_prefix('<').and_then(|s| s.strip_suffix(">k__BackingField")).unwrap_or(n)
}

/// Convenient access to the fields of the root object.
pub struct Fields<'g> {
    graph: &'g Graph,
    #[allow(dead_code)]
    pub class_name: String,
    map: HashMap<String, &'g Value>,
}

impl Graph {
    pub fn root_fields(&self) -> Option<Fields<'_>> {
        match self.objects.get(&self.root)? {
            Obj::Class { name, members } => Some(Fields {
                graph: self,
                class_name: name.clone(),
                map: members.iter().map(|(k, v)| (clean_name(k).to_string(), v)).collect(),
            }),
            _ => None,
        }
    }

    fn resolve<'a>(&'a self, v: &'a Value) -> &'a Value {
        let mut cur = v;
        for _ in 0..8 {
            match cur {
                Value::Ref(id) => match self.objects.get(id) {
                    // Enums are stored as an object with exactly one field `value__`.
                    Some(Obj::Class { members, .. }) if members.len() == 1 && members[0].0 == "value__" => cur = &members[0].1,
                    _ => return cur,
                },
                _ => return cur,
            }
        }
        cur
    }
}

impl Fields<'_> {
    fn get(&self, names: &[&str]) -> Option<&Value> {
        names.iter().find_map(|n| self.map.get(*n)).map(|v| self.graph.resolve(v))
    }

    /// Fields of an object referenced by a field (e.g. a list class with extra properties).
    pub fn nested(&self, names: &[&str]) -> Option<Fields<'_>> {
        let Value::Ref(id) = self.get(names)? else { return None };
        match self.graph.objects.get(id)? {
            Obj::Class { name, members } => Some(Fields {
                graph: self.graph,
                class_name: name.clone(),
                map: members.iter().map(|(k, v)| (clean_name(k).to_string(), v)).collect(),
            }),
            _ => None,
        }
    }

    pub fn int(&self, names: &[&str]) -> Option<i64> {
        match self.get(names)? {
            Value::Int(i) => Some(*i),
            Value::UInt(u) => i64::try_from(*u).ok(),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }

    pub fn bool(&self, names: &[&str]) -> Option<bool> {
        match self.get(names)? {
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            _ => None,
        }
    }

    pub fn string(&self, names: &[&str]) -> Option<String> {
        match self.get(names)? {
            Value::Str(s) => Some(s.clone()),
            Value::Ref(id) => match self.graph.objects.get(id) {
                Some(Obj::Str(s)) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        }
        .filter(|s| !s.is_empty())
    }

    /// List of strings, e.g. from `List<string>` (`_items` array).
    pub fn strings(&self, names: &[&str]) -> Vec<String> {
        let Some(Value::Ref(id)) = self.get(names) else { return Vec::new() };
        let items = match self.graph.objects.get(id) {
            Some(Obj::Array(items)) => items.clone(),
            Some(Obj::Class { members, .. }) => {
                let size = members.iter().find(|(k, _)| k == "_size").and_then(|(_, v)| match v {
                    Value::Int(i) => Some(*i as usize),
                    _ => None,
                });
                match members.iter().find(|(k, _)| k == "_items").map(|(_, v)| v) {
                    Some(Value::Ref(aid)) => match self.graph.objects.get(aid) {
                        Some(Obj::Array(items)) => items.iter().take(size.unwrap_or(usize::MAX)).cloned().collect(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        items
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                Value::Ref(r) => match self.graph.objects.get(r) {
                    Some(Obj::Str(s)) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures were generated with the .NET 8 BinaryFormatter from classes with
    // the same field names as in the original.
    #[test]
    fn rdp_settings() {
        let g = parse(include_bytes!("../../tests/fixtures/rdpsettings.bin")).unwrap();
        let f = g.root_fields().unwrap();
        assert!(f.class_name.ends_with("RdpSettings"));
        assert_eq!(f.int(&["RdpPort", "_rdpPort"]), Some(3390));
        assert_eq!(f.int(&["ColorDepth"]), Some(24));
        assert_eq!(f.bool(&["Clipboard"]), Some(true));
        assert_eq!(f.bool(&["Printer"]), Some(false));
        assert_eq!(f.string(&["WindowResolution"]).as_deref(), Some("1920x1080"));
        assert_eq!(f.string(&["GwServer"]), None);
        assert_eq!(f.string(&["KeyboardLayout"]).as_deref(), Some("00000407"));
        assert_eq!(f.strings(&["LocalDrives"]), vec!["C:".to_string(), "D:".to_string()]);
    }

    #[test]
    fn putty_settings_with_nested_font() {
        let g = parse(include_bytes!("../../tests/fixtures/puttysettings.bin")).unwrap();
        let f = g.root_fields().unwrap();
        assert_eq!(f.int(&["Port"]), Some(2222));
        assert_eq!(f.bool(&["UseOwnCmd"]), Some(true));
        assert_eq!(f.string(&["CommandLine"]).as_deref(), Some("-X"));
        assert_eq!(f.string(&["ProfileName"]).as_deref(), Some("Default Settings"));
    }

    fn header() -> Vec<u8> {
        let mut d = vec![0u8];
        for v in [1i32, -1, 1, 0] {
            d.extend(v.to_le_bytes());
        }
        d
    }

    #[test]
    fn huge_null_counts_are_rejected_quickly() {
        // ArraySinglePrimitive of 2^31-1 "Null" values: zero bytes each, so only a budget stops it.
        let mut d = header();
        d.push(15);
        d.extend(1i32.to_le_bytes());
        d.extend(i32::MAX.to_le_bytes());
        d.push(17);
        let t = std::time::Instant::now();
        assert!(parse(&d).is_err());
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        // ObjectNullMultiple inside an object array.
        let mut d = header();
        d.push(16);
        d.extend(1i32.to_le_bytes());
        d.extend(i32::MAX.to_le_bytes());
        d.push(14);
        d.extend(i32::MAX.to_le_bytes());
        assert!(parse(&d).is_err());
    }

    #[test]
    fn deep_nesting_is_rejected() {
        // Nested object arrays, each containing the next one: would overflow the stack.
        let mut d = header();
        for i in 0..100_000i32 {
            d.push(16);
            d.extend((i + 1).to_le_bytes());
            d.extend(1i32.to_le_bytes());
        }
        assert!(parse(&d).is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse(b"").is_err());
        assert!(parse(&[0, 1, 0, 0]).is_err());
        assert!(parse(b"hello").is_err());
    }
}
