use serde_json::{json, Value};
const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";
#[derive(Default)]
struct Code {
    span: usize,
    fence: usize,
    mid: bool,
}
impl Code {
    fn read(v: &Value) -> Self {
        Self {
            span: v["span"].as_u64().unwrap_or(0) as usize,
            fence: v["fence"].as_u64().unwrap_or(0) as usize,
            mid: v["mid"] == true,
        }
    }
    fn value(&self) -> Value {
        json!({"span":self.span,"fence":self.fence,"mid":self.mid})
    }
    fn close(&mut self, s: &str, last: bool) -> (Option<usize>, usize) {
        let mut j = 0;
        while j < s.len() {
            let c = s[j..].chars().next().unwrap();
            if c == '`' {
                let n = s[j..].bytes().take_while(|c| *c == b'`').count();
                let k = j + n;
                if k == s.len() && !last {
                    return (None, s.len() - j);
                }
                if self.fence > 0 {
                    if !self.mid && n >= self.fence {
                        self.fence = 0;
                    }
                } else if self.span > 0 {
                    if n == self.span {
                        self.span = 0;
                    }
                } else if !self.mid && n >= 3 {
                    self.fence = n;
                } else {
                    self.span = n;
                }
                self.mid = true;
                j = k;
            } else if c == '\n' {
                self.span = 0;
                self.mid = false;
                j += 1;
            } else if c == ' ' || c == '\t' {
                j += 1;
            } else if c == '<' && self.span == 0 && self.fence == 0 {
                if s[j..].starts_with(CLOSE) {
                    return (Some(j), 0);
                }
                if !last && CLOSE.starts_with(&s[j..]) {
                    return (None, s.len() - j);
                }
                self.mid = true;
                j += 1;
            } else {
                self.mid = true;
                j += c.len_utf8();
            }
        }
        (None, 0)
    }
}
fn whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
pub fn whole(text: &str) -> String {
    let Some(rest) = text.trim_start_matches(whitespace).strip_prefix(OPEN) else {
        return text.into();
    };
    let (Some(at), _) = Code::default().close(rest, true) else {
        return text.into();
    };
    rest[at + CLOSE.len()..]
        .trim_start_matches(whitespace)
        .into()
}
pub fn piece(state: &mut Value, key: &str, piece: &str) -> String {
    let st = &state[key];
    if st["begun"] == true {
        return piece.into();
    }
    let mut code = Code::read(&st["code"]);
    let mut inside = st["inside"] == true;
    let mut trim = st["trim"] == true;
    let mut begun = false;
    let mut s = format!("{}{piece}", st["carry"].as_str().unwrap_or(""));
    let mut carry = String::new();
    let out = loop {
        if inside {
            let (at, hold) = code.close(&s, false);
            if let Some(at) = at {
                s = s[at + CLOSE.len()..].into();
                inside = false;
                trim = true;
                continue;
            }
            if hold > 0 {
                carry = s[s.len() - hold..].into();
            }
            break String::new();
        }
        if trim {
            s = s.trim_start_matches(whitespace).into();
            if s.is_empty() {
                break s;
            }
            trim = false;
            begun = true;
            break s;
        }
        let rest = s.trim_start_matches(whitespace);
        if let Some(remaining) = rest.strip_prefix(OPEN) {
            s = remaining.into();
            inside = true;
            continue;
        }
        if rest.len() < OPEN.len() && OPEN.starts_with(rest) {
            carry = s;
            break String::new();
        }
        begun = true;
        break s;
    };
    state[key] =
        json!({"inside":inside,"carry":carry,"begun":begun,"trim":trim,"code":code.value()});
    out
}
