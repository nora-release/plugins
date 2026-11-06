use nora_gateway_middleware::{
    value::{number, truthy},
    Middleware,
};
use serde_json::Value;
use std::cmp::Ordering;
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "reserve";
    fn order(
        &self,
        name: &str,
        _pool: &str,
        seats: &[Value],
        options: &Value,
    ) -> Result<Vec<usize>, String> {
        if !matches!(name, "ceiling" | "accounts-first") {
            return Err("Unknown reserve strategy".into());
        }
        let c = number(&options["ceiling"])
            .filter(|c| c.is_finite() && *c > 0. && *c <= 100.)
            .unwrap_or(80.);
        let over = |s: &Value| truthy(&s["known"]) && n(s, "used") >= c;
        let tier = |s: &Value| {
            if truthy(&s["resting"]) {
                2
            } else if name == "ceiling" {
                if over(s) {
                    1
                } else {
                    0
                }
            } else if truthy(&s["account"]) {
                0
            } else {
                1
            }
        };
        let mut order: Vec<_> = (0..seats.len()).collect();
        order.sort_by(|a, b| {
            let (x, y) = (&seats[*a], &seats[*b]);
            tier(x)
                .cmp(&tier(y))
                .then_with(|| {
                    if name == "ceiling" {
                        if over(x) && over(y) {
                            cmp(n(x, "used"), n(y, "used"))
                        } else {
                            Ordering::Equal
                        }
                    } else if truthy(&x["account"]) && truthy(&y["account"]) {
                        cmp(
                            if truthy(&x["known"]) {
                                n(x, "used")
                            } else {
                                0.
                            },
                            if truthy(&y["known"]) {
                                n(y, "used")
                            } else {
                                0.
                            },
                        )
                    } else if !truthy(&x["account"]) && !truthy(&y["account"]) {
                        cmp(weight(y), weight(x)).then_with(|| cmp(n(x, "tokens"), n(y, "tokens")))
                    } else {
                        Ordering::Equal
                    }
                })
                .then_with(|| a.cmp(b))
        });
        Ok(order)
    }
}
fn n(s: &Value, k: &str) -> f64 {
    s[k].as_f64().unwrap_or(0.)
}
fn weight(s: &Value) -> f64 {
    let w = n(s, "weight");
    if w == 0. {
        1.
    } else {
        w
    }
}
fn cmp(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}
