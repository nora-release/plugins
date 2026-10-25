use super::{number, paths};
use serde_json::Value;

struct Number<'a> {
    scalar: f64,
    values: Option<Vec<Option<f64>>>,
    items: Option<Vec<Option<&'a Value>>>,
}
impl Number<'_> {
    fn scalar(scalar: f64) -> Self {
        Self {
            scalar,
            values: None,
            items: None,
        }
    }
}

struct Parser<'a> {
    reply: &'a Value,
    source: &'a str,
    position: usize,
    depth: usize,
}

pub(super) fn evaluate(reply: &Value, source: &str) -> Result<f64, String> {
    let mut parser = Parser {
        reply,
        source,
        position: 0,
        depth: 0,
    };
    let result = parser.sum()?;
    parser.space();
    if parser.position != source.len() {
        return Err("The balance expression contains unexpected text.".into());
    }
    if result.values.is_some() {
        return Err(
            "The balance path has several values; put them in sum(), max(), min() or count()."
                .into(),
        );
    }
    Ok(result.scalar)
}

impl<'a> Parser<'a> {
    fn space(&mut self) {
        while self.peek().is_some_and(u8::is_ascii_whitespace) {
            self.position += 1;
        }
    }
    fn peek(&self) -> Option<&u8> {
        self.source.as_bytes().get(self.position)
    }

    fn sum(&mut self) -> Result<Number<'a>, String> {
        let mut value = self.product()?;
        loop {
            self.space();
            let Some(&operator @ (b'+' | b'-')) = self.peek() else {
                return Ok(value);
            };
            self.position += 1;
            value = operation(value, self.product()?, operator)?;
        }
    }

    fn product(&mut self) -> Result<Number<'a>, String> {
        let mut value = self.unary()?;
        loop {
            self.space();
            let Some(&operator @ (b'*' | b'/')) = self.peek() else {
                return Ok(value);
            };
            self.position += 1;
            value = operation(value, self.unary()?, operator)?;
        }
    }

    fn unary(&mut self) -> Result<Number<'a>, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("The balance expression is nested too deeply.".into());
        }
        let result = self.value();
        self.depth -= 1;
        result
    }

    fn value(&mut self) -> Result<Number<'a>, String> {
        self.space();
        match self.peek().copied() {
            None => {
                return Err(
                    "The balance expression ends where an amount or field is required.".into(),
                )
            }
            Some(b'-') => {
                self.position += 1;
                return operation(Number::scalar(0.0), self.unary()?, b'-');
            }
            Some(b'(') => {
                self.position += 1;
                let value = self.sum()?;
                return self.closed(value);
            }
            Some(ch) if ch.is_ascii_digit() || ch == b'.' => {
                let start = self.position;
                while self
                    .peek()
                    .is_some_and(|ch| ch.is_ascii_digit() || *ch == b'.')
                {
                    self.position += 1;
                }
                let scalar = self.source[start..self.position]
                    .parse::<f64>()
                    .map_err(|_| "The balance expression contains an invalid number.")?;
                return Ok(Number::scalar(scalar));
            }
            _ => {}
        }
        let start = self.position;
        while let Some(&ch) = self.peek() {
            if ch == b'[' {
                let length = self.source[self.position..]
                    .find(']')
                    .ok_or("The balance path has a [ without its ].")?;
                self.position += length + 1;
                continue;
            }
            if ch == b'*'
                && (self.position == start || self.source.as_bytes()[self.position - 1] == b'.')
            {
                self.position += 1;
                continue;
            }
            if b"+-*/() \t\r\n".contains(&ch) {
                break;
            }
            self.position += 1;
        }
        if self.position == start {
            return Err("The balance expression requires an amount or field.".into());
        }
        let path = &self.source[start..self.position];
        if ["sum", "max", "min", "count"].contains(&path) && self.peek() == Some(&b'(') {
            self.position += 1;
            let value = self.sum()?;
            let value = self.closed(value)?;
            return aggregate(path, value);
        }
        let (items, many) = paths::resolve(self.reply, path)?;
        if !many {
            let scalar = items
                .first()
                .copied()
                .flatten()
                .and_then(number)
                .ok_or("The balance field is not a number.")?;
            return Ok(Number::scalar(scalar));
        }
        Ok(Number {
            scalar: 0.0,
            values: Some(items.iter().map(|item| item.and_then(number)).collect()),
            items: Some(items),
        })
    }

    fn closed(&mut self, value: Number<'a>) -> Result<Number<'a>, String> {
        self.space();
        if self.peek() != Some(&b')') {
            return Err("The balance expression has a ( without its ).".into());
        }
        self.position += 1;
        Ok(value)
    }
}

fn operation<'a>(left: Number<'a>, right: Number<'a>, operator: u8) -> Result<Number<'a>, String> {
    let one = |left: f64, right: f64| match operator {
        b'+' => Some(left + right),
        b'-' => Some(left - right),
        b'*' => Some(left * right),
        _ => (right != 0.0).then_some(left / right),
    };
    if left.values.is_none() && right.values.is_none() {
        return one(left.scalar, right.scalar)
            .map(Number::scalar)
            .ok_or_else(|| "The balance expression divides by 0.".into());
    }
    if matches!((&left.values, &right.values), (Some(left), Some(right)) if left.len()!=right.len())
    {
        return Err("The balance expression combines lists of different lengths.".into());
    }
    let count = left
        .values
        .as_ref()
        .map(Vec::len)
        .or_else(|| right.values.as_ref().map(Vec::len))
        .unwrap_or_default();
    let values = (0..count)
        .map(|index| {
            let a = left
                .values
                .as_ref()
                .map(|values| values[index])
                .unwrap_or(Some(left.scalar))?;
            let b = right
                .values
                .as_ref()
                .map(|values| values[index])
                .unwrap_or(Some(right.scalar))?;
            one(a, b)
        })
        .collect();
    Ok(Number {
        scalar: 0.0,
        values: Some(values),
        items: None,
    })
}

fn aggregate<'a>(name: &str, value: Number<'a>) -> Result<Number<'a>, String> {
    if name == "count" {
        if let Some(items) = value.items {
            return Ok(Number::scalar(
                items.iter().filter(|item| item.is_some()).count() as f64,
            ));
        }
    }
    let numbers = value
        .values
        .unwrap_or_else(|| vec![Some(value.scalar)])
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let scalar = match name {
        "sum" => numbers.iter().fold(0.0, |total, value| total + value),
        "count" => numbers.len() as f64,
        "max" => numbers
            .into_iter()
            .reduce(f64::max)
            .ok_or("max() of the balance path has nothing to take it of.")?,
        "min" => numbers
            .into_iter()
            .reduce(f64::min)
            .ok_or("min() of the balance path has nothing to take it of.")?,
        _ => return Err("The balance expression has an unsupported function.".into()),
    };
    Ok(Number::scalar(scalar))
}
