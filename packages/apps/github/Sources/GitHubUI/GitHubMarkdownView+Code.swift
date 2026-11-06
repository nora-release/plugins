import AppKit
import SwiftUI

@MainActor
struct GitHubMarkdownCodeView: View {
    let language: String?
    let source: String
    let path: String
    @State private var highlighter = GitHubCodeHighlighter(fontSize: 13)

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(verbatim: language.flatMap { $0.isEmpty ? nil : $0 } ?? GHL("Code"))
                    .font(.system(size: 11, weight: .medium, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    Self.copy(source)
                } label: {
                    Image(systemName: "doc.on.doc")
                }
                .buttonStyle(.borderless)
                .help(GHL("Copy code"))
                .accessibilityLabel(GHL("Copy code"))
                .accessibilityIdentifier("github.markdown.code.\(path).copy")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            Rectangle().fill(Color.primary.opacity(0.06)).frame(height: 1)
            ScrollView(.horizontal) {
                Text(Self.highlighted(source, language: language, using: highlighter))
                    .font(.system(size: 13, design: .monospaced))
                    .lineSpacing(3)
                    .fixedSize(horizontal: true, vertical: true)
                    .textSelection(.enabled)
                    .padding(12)
                    .accessibilityIdentifier("github.markdown.code.\(path).text")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.022), in: RoundedRectangle(cornerRadius: 6))
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay {
            RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.06))
        }
    }

    static func highlighted(_ source: String, language: String?, using highlighter: GitHubCodeHighlighter) -> AttributedString {
        let highlighted = highlighter.attributedString(code: source, language: language)
        var result = AttributedString(source)
        highlighted.enumerateAttributes(in: NSRange(location: 0, length: highlighted.length)) { attributes, range, _ in
            guard let range = Range(range, in: result) else { return }
            if let color = attributes[.foregroundColor] as? NSColor {
                result[range].foregroundColor = Color(nsColor: color)
            }
            if let font = attributes[.font] as? NSFont {
                result[range].font = Font(font)
            }
        }
        return result
    }

    static func copy(_ source: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(source, forType: .string)
    }
}
