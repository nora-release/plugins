import GitHubCore
import SwiftUI

struct GitHubMarkdownView: View {
    let source: String
    private let document: [GitHubMarkdownBlock]
    private let rendered: [GitHubImageSources.Rendered]

    /// `html` is the body as GitHub rendered it, whose pictures GitHub serves itself.
    init(source: String, html: String = "") {
        self.source = source
        let parsed = GitHubMarkdownCache.shared.body(source, html: html)
        document = parsed.document
        rendered = parsed.rendered
    }

    var body: some View {
        GitHubMarkdownBlockList(blocks: document, path: "body")
            .environment(\.githubRenderedImages, rendered)
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
        GitHubMarkdownCache.shared.inline(source)
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

/// Parsed bodies and inline text, kept: a view is made again each time its parent redraws, and parsing a
/// long discussion on every redraw, while its sections arrive one by one, stalls the panel.
@MainActor
final class GitHubMarkdownCache {
    static let shared = GitHubMarkdownCache()
    private static let limit = 300

    struct Body {
        let document: [GitHubMarkdownBlock]
        let rendered: [GitHubImageSources.Rendered]
    }

    private struct Key: Hashable {
        let source: String
        let html: String
    }

    private var bodies: [Key: Body] = [:]
    private var bodyOrder: [Key] = []
    private var inlines: [String: AttributedString] = [:]
    private var inlineOrder: [String] = []

    func body(_ source: String, html: String) -> Body {
        let key = Key(source: source, html: html)
        if let body = bodies[key] { return body }
        let body = Body(document: GitHubMarkdownDocument.parse(source), rendered: GitHubImageSources.rendered(html))
        Self.keep(body, for: key, in: &bodies, order: &bodyOrder)
        return body
    }

    func inline(_ source: String) -> AttributedString {
        if let text = inlines[source] { return text }
        let text = GitHubMarkdownDocument.inline(source)
        Self.keep(text, for: source, in: &inlines, order: &inlineOrder)
        return text
    }

    private static func keep<K: Hashable, V>(_ value: V, for key: K, in values: inout [K: V], order: inout [K]) {
        values[key] = value
        order.append(key)
        if order.count > limit {
            for old in order.prefix(order.count - limit) { values[old] = nil }
            order.removeFirst(order.count - limit)
        }
    }
}
