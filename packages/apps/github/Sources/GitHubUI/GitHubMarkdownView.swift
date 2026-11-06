import SwiftUI

struct GitHubMarkdownView: View {
    let source: String
    private let document: [GitHubMarkdownBlock]

    init(source: String) {
        self.source = source
        document = GitHubMarkdownDocument.parse(source)
    }

    var body: some View {
        GitHubMarkdownBlockList(blocks: document, path: "body")
            .id(source)
            .font(.system(size: 14))
            .foregroundStyle(Color.primary.opacity(0.92))
            .lineSpacing(4)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    static func blocks(_ source: String) -> [GitHubMarkdownBlock] {
        GitHubMarkdownDocument.parse(source)
    }

    static func attributedText(_ source: String) -> AttributedString {
        GitHubMarkdownDocument.inline(source)
    }
}

struct GitHubMarkdownInlineText: View {
    let source: String

    var body: some View {
        Text(GitHubMarkdownView.attributedText(source))
            .fixedSize(horizontal: false, vertical: true)
            .textSelection(.enabled)
    }
}
