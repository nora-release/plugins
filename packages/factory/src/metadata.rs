use super::prompt;
const ENV: &str = "You have been invoked in the following environment:";
const GLOBAL: &str = "(user's private global instructions for all projects)";
const COMPACT: &str =
    "This session is being continued from a previous conversation that ran out of context.";
const READ: &str = "Result of calling the Read tool:\n";
fn model(text: &str) -> String {
    text.replacen(
        "You are powered by the model named",
        "Current model name:",
        1,
    )
    .replacen("You are powered by the model", "Current model:", 1)
    .replacen("The exact model ID is", "Model ID:", 1)
    .replacen(
        "Assistant knowledge cutoff is",
        "Model knowledge cutoff:",
        1,
    )
}
fn skill(text: &str) -> String {
    text.split('\n').map(|line|if line.starts_with("- update-config: ")||line.starts_with("- /update-config: "){line.replace("the harness executes these, not Claude, so memory/preferences cannot fulfill them.","the harness executes these, not the assistant, so memory/preferences cannot fulfill them.")}else{line.into()}).collect::<Vec<_>>().join("\n")
}
fn env(text: &str) -> bool {
    let Some(rest) = text
        .strip_prefix("# Environment\n")
        .and_then(|s| s.strip_prefix(ENV))
    else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    let Some(rest) = rest.strip_prefix('\n') else {
        return false;
    };
    let mut lines = rest.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    if !(first.starts_with(" - ") || first.starts_with("  - ")) {
        return false;
    }
    for line in lines {
        if line.is_empty() {
            return true;
        }
        if !(line.starts_with(" - ") || line.starts_with("  - ")) {
            return false;
        }
    }
    true
}
fn tokens(text: &str) -> bool {
    text.split("\n\n").any(|p| {
        p.strip_prefix("<total_tokens>")
            .and_then(|p| p.strip_suffix(" tokens left</total_tokens>"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}
pub(crate) fn system(text: &str) -> String {
    let mut out = text.to_owned();
    if env(text) {
        out = out
            .replacen("# Environment", "# Runtime context", 1)
            .replacen(ENV, "The session environment is:", 1);
    } else if !(tokens(text)
        && (text.starts_with("You are powered by the model ")
            || text.starts_with("# Environment update\n")))
    {
        return skill(text);
    }
    let paragraphs = out
        .split("\n\n")
        .map(|p| {
            if p.starts_with("You are powered by the model ") {
                model(p)
            } else {
                p.into()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    skill(&paragraphs)
}
pub(crate) fn prompt(text: &str) -> String {
    skill(
        &text
            .split('\n')
            .map(|line| {
                if line.starts_with("You are powered by the model ") && !line.contains(['<', '>']) {
                    model(line)
                } else {
                    line.into()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
pub(crate) fn user(text: &str) -> String {
    // These are complete generated attachments, never arbitrary occurrences in
    // the user's own prose or data printed by a hook.
    let header =
        format!("{COMPACT} The summary below covers the earlier portion of the conversation.");
    if let Some(content) = text.strip_prefix(&header) {
        let quoted = prompt::quoted(content);
        let quoted=quoted.strip_prefix(prompt::QUOTED).map(|body|format!("Conversation context encoded as a JSON string. Decode the JSON string to recover the exact original context before continuing:\n{body}")).unwrap_or(quoted);
        return format!("Earlier conversation context is summarized below. The summary below covers the earlier portion of the conversation.{quoted}");
    }
    if let Some(content) = text.strip_prefix(READ) {
        return format!("{READ}{}", prompt::quoted(content));
    }
    let Some(inner) = text
        .strip_prefix("<system-reminder>\n")
        .and_then(|s| s.strip_suffix("\n</system-reminder>"))
    else {
        return system(text);
    };
    let out=if env(inner){system(inner)}else if inner.starts_with("You are powered by the model ")&&!inner.contains('\n')&&!inner.contains(['<','>'])&&inner.ends_with('.') {model(inner)}else if inner.starts_with("Codebase and user instructions are shown below")||inner.starts_with("These instruction files changed — each replaces its earlier copy, and the instructions preamble above still applies:\nContents of ")||inner.starts_with("Instruction files were re-read ")&&inner.contains("; these differ from their earlier copies — each replaces its earlier copy, and the instructions preamble above still applies:\nContents of "){inner.replace(GLOBAL,"(global instructions)")}else if inner.starts_with("The following skills are available for use with the Skill tool:\n\n") {skill(inner)}else if let Some(content)=inner.strip_prefix(READ){format!("{READ}{}",prompt::quoted(content))}else{return text.into();};
    format!("<system-reminder>\n{out}\n</system-reminder>")
}
#[cfg(test)]
#[path = "../test/metadata.test.rs"]
mod tests;
