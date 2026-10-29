mod protocol;
pub mod regex;
pub mod text;
pub mod value;
pub use protocol::serve;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Context {
    pub protocol: String,
    pub model: String,
    pub path: String,
    pub agent: String,
    pub stream: bool,
    pub status: u16,
    pub options: Value,
    pub state: Value,
    pub provider: Value,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Keep,
    Replace,
    Drop,
    Reject { status: u16, message: String },
}
pub trait Middleware: Default + Send + Sync + 'static {
    const NAME: &'static str;
    fn apply(&self, _phase: &str, _body: &mut Value, _ctx: &mut Context) -> Result<Action, String> {
        Ok(Action::Keep)
    }
    fn order(
        &self,
        _name: &str,
        _pool: &str,
        seats: &[Value],
        _options: &Value,
    ) -> Result<Vec<usize>, String> {
        Ok((0..seats.len()).collect())
    }
}
