import AppKit
import GitHubCore
import SwiftUI

struct GitHubCommentComposer: View {
    @Bindable var service: GitHubService
    @Binding var draft: String
    @State private var preview = false
    @FocusState private var focused: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            GitHubAvatar(person: service.user ?? .null, size: 32).padding(.top, 7)
            VStack(alignment: .leading, spacing: 0) {
                if preview {
                    GitHubMarkdownView(source: draft).padding(14).frame(minHeight: 106, alignment: .topLeading)
                } else {
                    ZStack(alignment: .topLeading) {
                        if draft.isEmpty {
                            Text(GHL("Leave a comment…")).font(.system(size: 13)).foregroundStyle(.tertiary)
                                .padding(.horizontal, 14).padding(.top, 14).allowsHitTesting(false)
                        }
                        GitHubCommentTextView(text: $draft).frame(height: 112)
                            .accessibilityIdentifier("github.comment.draft")
                    }
                }
                HStack(spacing: 10) {
                    Button { preview.toggle() } label: {
                        Label(preview ? GHL("Write") : GHL("Preview"), systemImage: preview ? "pencil" : "eye")
                    }.buttonStyle(.plain).font(.system(size: 11)).foregroundStyle(.secondary).disabled(draft.isEmpty)
                    Spacer()
                    if let notice = service.commentNotice { Text(notice).font(.caption).foregroundStyle(.green) }
                    Button {
                        let submitted = draft
                        Task {
                            await service.postComment(submitted)
                            if service.commentNotice != nil, service.commentError == nil, draft == submitted { draft = ""; preview = false }
                        }
                    } label: {
                        HStack(spacing: 5) {
                            if service.isPostingComment { ProgressView().controlSize(.mini) }
                            Text(GHL("Comment"))
                        }
                    }
                    .buttonStyle(.borderedProminent).controlSize(.small)
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || service.isPostingComment || service.detail?["locked"].bool == true)
                    .accessibilityIdentifier("github.comment.submit")
                }.padding(.horizontal, 12).padding(.bottom, 12)
                if let error = service.commentError { Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled).padding(.horizontal, 14).padding(.bottom, 12) }
            }
            .frame(maxWidth: .infinity)
            .background(Color.primary.opacity(0.012), in: RoundedRectangle(cornerRadius: 6))
            .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.11)) }
        }
        .padding(.top, 4)
    }
}

private struct GitHubCommentTextView: NSViewRepresentable {
    @Binding var text: String
    func makeCoordinator() -> Coordinator { Coordinator(text: $text) }
    func makeNSView(context: Context) -> NSScrollView {
        let editor = NSTextView()
        editor.isRichText = false
        editor.isAutomaticQuoteSubstitutionEnabled = false
        editor.isAutomaticDashSubstitutionEnabled = false
        editor.isContinuousSpellCheckingEnabled = true
        editor.font = .systemFont(ofSize: 13)
        editor.textColor = .labelColor
        editor.drawsBackground = false
        editor.textContainerInset = NSSize(width: 10, height: 12)
        editor.textContainer?.lineFragmentPadding = 4
        editor.textContainer?.widthTracksTextView = true
        editor.isVerticallyResizable = true
        editor.isHorizontallyResizable = false
        editor.autoresizingMask = [.width]
        editor.delegate = context.coordinator
        editor.setAccessibilityLabel(GHL("Leave a comment"))
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.documentView = editor
        return scroll
    }
    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.text = $text
        guard let editor = scroll.documentView as? NSTextView, editor.string != text else { return }
        editor.string = text
    }
    @MainActor final class Coordinator: NSObject, NSTextViewDelegate {
        var text: Binding<String>
        init(text: Binding<String>) { self.text = text }
        func textDidChange(_ notification: Notification) {
            guard let editor = notification.object as? NSTextView else { return }
            text.wrappedValue = editor.string
        }
    }
}
