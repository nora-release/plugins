#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Chat,
    Responses,
    Anthropic,
}
impl Protocol {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" => Some(Self::Chat),
            "responses" => Some(Self::Responses),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }
    pub(crate) fn api(self) -> &'static str {
        match self {
            Self::Chat => "openai-completions",
            Self::Responses => "openai-responses",
            Self::Anthropic => "anthropic-messages",
        }
    }
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Chat => "/chat/completions",
            Self::Responses => "/responses",
            Self::Anthropic => "/messages",
        }
    }
    pub(crate) fn agent(self) -> String {
        let version = match self {
            Self::Chat => "4.0.23",
            Self::Responses => "4.0.40",
            Self::Anthropic => "4.0.46",
        };
        format!("opencode/1.19.0 ai-sdk/provider-utils/{version} runtime/bun/1.3.14")
    }
}
