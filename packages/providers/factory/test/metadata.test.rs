use super::*;
#[test]
fn complete_generated_environment_is_adapted_but_pasted_fragment_is_preserved() {
    let text="<system-reminder>\n# Environment\nYou have been invoked in the following environment:\n - cwd: /workspace\n</system-reminder>";
    let output = user(text);
    assert!(output.contains("# Runtime context\nThe session environment is:"));
    assert!(output.contains("/workspace"));
    let fragment = format!("Here is a quoted reminder: {text}\nActual user question");
    assert_eq!(user(&fragment), fragment);
}
#[test]
fn generated_global_instruction_wrapper_keeps_all_instructions() {
    let text="<system-reminder>\nCodebase and user instructions are shown below:\nContents of /home/CLAUDE.md (user's private global instructions for all projects):\nNever discard this instruction.\n</system-reminder>";
    let output = user(text);
    assert!(output.contains("(global instructions)"));
    assert!(output.contains("Never discard this instruction."));
    assert_eq!(
        user("quoted (user's private global instructions for all projects)"),
        "quoted (user's private global instructions for all projects)"
    );
}
#[test]
fn prompt_model_metadata_keeps_task_text_and_newline() {
    let text = "You are powered by the model named Claude.\nKeep this task exactly.\n";
    assert_eq!(
        prompt(text),
        "Current model name: Claude.\nKeep this task exactly.\n"
    );
}
